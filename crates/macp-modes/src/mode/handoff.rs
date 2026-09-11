use crate::mode::util::{
    check_commitment_authority, enforce_commitment_policy, is_declared_participant,
    validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::handoff_pb::{
    HandoffAcceptPayload, HandoffContextPayload, HandoffDeclinePayload, HandoffOfferPayload,
};
use macp_pb::pb::Envelope;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HandoffDisposition {
    Offered,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffOfferRecord {
    pub handoff_id: String,
    pub target_participant: String,
    pub scope: String,
    pub reason: String,
    pub offered_by: String,
    pub disposition: HandoffDisposition,
    pub accepted_by: Option<String>,
    pub declined_by: Option<String>,
    pub outcome_reason: Option<String>,
    #[serde(default)]
    pub offered_at_ms: i64,
    /// `Session::accumulated_suspended_ms` as it stood when the offer was
    /// recorded — a snapshot, never a live read.
    ///
    /// RFC-MACP-0010 §5.1(1): time the session spends `Suspended` must not
    /// count toward the implicit-accept deadline, and the suspension accrued
    /// *since this offer* is `session.accumulated_suspended_ms` minus this
    /// value. Both terms are on the recorded timeline (suspend/resume replay
    /// from the log), so the subtraction is replay-deterministic.
    ///
    /// Legacy `mode_state` recorded before this field existed deserializes as
    /// `0` (serde default), which is also the value for an offer made on a
    /// session that had never been suspended.
    #[serde(default)]
    pub suspended_ms_at_offer: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffContextRecord {
    pub content_type: String,
    pub context: Vec<u8>,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffState {
    pub offers: BTreeMap<String, HandoffOfferRecord>,
    pub contexts: BTreeMap<String, Vec<HandoffContextRecord>>,
}

pub struct HandoffMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl HandoffMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn encode_state(state: &HandoffState) -> Vec<u8> {
        crate::mode::util::encode_mode_state(state)
    }

    fn decode_state(data: &[u8]) -> Result<HandoffState, MacpError> {
        crate::mode::util::decode_mode_state(data)
    }

    /// Elapsed time an outstanding offer's `implicit_accept_timeout_ms` is
    /// measured against, selected by the session's semantics revision.
    ///
    /// Rev <= 1 keeps the raw difference verbatim, so legacy histories replay
    /// to the outcome they were accepted with. Rev >= 2 has its own branch
    /// because the suspension-corrected deadline of RFC-MACP-0010 §5.1(1)
    /// lands there; see [`Self::rev2_elapsed_ms`], which is deliberately
    /// identical to the legacy arithmetic in this revision of the code.
    fn implicit_accept_elapsed_ms(
        session: &Session,
        offer: &HandoffOfferRecord,
        now_ms: i64,
    ) -> i64 {
        if session.semantics_rev >= 2 {
            Self::rev2_elapsed_ms(offer, now_ms)
        } else {
            now_ms - offer.offered_at_ms
        }
    }

    /// The rev >= 2 elapsed computation.
    ///
    /// Today this is the raw difference — byte-for-byte the same outcome as
    /// rev 1 — so introducing the revision changes nothing observable. The
    /// suspension-correction term
    /// (`session.accumulated_suspended_ms - offer.suspended_ms_at_offer`,
    /// RFC-MACP-0010 §5.1(1)) is subtracted here by the change that takes the
    /// revision live; keeping that a separate commit keeps any regression
    /// bisectable.
    fn rev2_elapsed_ms(offer: &HandoffOfferRecord, now_ms: i64) -> i64 {
        now_ms - offer.offered_at_ms
    }

    fn commitment_ready(state: &HandoffState) -> bool {
        state.offers.values().any(|offer| {
            offer.disposition == HandoffDisposition::Accepted
                || offer.disposition == HandoffDisposition::Declined
        })
    }
}

impl Mode for HandoffMode {
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
            "Commitment" => check_commitment_authority(session, &env.sender),
            // HandoffOffer: only initiator can offer
            "HandoffOffer" if env.sender == session.initiator_sender => Ok(()),
            "HandoffOffer" => Err(MacpError::Forbidden),
            // HandoffContext: any declared participant (on_message enforces offerer match)
            _ if is_declared_participant(&session.participants, &env.sender) => Ok(()),
            _ => Err(MacpError::Forbidden),
        }
    }

    fn on_session_start(
        &self,
        session: &Session,
        _env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        // RFC-MACP-0010 §2 (delegated model): the accepted SessionStart
        // sender IS the current responsibility owner, and §3 binds
        // `participants` as "current owner and eligible targets". Both checks
        // below are stricter than the literal §3 text but follow from the
        // model: the owner must be in the list, alongside ≥1 eligible target.
        // (Unlike Task/Decision/Quorum, initiator membership is intrinsic
        // here — the initiator is a transfer party, not just a coordinator.)
        if session.participants.len() < 2 {
            return Err(MacpError::InvalidPayload);
        }
        if !session
            .participants
            .iter()
            .any(|p| p == &session.initiator_sender)
        {
            return Err(MacpError::InvalidPayload);
        }
        Ok(ModeResponse::PersistState(Self::encode_state(
            &HandoffState::default(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        // Legacy clock (semantics rev 0): the client-supplied envelope
        // timestamp. The kernel calls `on_message_at`, which selects the
        // acceptance clock for rev >= 1 sessions; this path remains for
        // legacy-history replay and direct library callers.
        self.handle_message(session, env, env.timestamp_unix_ms)
    }

    fn on_message_at(
        &self,
        session: &Session,
        env: &Envelope,
        ctx: &macp_core::mode::MessageContext,
    ) -> Result<ModeResponse, MacpError> {
        // Rev >= 1: the implicit-accept timeout is measured against the
        // runtime's acceptance clock, which the initiator cannot forge (the
        // envelope timestamp let an initiator post-date a Commitment to
        // finalize an offer the target never accepted). Legacy (rev 0)
        // sessions keep the envelope clock so their histories replay to the
        // same outcome they were accepted with.
        let clock_ms = if session.semantics_rev >= 1 {
            ctx.accepted_at_ms
        } else {
            env.timestamp_unix_ms
        };
        self.handle_message(session, env, clock_ms)
    }
}

impl HandoffMode {
    fn handle_message(
        &self,
        session: &Session,
        env: &Envelope,
        clock_ms: i64,
    ) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            HandoffState::default()
        } else {
            Self::decode_state(&session.mode_state)?
        };

        match env.message_type.as_str() {
            "HandoffOffer" => {
                let payload = HandoffOfferPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0010: At most one offer may be outstanding at any time.
                // Once an offer is accepted, no further offers may be issued.
                if payload.handoff_id.is_empty()
                    || payload.target_participant.is_empty()
                    || state.offers.contains_key(&payload.handoff_id)
                    || !is_declared_participant(&session.participants, &payload.target_participant)
                    || payload.target_participant == env.sender
                    || state
                        .offers
                        .values()
                        .any(|o| o.disposition == HandoffDisposition::Offered)
                    || state
                        .offers
                        .values()
                        .any(|o| o.disposition == HandoffDisposition::Accepted)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.offers.insert(
                    payload.handoff_id.clone(),
                    HandoffOfferRecord {
                        handoff_id: payload.handoff_id,
                        target_participant: payload.target_participant,
                        scope: payload.scope,
                        reason: payload.reason,
                        offered_by: env.sender.clone(),
                        disposition: HandoffDisposition::Offered,
                        accepted_by: None,
                        declined_by: None,
                        outcome_reason: None,
                        // Rev >= 1: record the runtime acceptance clock (the
                        // same value the log entry records, so replay is
                        // identical). The client envelope timestamp is
                        // unvalidated — recording it here let an offering
                        // participant BACK-date the offer and immediately
                        // commit, forging elapsed time past the implicit-
                        // accept timeout (the same attack as post-dating the
                        // commitment, relocated to the offer side).
                        offered_at_ms: if session.semantics_rev >= 1 {
                            clock_ms
                        } else {
                            env.timestamp_unix_ms
                        },
                        // Recorded unconditionally (it is read only under the
                        // rev >= 2 branch of `implicit_accept_elapsed_ms`, so
                        // recording it on every session is behavior-neutral
                        // and keeps the field trustworthy wherever it is
                        // later consulted).
                        suspended_ms_at_offer: session.accumulated_suspended_ms,
                    },
                );
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffContext" => {
                let payload = HandoffContextPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let offer = state
                    .offers
                    .get(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.offered_by != env.sender {
                    return Err(MacpError::Forbidden);
                }
                // RFC-MACP-0010 §2.1: Late context (sent after accept/decline) is
                // permitted as supplementary documentation. No disposition check.
                state
                    .contexts
                    .entry(payload.handoff_id)
                    .or_default()
                    .push(HandoffContextRecord {
                        content_type: payload.content_type,
                        context: payload.context,
                        sender: env.sender.clone(),
                    });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffAccept" => {
                let payload = HandoffAcceptPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0010 §5.1: `implicit` is runtime-synthesized only
                // (see the implicit_accept_timeout_ms handling below, which
                // never constructs this message type). A client-submitted
                // accept setting it MUST be rejected.
                if payload.implicit {
                    return Err(MacpError::InvalidPayload);
                }
                let offer = state
                    .offers
                    .get_mut(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.target_participant != env.sender {
                    return Err(MacpError::Forbidden);
                }
                if !payload.accepted_by.is_empty() && payload.accepted_by != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if offer.disposition != HandoffDisposition::Offered {
                    return Err(MacpError::InvalidPayload);
                }
                offer.disposition = HandoffDisposition::Accepted;
                offer.accepted_by = Some(env.sender.clone());
                offer.outcome_reason = Some(payload.reason);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffDecline" => {
                let payload = HandoffDeclinePayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let offer = state
                    .offers
                    .get_mut(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.target_participant != env.sender {
                    return Err(MacpError::Forbidden);
                }
                if !payload.declined_by.is_empty() && payload.declined_by != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if offer.disposition != HandoffDisposition::Offered {
                    return Err(MacpError::InvalidPayload);
                }
                offer.disposition = HandoffDisposition::Declined;
                offer.declined_by = Some(env.sender.clone());
                offer.outcome_reason = Some(payload.reason);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                let commitment = validate_commitment_payload_for_session(session, &env.payload)?;
                // RFC-MACP-0012: lazy implicit_accept_timeout_ms check
                if let Some(ref policy) = session.policy_definition {
                    let rules: macp_core::policy::rules::HandoffPolicyRules =
                        serde_json::from_value(policy.rules.clone()).unwrap_or_default();
                    if rules.acceptance.implicit_accept_timeout_ms > 0 {
                        // Clock selected by `on_message_at` per the session's
                        // semantics revision: acceptance time (rev >= 1) or the
                        // legacy envelope timestamp (rev 0). Both are
                        // log-recorded, so replay is deterministic either way.
                        let now_ms = clock_ms;
                        let timeout = rules.acceptance.implicit_accept_timeout_ms as i64;
                        for offer in state.offers.values_mut() {
                            if offer.disposition == HandoffDisposition::Offered
                                && offer.offered_at_ms > 0
                                && Self::implicit_accept_elapsed_ms(session, offer, now_ms)
                                    >= timeout
                            {
                                offer.disposition = HandoffDisposition::Accepted;
                                offer.accepted_by = Some(offer.target_participant.clone());
                                offer.outcome_reason = Some("implicit accept (timeout)".into());
                            }
                        }
                    }
                }
                if !Self::commitment_ready(&state) {
                    return Err(MacpError::InvalidPayload);
                }
                // Governance policy gate (shared): fail closed, only
                // an explicit Allow proceeds.
                enforce_commitment_policy(
                    session,
                    macp_core::policy::CommitmentMode::Handoff,
                    commitment.outcome_positive,
                    &*self.evaluator,
                )?;
                Ok(ModeResponse::PersistAndResolve {
                    state: Self::encode_state(&state),
                    resolution: env.payload.clone(),
                })
            }
            _ => Err(MacpError::InvalidPayload),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::session::Session;
    use macp_pb::pb::CommitmentPayload;

    fn base_session() -> Session {
        Session::builder("s1", "macp.mode.handoff.v1", "owner")
            .ttl_ms(60_000)
            .participants(vec!["owner".into(), "target".into()])
            .mode_version("1.0.0")
            .configuration_version("config")
            .policy_version("policy")
            .build()
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.handoff.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }

    fn commitment_payload() -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "accepted".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec()
    }

    fn apply(session: &mut Session, result: ModeResponse) {
        match result {
            ModeResponse::PersistState(data) => session.mode_state = data,
            ModeResponse::PersistAndResolve { state, .. } => session.mode_state = state,
            _ => {}
        }
    }

    fn make_offer(handoff_id: &str, target: &str) -> Vec<u8> {
        HandoffOfferPayload {
            handoff_id: handoff_id.into(),
            target_participant: target.into(),
            scope: "support".into(),
            reason: "escalate".into(),
        }
        .encode_to_vec()
    }

    fn make_context(handoff_id: &str) -> Vec<u8> {
        HandoffContextPayload {
            handoff_id: handoff_id.into(),
            content_type: "text/plain".into(),
            context: b"background info".to_vec(),
        }
        .encode_to_vec()
    }

    fn make_accept(handoff_id: &str, accepted_by: &str) -> Vec<u8> {
        HandoffAcceptPayload {
            handoff_id: handoff_id.into(),
            accepted_by: accepted_by.into(),
            reason: "ready".into(),
            implicit: false,
        }
        .encode_to_vec()
    }

    fn make_decline(handoff_id: &str, declined_by: &str) -> Vec<u8> {
        HandoffDeclinePayload {
            handoff_id: handoff_id.into(),
            declined_by: declined_by.into(),
            reason: "busy".into(),
        }
        .encode_to_vec()
    }

    // --- Session Start ---

    #[test]
    fn session_start_initializes_state() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert!(state.offers.is_empty());
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn session_start_requires_two_participants() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into()]; // only 1
        let err = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn session_start_rejects_when_initiator_not_participant() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["target".into(), "other".into()]; // owner not included
        let err = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- HandoffOffer ---

    #[test]
    fn offer_creates_entry() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert!(state.offers.contains_key("h1"));
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Offered);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn duplicate_offer_id_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn offer_to_self_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "owner")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn offer_to_non_participant_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "outsider")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- HandoffContext ---

    #[test]
    fn context_for_existing_offer() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffContext", make_context("h1")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.contexts["h1"].len(), 1);
                assert_eq!(state.contexts["h1"][0].content_type, "text/plain");
                assert_eq!(state.contexts["h1"][0].sender, "owner");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn context_from_non_offerer_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("target", "HandoffContext", make_context("h1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- HandoffAccept / HandoffDecline ---

    #[test]
    fn target_can_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Accepted);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn client_submitted_implicit_accept_is_rejected() {
        // RFC-MACP-0010 §5.1: `implicit` is runtime-synthesized only; a
        // client MUST NOT submit it, and the runtime MUST reject it.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let forged_accept = HandoffAcceptPayload {
            handoff_id: "h1".into(),
            accepted_by: "target".into(),
            reason: "ready".into(),
            implicit: true,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("target", "HandoffAccept", forged_accept))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn wrong_target_cannot_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffAccept", make_accept("h1", "owner")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn target_can_decline() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Declined);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn cannot_accept_already_accepted() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Commitment ---

    #[test]
    fn commitment_after_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_after_decline() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_without_response_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn commitment_with_no_offers_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Full lifecycle ---

    #[test]
    fn full_handoff_lifecycle() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffContext", make_context("h1")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Serial offer enforcement ---

    #[test]
    fn second_offer_while_first_pending_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn second_offer_after_first_accepted_is_rejected() {
        // RFC-MACP-0010: "Once an offer is accepted, no further offers may be issued."
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn second_offer_after_first_declined_succeeds() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        mode.on_message(
            &session,
            &env("owner", "HandoffOffer", make_offer("h2", "other")),
        )
        .unwrap();
    }

    // --- Commitment version mismatch ---

    #[test]
    fn commitment_version_mismatch_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let bad_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "accepted".into(),
            mode_version: "wrong".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("owner", "Commitment", bad_commitment))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Unknown message type ---

    #[test]
    fn unknown_message_type_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "CustomType", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn context_after_accept_is_permitted() {
        // RFC-MACP-0010 §2.1: Late context after accept/decline is permitted
        // as supplementary documentation.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Late context after accept should succeed
        let result = mode.on_message(
            &session,
            &env("owner", "HandoffContext", make_context("h1")),
        );
        assert!(
            result.is_ok(),
            "late HandoffContext should be permitted per RFC"
        );
    }

    // --- Policy ---

    #[test]
    fn handoff_policy_evaluator_always_allows() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-handoff".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "handoff policy".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 0 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        // Handoff policy evaluator always allows — commitment should succeed
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Second HandoffOffer while first pending ---

    #[test]
    fn second_offer_to_different_target_while_first_pending_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "targetA".into(), "targetB".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // First offer to targetA — succeeds
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "targetA")),
            )
            .unwrap();
        apply(&mut session, result);
        // Second offer to targetB while h1 is still pending — rejected
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "targetB")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- After HandoffAccept, further offers are allowed (prior resolved) ---

    #[test]
    fn offer_after_accept_blocked_per_rfc() {
        // RFC-MACP-0010: "Once an offer is accepted, no further offers may be issued
        // for the Session. Only one final Commitment may resolve the Session."
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        // New HandoffOffer MUST be rejected after an offer has been accepted
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(state.offers.len(), 1);
        assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Accepted);
    }

    #[test]
    fn offered_at_ms_is_populated() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert!(
            state.offers["h1"].offered_at_ms > 0,
            "offered_at_ms should be set"
        );
    }

    #[test]
    fn implicit_accept_timeout_fires() {
        // RFC-MACP-0010: when implicit_accept_timeout_ms policy is set and
        // sufficient time has elapsed, the offer is auto-accepted at commitment.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "auto-accept".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "short timeout".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 100 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // Offer with a specific timestamp
        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);
        // Commitment with timestamp past the timeout (offer_time + 100ms = 1100)
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 200; // well past 100ms timeout
        let commit = mode.on_message(&session, &commit_env).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    fn auto_accept_policy() -> macp_core::policy::PolicyDefinition {
        macp_core::policy::PolicyDefinition {
            policy_id: "auto-accept".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "short timeout".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 100 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        }
    }

    /// Semantics rev >= 1: the implicit-accept timeout is measured against the
    /// runtime acceptance clock, so an initiator post-dating the Commitment
    /// envelope can no longer finalize an offer the target never accepted.
    #[test]
    fn implicit_accept_ignores_forged_envelope_timestamp_on_rev1() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        assert!(session.semantics_rev >= 1, "builder default is current rev");
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);

        // Initiator forges a far-future envelope timestamp, but the runtime's
        // acceptance clock says only 50ms elapsed: no implicit accept, and the
        // commitment is not ready (no accepted offer) -> rejected.
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 1_000_000;
        let ctx = macp_core::mode::MessageContext::new(offer_time + 50);
        let err = mode.on_message_at(&session, &commit_env, &ctx).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // With genuine elapsed acceptance time past the timeout, it fires.
        let ctx = macp_core::mode::MessageContext::new(offer_time + 200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// Legacy sessions (rev 0) keep the envelope-timestamp clock through the
    /// kernel entry point, so pre-fix histories replay to the outcome they
    /// were accepted with.
    #[test]
    fn implicit_accept_legacy_rev0_keeps_envelope_clock() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.semantics_rev = 0;
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);

        // Legacy semantics: the envelope timestamp drives the timeout even
        // when the acceptance clock disagrees (as it did before the fix).
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 200;
        let ctx = macp_core::mode::MessageContext::new(offer_time + 10);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// The offer-side twin of the forged-commitment test: on rev >= 1 the
    /// offer time is the runtime acceptance clock, so BACK-dating the
    /// HandoffOffer envelope no longer forges elapsed time past the
    /// implicit-accept timeout.
    #[test]
    fn implicit_accept_ignores_backdated_offer_timestamp_on_rev1() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        assert!(session.semantics_rev >= 1);
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        // Offer envelope BACK-dated far into the past, but accepted "now".
        let now = 1_000_000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = now - 1_000_000; // forged past
        let ctx = macp_core::mode::MessageContext::new(now);
        let result = mode.on_message_at(&session, &offer_env, &ctx).unwrap();
        apply(&mut session, result);

        // Commitment accepted 50ms later: elapsed (per the acceptance clock)
        // is 50ms < 100ms timeout — no implicit accept, commitment rejected.
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = now + 50;
        let ctx = macp_core::mode::MessageContext::new(now + 50);
        let err = mode.on_message_at(&session, &commit_env, &ctx).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // With genuinely elapsed acceptance time, it fires.
        let ctx = macp_core::mode::MessageContext::new(now + 200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// The offer snapshots the session's cumulative suspension so the rev >= 2
    /// deadline can later subtract only the suspension accrued *after* the
    /// offer. Snapshot, not a live read.
    #[test]
    fn offer_records_suspension_snapshot() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // The session was already suspended once before the offer landed.
        session.accumulated_suspended_ms = 5_000;
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let ctx = macp_core::mode::MessageContext::new(1_000);
        let result = mode
            .on_message_at(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
                &ctx,
            )
            .unwrap();
        apply(&mut session, result);

        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(state.offers["h1"].suspended_ms_at_offer, 5_000);
    }

    /// Legacy-state fixture: `mode_state` written before the snapshot field
    /// existed must still decode (serde default) and must still reach the
    /// outcome it was accepted with — the implicit-accept timeout fires off
    /// the recorded `offered_at_ms` exactly as before.
    #[test]
    fn legacy_offer_mode_state_without_suspension_snapshot_replays_unchanged() {
        let legacy_state = serde_json::json!({
            "offers": {
                "h1": {
                    "handoff_id": "h1",
                    "target_participant": "target",
                    "scope": "support",
                    "reason": "escalate",
                    "offered_by": "owner",
                    "disposition": "Offered",
                    "accepted_by": null,
                    "declined_by": null,
                    "outcome_reason": null,
                    "offered_at_ms": 1000
                }
            },
            "contexts": {}
        });
        let decoded: HandoffState = serde_json::from_value(legacy_state.clone()).unwrap();
        assert_eq!(decoded.offers["h1"].suspended_ms_at_offer, 0);
        assert_eq!(decoded.offers["h1"].offered_at_ms, 1000);

        // And it still drives the original outcome: 200ms of elapsed
        // acceptance time past a 100ms timeout implicitly accepts, so the
        // commitment resolves.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(auto_accept_policy());
        session.mode_state = serde_json::to_vec(&legacy_state).unwrap();
        let commit_env = env("owner", "Commitment", commitment_payload());
        let ctx = macp_core::mode::MessageContext::new(1_200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        apply(&mut session, commit);
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(
            state.offers["h1"].outcome_reason.as_deref(),
            Some("implicit accept (timeout)")
        );
    }

    /// Revision 2 is behavior-neutral scaffolding: with identical inputs —
    /// including a suspension accrued after the offer, the case the
    /// suspension-corrected deadline will eventually change — rev 1 and rev 2
    /// reach the same outcome. When the correction goes live this test is the
    /// canary: its suspended half is expected to diverge then, and only then.
    #[test]
    fn rev2_implicit_accept_is_identical_to_rev1_today() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let offer_time = 1_000i64;

        // (rev, suspended-after-offer ms) -> outcome
        let outcome = |rev: u32, suspended_after_offer_ms: i64, commit_at: i64| {
            let mut session = base_session();
            session.semantics_rev = rev;
            session.policy_definition = Some(auto_accept_policy());
            let result = mode
                .on_session_start(&session, &env("owner", "SessionStart", vec![]))
                .unwrap();
            apply(&mut session, result);

            let offer_ctx = macp_core::mode::MessageContext::new(offer_time);
            let result = mode
                .on_message_at(
                    &session,
                    &env("owner", "HandoffOffer", make_offer("h1", "target")),
                    &offer_ctx,
                )
                .unwrap();
            apply(&mut session, result);
            // The pause happens between the offer and the commitment.
            session.accumulated_suspended_ms += suspended_after_offer_ms;

            let commit_env = env("owner", "Commitment", commitment_payload());
            let ctx = macp_core::mode::MessageContext::new(commit_at);
            mode.on_message_at(&session, &commit_env, &ctx)
                .map(|r| matches!(r, ModeResponse::PersistAndResolve { .. }))
                .map_err(|e| e.to_string())
        };

        for (suspended, commit_at) in [
            (0i64, offer_time + 200), // fires under both
            (0, offer_time + 50),     // fires under neither
            (500, offer_time + 200),  // the case the correction will change
            (500, offer_time + 50),
        ] {
            assert_eq!(
                outcome(1, suspended, commit_at),
                outcome(2, suspended, commit_at),
                "rev 1 and rev 2 must agree (suspended={suspended}, commit_at={commit_at})"
            );
        }
    }

    /// The scaffolding is wired to the live constant, not to a hard-coded 2:
    /// a session built today is on the current revision and takes the rev >= 2
    /// branch.
    #[test]
    fn builder_default_session_is_on_current_semantics_rev() {
        assert_eq!(
            base_session().semantics_rev,
            macp_core::session::CURRENT_SEMANTICS_REV
        );
        // Compile-time: the rev >= 2 branch is reachable for new sessions at
        // all only while the constant stays there.
        const _: () = assert!(macp_core::session::CURRENT_SEMANTICS_REV >= 2);
    }
}
