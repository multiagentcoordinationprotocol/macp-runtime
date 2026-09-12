use crate::mode::util::{
    check_commitment_authority, enforce_commitment_policy, is_declared_participant,
    validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;
use macp_pb::quorum_pb::{AbstainPayload, ApprovalRequestPayload, ApprovePayload, RejectPayload};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BallotChoice {
    Approve,
    Reject,
    Abstain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequestRecord {
    pub request_id: String,
    pub action: String,
    pub summary: String,
    pub details: Vec<u8>,
    pub required_approvals: u32,
    pub requested_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BallotRecord {
    pub request_id: String,
    pub choice: BallotChoice,
    pub sender: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QuorumState {
    pub request: Option<ApprovalRequestRecord>,
    pub ballots: BTreeMap<String, BallotRecord>,
}

/// The approval bar an accepted `ApprovalRequest` must clear, as the mode
/// resolves it against the session's bound policy.
///
/// Returned by [`QuorumMode::effective_threshold`] and, wrapped, by
/// [`QuorumMode::effective_threshold_for_session`]. Both variants are
/// reachable; "this session has no `ApprovalRequest` yet" is *not* a variant
/// here — that question is answered by the `Option` the session-level
/// accessor returns, so the two cannot be confused.
///
/// The policy's own inert case (`threshold.value <= 0`, which includes the
/// schema default and therefore every session with no `threshold` rule at
/// all) is **not** a variant either: the mode's documented fallback for it is
/// the `ApprovalRequest`'s own `required_approvals`, so it arrives here
/// already resolved as `Approvals(required_approvals)`. Surfacing
/// [`macp_core::policy::rules::EffectiveThreshold::Inert`] instead would hand
/// the fallback rule back to the caller, which is the re-implementation this
/// accessor exists to delete (issue #146).
///
/// **Deliberately not `#[non_exhaustive]`**, matching
/// [`macp_core::policy::rules::EffectiveThreshold`] and for the same reason:
/// a `_` arm in a caller would silently reinterpret a future variant as one
/// of these, and silently mis-handling a governance bar is the defect class
/// issue #145 was. Adding a variant is a major `cargo-semver-checks` lint
/// (`enum_variant_added`) and `release-plz.toml` sets `semver_check = true`,
/// so it blocks the release PR rather than slipping out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalThreshold {
    /// This many `Approve` ballots seal a **positive** commitment.
    ///
    /// Never zero, and for a session whose `ApprovalRequest` the mode
    /// accepted, never above the declared participant count: `on_message`
    /// constrains both the payload field and any policy replacement for it to
    /// `1..=participants`.
    Approvals(u32),
    /// The bound policy admits no positive commitment at any approval count.
    ///
    /// Reached by an unrecognised `threshold.type` — including `weighted`,
    /// which RFC-MACP-0012 1.2.0-draft removed from the vocabulary and
    /// reserved — or by a `percentage` over an empty participant set — see
    /// [`macp_core::policy::rules::EffectiveThreshold::Unsatisfiable`].
    /// `RegisterPolicy` refuses the first, so a session can only carry
    /// such a policy if the `PolicyDefinition` was constructed directly (or
    /// restored from a checkpoint that predates those checks). The second is
    /// not catchable at registration — there is no participant count there —
    /// and is blocked by `QuorumMode::on_session_start` rejecting an empty
    /// participant set. The mode seals **neither** outcome on such a session,
    /// positive or negative.
    Unsatisfiable,
}

pub struct QuorumMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl QuorumMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn encode_state(state: &QuorumState) -> Vec<u8> {
        crate::mode::util::encode_mode_state(state)
    }

    fn decode_state(data: &[u8]) -> Result<QuorumState, MacpError> {
        crate::mode::util::decode_mode_state(data)
    }

    /// Resolve the effective approval threshold for one `ApprovalRequest`,
    /// applying any policy override bound to the session.
    ///
    /// RFC-MACP-0011 §6: "When policy specifies a threshold override, it
    /// replaces (not supplements) the required_approvals value from
    /// ApprovalRequest."
    ///
    /// This is the bar the runtime itself enforces — the same call the
    /// mode's own `commitment_ready` makes — so a caller that needs the number
    /// should read it from here rather than re-deriving it (issue #146). The
    /// arithmetic lives one layer down in
    /// [`QuorumThreshold::effective`](macp_core::policy::rules::QuorumThreshold::effective),
    /// the *same* function `evaluate_quorum_commitment_outcome` calls, so one
    /// policy cannot produce two different bars in the two layers (issue
    /// #145). This function adds only the mode's fallback for an inert rule:
    /// `request.required_approvals`.
    ///
    /// Prefer [`Self::effective_threshold_for_session`] when you hold a
    /// [`Session`] rather than a decoded [`ApprovalRequestRecord`]; it is the
    /// same rule with the state decoding done for you. **Read the
    /// non-monotonicity warning there before probing readiness by
    /// experiment.**
    ///
    /// A rules object that fails to parse falls back to the schema defaults
    /// (`unwrap_or_default`) and therefore to `required_approvals`, where the
    /// evaluator instead denies the commitment. That divergence is recorded in
    /// `ASSUMPTIONS.md` and deliberately left alone here.
    pub fn effective_threshold(
        session: &Session,
        request: &ApprovalRequestRecord,
    ) -> ApprovalThreshold {
        let Some(ref policy) = session.policy_definition else {
            return ApprovalThreshold::Approvals(request.required_approvals);
        };
        let rules: macp_core::policy::rules::QuorumPolicyRules =
            serde_json::from_value(policy.rules.clone()).unwrap_or_default();
        match rules.threshold.effective(session.participants.len()) {
            macp_core::policy::rules::EffectiveThreshold::Inert => {
                ApprovalThreshold::Approvals(request.required_approvals)
            }
            macp_core::policy::rules::EffectiveThreshold::Approvals(required) => {
                ApprovalThreshold::Approvals(required)
            }
            macp_core::policy::rules::EffectiveThreshold::Unsatisfiable => {
                ApprovalThreshold::Unsatisfiable
            }
        }
    }

    /// Resolve the effective approval threshold for a quorum **session**,
    /// reading the accepted `ApprovalRequest` out of `session.mode_state`.
    ///
    /// This is the public entry point for "how many approvals does this
    /// session need?" (issue #146). Each layer of the return type answers one
    /// question, and the three answers must not be conflated:
    ///
    /// | Return | Meaning |
    /// |--------|---------|
    /// | `Ok(Some(`[`ApprovalThreshold::Approvals`]`(n)))` | `n` approvals seal a positive commitment |
    /// | `Ok(Some(`[`ApprovalThreshold::Unsatisfiable`]`))` | the bound policy can never be satisfied; no outcome will seal |
    /// | `Ok(None)` | no `ApprovalRequest` has been accepted yet — there is nothing to resolve |
    /// | `Err(`[`MacpError::InvalidModeState`]`)` | `session.mode_state` is not decodable quorum state, so no answer would be honest |
    ///
    /// The `Err` arm also covers a session belonging to a different mode:
    /// `QuorumState`'s fields are not `#[serde(default)]`, so another mode's
    /// state (or a bare `{}`) fails to decode rather than reporting a
    /// confident "no request".
    ///
    /// A session with no `threshold` policy rule — the common case — yields
    /// `Ok(Some(Approvals(required_approvals)))`, the value from the
    /// `ApprovalRequest` payload.
    ///
    /// # Do not probe commitment readiness to find this number
    ///
    /// The mode's internal `commitment_ready` predicate is **non-monotonic**
    /// in the approval count. It fires when the bar is met *or* when it has become
    /// mathematically unreachable, which is RFC-MACP-0011 §4a's trigger for a
    /// *negative* commitment:
    ///
    /// ```text
    /// approvals >= required || (counted > 0 && approvals + remaining < required)
    /// //                                       ^ remaining = participants - counted
    /// ```
    ///
    /// So readiness is a function of the whole ballot box — how many ballots
    /// are in and how they split — not of the approval count alone, and it is
    /// not a step function of that count. On three participants with
    /// `required = 3`: three rejections (0 approvals) are ready, one approval
    /// plus two rejections is ready, two approvals and one participant yet to
    /// vote is **not** ready, three approvals are ready. A binary search over
    /// readiness therefore returns a confident wrong answer, and even a
    /// linear sweep measures the decline trigger rather than the bar. Call
    /// this function instead; it returns the bar itself.
    pub fn effective_threshold_for_session(
        session: &Session,
    ) -> Result<Option<ApprovalThreshold>, MacpError> {
        if session.mode_state.is_empty() {
            return Ok(None);
        }
        let state = Self::decode_state(&session.mode_state)?;
        Ok(state
            .request
            .as_ref()
            .map(|request| Self::effective_threshold(session, request)))
    }

    fn commitment_ready(session: &Session, state: &QuorumState) -> bool {
        let request = match &state.request {
            Some(request) => request,
            None => return false,
        };
        let ApprovalThreshold::Approvals(required) = Self::effective_threshold(session, request)
        else {
            // Unsatisfiable threshold: neither outcome may be sealed. Without
            // this, the unreachable-threshold branch below would fire on it
            // and turn an unimplementable policy into a binding decline.
            return false;
        };
        let approvals = state
            .ballots
            .values()
            .filter(|ballot| ballot.choice == BallotChoice::Approve)
            .count() as u32;
        let total_eligible = session.participants.len() as u32;
        let counted = state.ballots.len() as u32;
        let remaining = total_eligible.saturating_sub(counted);
        // Ready when the threshold is reached, or when it has become
        // mathematically unreachable (RFC-MACP-0011 §4a, which makes that the
        // trigger for a negative Commitment).
        //
        // `counted > 0` guards the second branch. With no ballot cast,
        // `approvals + remaining < required` reduces to
        // `participants.len() < required` — a state reachable only through a
        // policy threshold larger than the participant pool, which
        // `on_message`'s ApprovalRequest arm now refuses up front and which
        // replay could otherwise re-introduce by binding edited policy rules
        // to an older session. Left ungated, it let the coordinator seal a
        // binding `quorum.rejected` before anyone voted (issue #145). Every
        // §4b decline the RFC describes — all-abstain, or abstentions plus
        // rejections — has at least one ballot behind it, so this refuses no
        // legitimate decline.
        approvals >= required || (counted > 0 && approvals + remaining < required)
    }
}

impl Mode for QuorumMode {
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
            "ApprovalRequest" if env.sender == session.initiator_sender => Ok(()),
            "ApprovalRequest" => Err(MacpError::Forbidden),
            "Commitment" => check_commitment_authority(session, &env.sender),
            _ if is_declared_participant(&session.participants, &env.sender) => Ok(()),
            _ => Err(MacpError::Forbidden),
        }
    }

    fn on_session_start(
        &self,
        session: &Session,
        _env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        if session.participants.is_empty() {
            return Err(MacpError::InvalidPayload);
        }
        Ok(ModeResponse::PersistState(Self::encode_state(
            &QuorumState::default(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            QuorumState::default()
        } else {
            Self::decode_state(&session.mode_state)?
        };

        match env.message_type.as_str() {
            "ApprovalRequest" => {
                if env.sender != session.initiator_sender {
                    return Err(MacpError::Forbidden);
                }
                let payload = ApprovalRequestPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if state.request.is_some()
                    || payload.request_id.is_empty()
                    || payload.required_approvals == 0
                    || payload.required_approvals > session.participants.len() as u32
                {
                    return Err(MacpError::InvalidPayload);
                }
                let record = ApprovalRequestRecord {
                    request_id: payload.request_id,
                    action: payload.action,
                    summary: payload.summary,
                    details: payload.details,
                    required_approvals: payload.required_approvals,
                    requested_by: env.sender.clone(),
                };
                // RFC-MACP-0011 §6 says a policy `threshold` *replaces*
                // `required_approvals`, so the replacement must satisfy the
                // same domain the check above just enforced on the field it
                // replaces: 1..=participants. A bar outside it makes the
                // positive outcome impossible from the first message, and the
                // coordinator could then seal a binding negative Commitment
                // with no ballot cast (issue #145). Refusing here reports the
                // misconfiguration at once instead of at commitment time, and
                // keeps a dead session from collecting ballots that can never
                // matter.
                match Self::effective_threshold(session, &record) {
                    ApprovalThreshold::Approvals(required)
                        if required >= 1 && required <= session.participants.len() as u32 => {}
                    other => {
                        tracing::warn!(
                            session_id = %session.session_id,
                            policy_id = session
                                .policy_definition
                                .as_ref()
                                .map(|p| p.policy_id.as_str())
                                .unwrap_or(""),
                            effective_threshold = ?other,
                            participants = session.participants.len(),
                            "quorum policy threshold is outside 1..=participants; \
                             refusing the ApprovalRequest"
                        );
                        return Err(MacpError::InvalidPayload);
                    }
                }
                state.request = Some(record);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Approve" => {
                let payload =
                    ApprovePayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                let request = state.request.as_ref().ok_or(MacpError::InvalidPayload)?;
                if payload.request_id != request.request_id
                    || state.ballots.contains_key(&env.sender)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.ballots.insert(
                    env.sender.clone(),
                    BallotRecord {
                        request_id: payload.request_id,
                        choice: BallotChoice::Approve,
                        sender: env.sender.clone(),
                        reason: payload.reason,
                    },
                );
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Reject" => {
                let payload =
                    RejectPayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                let request = state.request.as_ref().ok_or(MacpError::InvalidPayload)?;
                if payload.request_id != request.request_id
                    || state.ballots.contains_key(&env.sender)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.ballots.insert(
                    env.sender.clone(),
                    BallotRecord {
                        request_id: payload.request_id,
                        choice: BallotChoice::Reject,
                        sender: env.sender.clone(),
                        reason: payload.reason,
                    },
                );
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Abstain" => {
                let payload =
                    AbstainPayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                let request = state.request.as_ref().ok_or(MacpError::InvalidPayload)?;
                if payload.request_id != request.request_id
                    || state.ballots.contains_key(&env.sender)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.ballots.insert(
                    env.sender.clone(),
                    BallotRecord {
                        request_id: payload.request_id,
                        choice: BallotChoice::Abstain,
                        sender: env.sender.clone(),
                        reason: payload.reason,
                    },
                );
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                let commitment = validate_commitment_payload_for_session(session, &env.payload)?;
                if !Self::commitment_ready(session, &state) {
                    return Err(MacpError::InvalidPayload);
                }
                // Governance policy gate (shared): fail closed, only
                // an explicit Allow proceeds.
                let approve_count = state
                    .ballots
                    .values()
                    .filter(|b| b.choice == BallotChoice::Approve)
                    .count();
                let reject_count = state
                    .ballots
                    .values()
                    .filter(|b| b.choice == BallotChoice::Reject)
                    .count();
                let abstain_count = state
                    .ballots
                    .values()
                    .filter(|b| b.choice == BallotChoice::Abstain)
                    .count();
                enforce_commitment_policy(
                    session,
                    macp_core::policy::CommitmentMode::Quorum {
                        approve_count,
                        reject_count,
                        abstain_count,
                    },
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
        Session::builder("s1", "macp.mode.quorum.v1", "coordinator")
            .ttl_ms(60_000)
            .participants(vec!["alice".into(), "bob".into(), "carol".into()])
            .mode_version("1.0.0")
            .configuration_version("config")
            .policy_version("policy")
            .build()
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.quorum.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 0,
            payload,
        }
    }

    fn commitment_payload() -> Vec<u8> {
        commitment("quorum.approved", true)
    }

    fn commitment(action: &str, outcome_positive: bool) -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: action.into(),
            authority_scope: "deploy".into(),
            reason: "threshold met".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive,
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

    fn make_approval_request(request_id: &str, required: u32) -> Vec<u8> {
        ApprovalRequestPayload {
            request_id: request_id.into(),
            action: "deploy.production".into(),
            summary: "Deploy v2".into(),
            details: vec![],
            required_approvals: required,
        }
        .encode_to_vec()
    }

    fn make_approve(request_id: &str, reason: &str) -> Vec<u8> {
        ApprovePayload {
            request_id: request_id.into(),
            reason: reason.into(),
        }
        .encode_to_vec()
    }

    fn make_reject(request_id: &str, reason: &str) -> Vec<u8> {
        RejectPayload {
            request_id: request_id.into(),
            reason: reason.into(),
        }
        .encode_to_vec()
    }

    fn make_abstain(request_id: &str, reason: &str) -> Vec<u8> {
        AbstainPayload {
            request_id: request_id.into(),
            reason: reason.into(),
        }
        .encode_to_vec()
    }

    // --- Session Start ---

    #[test]
    fn session_start_initializes_state() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: QuorumState = serde_json::from_slice(&data).unwrap();
                assert!(state.request.is_none());
                assert!(state.ballots.is_empty());
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn session_start_requires_participants() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants.clear();
        let err = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- ApprovalRequest ---

    #[test]
    fn approval_request_from_coordinator() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: QuorumState = serde_json::from_slice(&data).unwrap();
                assert!(state.request.is_some());
                assert_eq!(state.request.unwrap().required_approvals, 2);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn duplicate_approval_request_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r2", 1),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn required_approvals_exceeds_participants_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 4),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn required_approvals_zero_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 0),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn non_coordinator_approval_request_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("alice", "ApprovalRequest", make_approval_request("r1", 2)),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- Ballots ---

    #[test]
    fn participant_can_approve() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "looks good")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: QuorumState = serde_json::from_slice(&data).unwrap();
                assert!(state.ballots.contains_key("alice"));
                assert_eq!(state.ballots["alice"].choice, BallotChoice::Approve);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn participant_can_reject() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Reject", make_reject("r1", "not ready")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: QuorumState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.ballots["alice"].choice, BallotChoice::Reject);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn participant_can_abstain() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Abstain", make_abstain("r1", "no opinion")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: QuorumState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.ballots["alice"].choice, BallotChoice::Abstain);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn duplicate_ballot_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "again")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn ballot_before_request_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "premature")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn wrong_request_id_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r2", "wrong id")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Commitment ---

    #[test]
    fn commitment_when_threshold_reached() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("bob", "Approve", make_approve("r1", "yes")))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_when_threshold_unreachable() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // All 3 must approve for threshold=3, but alice rejects
        let result = mode
            .on_message(&session, &env("alice", "Reject", make_reject("r1", "no")))
            .unwrap();
        apply(&mut session, result);
        // Threshold is now unreachable (need 3, max possible = 2)
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_before_threshold_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        // Only 1 approval, threshold is 2, and 2 participants left can still vote
        let err = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn non_coordinator_commitment_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("bob", "Approve", make_approve("r1", "yes")))
            .unwrap();
        apply(&mut session, result);
        let commit_env = env("alice", "Commitment", commitment_payload());
        let err = mode.authorize_sender(&session, &commit_env).unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- Full lifecycle ---

    #[test]
    fn full_quorum_approve_lifecycle() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "green")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("bob", "Approve", make_approve("r1", "ready")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn full_quorum_reject_lifecycle() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Reject", make_reject("r1", "not ready")),
            )
            .unwrap();
        apply(&mut session, result);
        // Threshold unreachable: need 3, 1 rejected, only 2 left, max possible = 2
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Commitment version mismatch ---

    #[test]
    fn commitment_version_mismatch_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("bob", "Approve", make_approve("r1", "yes")))
            .unwrap();
        apply(&mut session, result);
        let bad_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "quorum.approved".into(),
            authority_scope: "deploy".into(),
            reason: "threshold met".into(),
            mode_version: "wrong".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("coordinator", "Commitment", bad_commitment))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Unknown message type ---

    #[test]
    fn unknown_message_type_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("alice", "CustomType", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Policy ---

    #[test]
    fn policy_denies_commitment_when_quorum_not_met_due_to_abstentions() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // Require all 3 participants as voters, but abstentions don't count toward quorum
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-strict-quorum".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "strict quorum".into(),
            rules: serde_json::json!({
                "threshold": { "type": "n_of_m", "value": 3 },
                "abstention": { "counts_toward_quorum": false, "interpretation": "neutral" }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("bob", "Approve", make_approve("r1", "yes")))
            .unwrap();
        apply(&mut session, result);
        // carol abstains — with counts_toward_quorum=false, effective voters = 2 < 3
        let result = mode
            .on_message(
                &session,
                &env("carol", "Abstain", make_abstain("r1", "no opinion")),
            )
            .unwrap();
        apply(&mut session, result);
        // Commitment should be denied (quorum not met: 2 effective voters < 3 required)
        let err = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "PolicyDenied");
    }

    // --- Negative outcome commitment ---

    #[test]
    fn negative_outcome_quorum_rejected() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // 3 participants, required_approvals = 2
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // Two participants reject, making the threshold (2 approvals) unreachable
        let result = mode
            .on_message(
                &session,
                &env("alice", "Reject", make_reject("r1", "not ready")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("bob", "Reject", make_reject("r1", "disagree")),
            )
            .unwrap();
        apply(&mut session, result);
        // Threshold is unreachable: 0 approvals + 1 remaining < 2 required
        // Commit with negative outcome
        let negative_commitment = commitment("quorum.rejected", false);
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", negative_commitment),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- All participants abstain — eligible for negative commitment ---

    #[test]
    fn all_participants_abstain_allows_negative_commitment() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // 3 participants, required_approvals = 2
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // All 3 participants abstain
        let result = mode
            .on_message(
                &session,
                &env("alice", "Abstain", make_abstain("r1", "neutral")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("bob", "Abstain", make_abstain("r1", "neutral")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("carol", "Abstain", make_abstain("r1", "neutral")),
            )
            .unwrap();
        apply(&mut session, result);
        // Threshold is unreachable: 0 approvals + 0 remaining < 2 required
        // commitment_ready() returns true, so a negative commitment should succeed
        let negative_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "quorum.rejected".into(),
            authority_scope: "deploy".into(),
            reason: "all abstained".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: false,
            supersedes: None,
        }
        .encode_to_vec();
        let result = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", negative_commitment),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Initiator not in participants cannot cast ballot ---

    #[test]
    fn initiator_not_in_participants_cannot_cast_ballot() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // coordinator is initiator but NOT in participants
        // participants are alice, bob, carol (coordinator excluded)
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // coordinator tries to Approve — should be Forbidden because coordinator
        // is not a declared participant
        let approve_env = env("coordinator", "Approve", make_approve("r1", "yes"));
        let err = mode.authorize_sender(&session, &approve_env).unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // ── Quorum policy threshold override (RFC-MACP-0011) ───────────

    #[test]
    fn policy_threshold_overrides_required_approvals() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // Policy sets n_of_m threshold to 1, while ApprovalRequest requires 3
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "threshold-override".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "low threshold".into(),
            rules: serde_json::json!({
                "threshold": { "type": "n_of_m", "value": 1.0 }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3), // requires 3, but policy overrides to 1
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // Just 1 approval should make commitment ready (policy overrides to 1)
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        // Commitment should succeed
        let commit = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn policy_percentage_threshold() {
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // 3 participants, 50% → ceil(1.5) = 2 required
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "pct-override".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "percentage threshold".into(),
            rules: serde_json::json!({
                "threshold": { "type": "percentage", "value": 50.0 }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // 1 approval is not enough (need 2 for 50% of 3)
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        // 2nd approval makes it ready
        let result = mode
            .on_message(
                &session,
                &env("bob", "Approve", make_approve("r1", "agreed")),
            )
            .unwrap();
        apply(&mut session, result);
        let commit = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn all_abstain_eligible_for_negative_commitment() {
        // RFC-MACP-0011: "When all eligible participants have abstained, the Session
        // becomes eligible for Commitment with a negative outcome."
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // All 3 participants abstain
        for sender in &["alice", "bob", "carol"] {
            let result = mode
                .on_message(
                    &session,
                    &env(sender, "Abstain", make_abstain("r1", "neutral")),
                )
                .unwrap();
            apply(&mut session, result);
        }
        // Commitment should be ready (0 approvals + 0 remaining < 2 required)
        let commit = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "Commitment",
                    commitment("quorum.rejected", false),
                ),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    // ── Threshold rounding parity across mode and evaluator (#145) ──

    fn quorum_policy(rules: serde_json::Value) -> macp_core::policy::PolicyDefinition {
        macp_core::policy::PolicyDefinition {
            policy_id: "threshold-parity".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "threshold parity fixture".into(),
            rules,
            schema_version: 1,
        }
    }

    fn session_with(participants: usize, rules: serde_json::Value) -> Session {
        let names = ["alice", "bob", "carol", "dave", "erin"];
        let mut session = Session::builder("s1", "macp.mode.quorum.v1", "coordinator")
            .ttl_ms(60_000)
            .participants(
                names[..participants]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
            .mode_version("1.0.0")
            .configuration_version("config")
            .policy_version("policy")
            .build();
        session.policy_definition = Some(quorum_policy(rules));
        session
    }

    /// A request whose own `required_approvals` is deliberately unlike any
    /// policy answer, so a silent fallback to it would show up as a mismatch
    /// rather than a pass.
    fn matrix_request() -> ApprovalRequestRecord {
        ApprovalRequestRecord {
            request_id: "r1".into(),
            action: "deploy.production".into(),
            summary: "Deploy v2".into(),
            details: vec![],
            required_approvals: 99,
            requested_by: "coordinator".into(),
        }
    }

    /// Collapse the public [`ApprovalThreshold`] to the `Option<u32>` the
    /// evaluator probe below speaks, so the two are directly comparable.
    fn as_option(threshold: ApprovalThreshold) -> Option<u32> {
        match threshold {
            ApprovalThreshold::Approvals(required) => Some(required),
            ApprovalThreshold::Unsatisfiable => None,
        }
    }

    /// The approval bar the **mode** derives from a request in hand.
    fn mode_required(participants: usize, rules: serde_json::Value) -> Option<u32> {
        let session = session_with(participants, rules);
        as_option(QuorumMode::effective_threshold(&session, &matrix_request()))
    }

    /// The same bar read through the **session-level** accessor, which decodes
    /// the request out of `session.mode_state` itself (issue #146 — this is
    /// the form a downstream caller reaches). The state is seated directly
    /// rather than through `on_message` because the matrix deliberately
    /// includes over-participant thresholds, which the `ApprovalRequest` arm
    /// refuses; the accessor must still answer for a session that carries one
    /// (replay can rebind edited policy rules to an accepted request).
    fn session_required(participants: usize, rules: serde_json::Value) -> Option<u32> {
        let mut session = session_with(participants, rules);
        session.mode_state = QuorumMode::encode_state(&QuorumState {
            request: Some(matrix_request()),
            ballots: BTreeMap::new(),
        });
        as_option(
            QuorumMode::effective_threshold_for_session(&session)
                .expect("state seated by encode_state decodes")
                .expect("a request was seated"),
        )
    }

    /// The approval bar the **evaluator** derives, recovered from its public
    /// behaviour: it denies a positive commitment while `approve_count` is
    /// below the bar and allows it at or above, so the smallest allowed count
    /// *is* the bar. `None` when no count is ever allowed.
    fn evaluator_required(participants: usize, rules: serde_json::Value) -> Option<u32> {
        let policy = quorum_policy(rules);
        (0u32..=64).find(|approve| {
            matches!(
                macp_policy::evaluator::evaluate_quorum_commitment_outcome(
                    &policy,
                    *approve as usize,
                    0,
                    0,
                    participants,
                    true,
                ),
                macp_core::policy::PolicyDecision::Allow { .. }
            )
        })
    }

    #[test]
    fn fractional_threshold_ceils_to_one_in_both_layers() {
        // Issue #145: the mode truncated (`0.5 as u32 == 0`) while the
        // evaluator ceiled, and a bar of 0 is met before any ballot is cast.
        let rules = serde_json::json!({ "threshold": { "type": "n_of_m", "value": 0.5 } });
        assert_eq!(mode_required(3, rules.clone()), Some(1));
        assert_eq!(evaluator_required(3, rules), Some(1));
    }

    #[test]
    fn threshold_is_floored_at_one_so_a_zero_bar_is_unreachable() {
        // Every positive value below 1 resolves to 1, in both layers.
        for value in [0.000_001, 0.1, 0.49, 0.5, 0.99] {
            let rules = serde_json::json!({ "threshold": { "type": "n_of_m", "value": value } });
            assert_eq!(mode_required(3, rules.clone()), Some(1), "value {value}");
            assert_eq!(evaluator_required(3, rules), Some(1), "value {value}");
        }
    }

    #[test]
    fn zero_ballot_decline_is_refused_under_a_fractional_threshold() {
        // With the old truncation the bar was 0, so `approvals >= required`
        // held with an empty ballot box and this negative commitment sealed.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(
            3,
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 0.5 } }),
        );
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "Commitment",
                    commitment("quorum.rejected", false),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        // Nor does the positive one seal with an empty ballot box.
        let err = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        // One approval meets the ceiled bar, and the session resolves.
        let result = mode
            .on_message(
                &session,
                &env("alice", "Approve", make_approve("r1", "yes")),
            )
            .unwrap();
        apply(&mut session, result);
        let commit = mode
            .on_message(
                &session,
                &env("coordinator", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn weighted_threshold_is_unsatisfiable_not_a_raw_approval_count() {
        // `weighted` used to fall through the `_` arm in both layers and be
        // read as a raw count. `threshold.value` is typed `integer` by the
        // canonical schema and per-participant quorum weights are not
        // modelled, so there is nothing to implement — it fails closed.
        let rules = serde_json::json!({ "threshold": { "type": "weighted", "value": 2 } });
        assert_eq!(mode_required(3, rules.clone()), None);
        assert_eq!(evaluator_required(3, rules.clone()), None);

        // An unrecognised type fails closed the same way.
        let unknown = serde_json::json!({ "threshold": { "type": "two_thirds", "value": 2 } });
        assert_eq!(mode_required(3, unknown.clone()), None);
        assert_eq!(evaluator_required(3, unknown), None);

        // End to end: the ApprovalRequest is refused, so no ballot is ever
        // cast into a session that could not resolve.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(3, rules);
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn mode_and_evaluator_agree_across_the_threshold_matrix() {
        // RFC-MACP-0011 §7: given the same participants, threshold and
        // ballots, every implementation must derive the same commitment
        // eligibility. Both layers now resolve through
        // `QuorumThreshold::effective`; this asserts the property they are
        // supposed to have rather than the fact that they share a function.
        //
        // `value = 0` is deliberately OUT of the matrix: both layers gate on
        // `value > 0.0` and diverge *outside* that gate (the mode falls back
        // to `required_approvals`, the evaluator applies no bar at all). That
        // residual divergence is recorded in `ASSUMPTIONS.md` and is not what
        // the ceil+floor fix addresses.
        let cases: [(&str, [f64; 5]); 3] = [
            ("n_of_m", [0.5, 1.0, 2.0, 3.0, 5.0]),
            ("count", [0.5, 1.0, 2.0, 3.0, 5.0]),
            // Fractional, sub-participant, exact, whole, and over-100.
            ("percentage", [0.5, 33.0, 50.0, 100.0, 150.0]),
        ];
        for (threshold_type, values) in cases {
            for value in values {
                for participants in 1..=3usize {
                    let rules = serde_json::json!({
                        "threshold": { "type": threshold_type, "value": value }
                    });
                    let mode = mode_required(participants, rules.clone());
                    let session = session_required(participants, rules.clone());
                    let evaluator = evaluator_required(participants, rules);
                    assert_eq!(
                        mode, evaluator,
                        "type={threshold_type} value={value} participants={participants}"
                    );
                    // Issue #146: the public session-level accessor must
                    // report the same bar the evaluator enforces, not merely
                    // the same bar the request-level form derives.
                    assert_eq!(
                        session, evaluator,
                        "session-level accessor: type={threshold_type} value={value} \
                         participants={participants}"
                    );
                    assert_ne!(
                        mode,
                        Some(0),
                        "type={threshold_type} value={value} participants={participants}: \
                         a bar of 0 is met before any ballot is cast"
                    );
                }
            }
        }
    }

    #[test]
    fn session_threshold_separates_no_request_from_unsatisfiable() {
        // Issue #146: the accessor's three answers must stay distinguishable.
        // Before this phase the inner function returned `Option<u32>` with
        // `None` meaning "unsatisfiable"; a session-level `Option<u32>` would
        // have made "no request yet" and "this policy can never be met" the
        // same value, which is the one thing a caller cannot afford here.
        let rules = serde_json::json!({ "threshold": { "type": "n_of_m", "value": 2 } });

        // Nothing accepted yet: empty mode_state, and explicitly seated
        // default state (replay writes the latter at SessionStart).
        let mut session = session_with(3, rules.clone());
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            None
        );
        session.mode_state = QuorumMode::encode_state(&QuorumState::default());
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            None
        );

        // Request accepted under a satisfiable policy: the bar itself.
        session.mode_state = QuorumMode::encode_state(&QuorumState {
            request: Some(matrix_request()),
            ballots: BTreeMap::new(),
        });
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            Some(ApprovalThreshold::Approvals(2))
        );

        // Same seated request under an unsatisfiable policy: a distinct
        // answer, not the `None` that means "no request".
        session.policy_definition = Some(quorum_policy(
            serde_json::json!({ "threshold": { "type": "weighted", "value": 2 } }),
        ));
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            Some(ApprovalThreshold::Unsatisfiable)
        );

        // Undecodable state is a third answer again: no honest bar exists, so
        // it is not silently reported as "no request".
        session.mode_state = b"{not-quorum-state".to_vec();
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session)
                .unwrap_err()
                .to_string(),
            "InvalidModeState"
        );
    }

    #[test]
    fn session_threshold_falls_back_to_the_requests_own_required_approvals() {
        // The common case a caller most needs: no policy override at all
        // resolves to the ApprovalRequest payload's own value, already
        // applied, so nothing about the fallback rule is left for the caller
        // to re-implement (issue #146). Driven through `on_message` so the
        // state is the one the runtime itself writes.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = None;
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            None
        );
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 2),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            Some(ApprovalThreshold::Approvals(2))
        );
        // An inert policy rule (`value: 0`, the schema default) is the same
        // fallback, not a separate outcome the caller has to handle.
        session.policy_definition = Some(quorum_policy(
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 0 } }),
        ));
        assert_eq!(
            QuorumMode::effective_threshold_for_session(&session).unwrap(),
            Some(ApprovalThreshold::Approvals(2))
        );
    }

    #[test]
    fn over_participant_policy_threshold_refuses_the_approval_request() {
        // RFC-MACP-0011 §6: a policy threshold *replaces* `required_approvals`,
        // which this arm already constrains to 1..=participants. A replacement
        // outside that domain makes the positive outcome impossible from the
        // first message, and `commitment_ready`'s unreachable branch would let
        // the coordinator seal a binding `quorum.rejected` with no ballot cast.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(
            3,
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 5 } }),
        );
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        // A threshold exactly at the participant count is still fine.
        session.policy_definition = Some(quorum_policy(
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 3 } }),
        ));
        assert!(mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .is_ok());
    }

    #[test]
    fn zero_ballot_decline_is_refused_when_policy_rebinds_over_the_participant_pool() {
        // The belt behind the ApprovalRequest guard: replay re-resolves
        // `policy_version` against the registry, so a policy edited between
        // runs can bind a larger threshold to a session whose request was
        // already accepted. The unreachable-threshold branch must not fire
        // with an empty ballot box.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(
            3,
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 3 } }),
        );
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        // The policy is edited out from under the accepted request.
        session.policy_definition = Some(quorum_policy(
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 5 } }),
        ));
        let err = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "Commitment",
                    commitment("quorum.rejected", false),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn genuine_unreachable_threshold_still_permits_a_negative_commitment() {
        // RFC-MACP-0011 §4a/§4b: once ballots make the bar unreachable, the
        // negative Commitment is the legitimate terminal. The zero-ballot gate
        // must not cost us this — all three participants abstain, which is
        // §4b's own worked example.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(
            3,
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 2 } }),
        );
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        for participant in ["alice", "bob", "carol"] {
            let result = mode
                .on_message(
                    &session,
                    &env(participant, "Abstain", make_abstain("r1", "no opinion")),
                )
                .unwrap();
            apply(&mut session, result);
        }
        let commit = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "Commitment",
                    commitment("quorum.rejected", false),
                ),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn a_single_ballot_still_unlocks_the_unreachable_branch() {
        // The gate is "at least one ballot", not "all ballots": one rejection
        // against a 3-of-3 bar makes the threshold unreachable and the decline
        // legitimate per §4a.
        let mode = QuorumMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = session_with(
            3,
            serde_json::json!({ "threshold": { "type": "n_of_m", "value": 3 } }),
        );
        let result = mode
            .on_session_start(&session, &env("coordinator", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "ApprovalRequest",
                    make_approval_request("r1", 3),
                ),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("alice", "Reject", make_reject("r1", "no")))
            .unwrap();
        apply(&mut session, result);
        let commit = mode
            .on_message(
                &session,
                &env(
                    "coordinator",
                    "Commitment",
                    commitment("quorum.rejected", false),
                ),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }
}
