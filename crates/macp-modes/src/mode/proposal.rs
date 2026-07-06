use crate::mode::util::{
    check_commitment_authority, enforce_commitment_policy, is_declared_participant,
    participants_all_accept, validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;
use macp_pb::proposal_pb::{
    AcceptPayload, CounterProposalPayload, ProposalPayload, RejectPayload, WithdrawPayload,
};
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalDisposition {
    Live,
    Withdrawn,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProposalPhase {
    #[default]
    Negotiating,
    Converged,
    TerminalRejected,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub proposal_id: String,
    pub title: String,
    pub summary: String,
    pub details: Vec<u8>,
    pub tags: Vec<String>,
    pub proposer: String,
    pub supersedes_proposal_id: Option<String>,
    pub disposition: ProposalDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalRejectRecord {
    pub proposal_id: String,
    pub sender: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectRecord {
    pub proposal_id: String,
    pub sender: String,
    pub reason: String,
    pub terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProposalState {
    pub proposals: BTreeMap<String, ProposalRecord>,
    pub accepts: BTreeMap<String, String>,
    pub terminal_rejections: Vec<TerminalRejectRecord>,
    /// All rejections (terminal and non-terminal) for audit trail.
    #[serde(default)]
    pub rejections: Vec<RejectRecord>,
    #[serde(default)]
    pub phase: ProposalPhase,
}

pub struct ProposalMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl ProposalMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn encode_state(state: &ProposalState) -> Vec<u8> {
        crate::mode::util::encode_mode_state(state)
    }

    fn decode_state(data: &[u8]) -> Result<ProposalState, MacpError> {
        crate::mode::util::decode_mode_state(data)
    }

    fn live_proposal<'a>(
        state: &'a ProposalState,
        proposal_id: &str,
    ) -> Option<&'a ProposalRecord> {
        state
            .proposals
            .get(proposal_id)
            .filter(|record| record.disposition == ProposalDisposition::Live)
    }

    fn refresh_phase(session: &Session, state: &mut ProposalState) {
        // RFC-MACP-0012: acceptance.criterion controls convergence check
        let criterion = session
            .policy_definition
            .as_ref()
            .map(|p| {
                serde_json::from_value::<macp_core::policy::rules::ProposalPolicyRules>(
                    p.rules.clone(),
                )
                .unwrap_or_default()
                .acceptance
                .criterion
            })
            .unwrap_or_else(|| "all_parties".to_string());

        state.phase = if !state.terminal_rejections.is_empty() {
            ProposalPhase::TerminalRejected
        } else if state
            .proposals
            .values()
            .filter(|proposal| proposal.disposition == ProposalDisposition::Live)
            .any(|proposal| {
                Self::check_acceptance_criterion(
                    &criterion,
                    session,
                    state,
                    &proposal.proposal_id,
                    &proposal.proposer,
                )
            })
        {
            ProposalPhase::Converged
        } else {
            ProposalPhase::Negotiating
        };
    }

    fn check_acceptance_criterion(
        criterion: &str,
        session: &Session,
        state: &ProposalState,
        proposal_id: &str,
        proposer: &str,
    ) -> bool {
        match criterion {
            "counterparty" => {
                // All participants except the proposer must accept
                session
                    .participants
                    .iter()
                    .filter(|p| p.as_str() != proposer)
                    .all(|p| state.accepts.get(p).map(String::as_str) == Some(proposal_id))
            }
            "initiator" => {
                // Only the session initiator must accept
                state
                    .accepts
                    .get(&session.initiator_sender)
                    .map(String::as_str)
                    == Some(proposal_id)
            }
            _ => {
                // "all_parties" (default)
                participants_all_accept(&session.participants, &state.accepts, proposal_id)
            }
        }
    }

    fn commitment_ready(state: &ProposalState) -> bool {
        matches!(
            state.phase,
            ProposalPhase::Converged | ProposalPhase::TerminalRejected
        )
    }

    fn ensure_mutable(state: &ProposalState) -> Result<(), MacpError> {
        if state.phase == ProposalPhase::Committed {
            Err(MacpError::SessionNotOpen)
        } else {
            Ok(())
        }
    }
}

impl Mode for ProposalMode {
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
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
            &ProposalState::default(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            ProposalState::default()
        } else {
            Self::decode_state(&session.mode_state)?
        };
        Self::ensure_mutable(&state)?;

        match env.message_type.as_str() {
            "Proposal" => {
                let payload = ProposalPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if payload.proposal_id.trim().is_empty()
                    || state.proposals.contains_key(&payload.proposal_id)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.proposals.insert(
                    payload.proposal_id.clone(),
                    ProposalRecord {
                        proposal_id: payload.proposal_id,
                        title: payload.title,
                        summary: payload.summary,
                        details: payload.details,
                        tags: payload.tags,
                        proposer: env.sender.clone(),
                        supersedes_proposal_id: None,
                        disposition: ProposalDisposition::Live,
                    },
                );
                Self::refresh_phase(session, &mut state);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "CounterProposal" => {
                let payload = CounterProposalPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if payload.proposal_id.trim().is_empty()
                    || payload.supersedes_proposal_id.trim().is_empty()
                    || state.proposals.contains_key(&payload.proposal_id)
                    || !state
                        .proposals
                        .contains_key(&payload.supersedes_proposal_id)
                {
                    return Err(MacpError::InvalidPayload);
                }
                // RFC-MACP-0012: enforce max_rounds at submission time to prevent
                // unbounded state growth. The evaluator also checks at commitment.
                if let Some(ref policy) = session.policy_definition {
                    let rules = serde_json::from_value::<
                        macp_core::policy::rules::ProposalPolicyRules,
                    >(policy.rules.clone())
                    .unwrap_or_default();
                    if rules.counter_proposal.max_rounds > 0 {
                        let counter_count = state
                            .proposals
                            .values()
                            .filter(|p| p.supersedes_proposal_id.is_some())
                            .count();
                        if counter_count >= rules.counter_proposal.max_rounds {
                            return Err(MacpError::InvalidPayload);
                        }
                    }
                }
                state.proposals.insert(
                    payload.proposal_id.clone(),
                    ProposalRecord {
                        proposal_id: payload.proposal_id,
                        title: payload.title,
                        summary: payload.summary,
                        details: payload.details,
                        tags: Vec::new(),
                        proposer: env.sender.clone(),
                        supersedes_proposal_id: Some(payload.supersedes_proposal_id),
                        disposition: ProposalDisposition::Live,
                    },
                );
                Self::refresh_phase(session, &mut state);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Accept" => {
                let payload =
                    AcceptPayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                if Self::live_proposal(&state, &payload.proposal_id).is_none() {
                    return Err(MacpError::InvalidPayload);
                }
                state
                    .accepts
                    .insert(env.sender.clone(), payload.proposal_id);
                Self::refresh_phase(session, &mut state);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Reject" => {
                let payload =
                    RejectPayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                // Reject only applies to live proposals (consistent with Accept validation)
                if Self::live_proposal(&state, &payload.proposal_id).is_none() {
                    return Err(MacpError::InvalidPayload);
                }
                // RFC-MACP-0012: terminal_on_any_reject overrides per-message terminal flag
                let is_terminal = payload.terminal
                    || session.policy_definition.as_ref().is_some_and(|p| {
                        serde_json::from_value::<macp_core::policy::rules::ProposalPolicyRules>(
                            p.rules.clone(),
                        )
                        .unwrap_or_default()
                        .rejection
                        .terminal_on_any_reject
                    });
                // Always record the rejection for audit trail
                state.rejections.push(RejectRecord {
                    proposal_id: payload.proposal_id.clone(),
                    sender: env.sender.clone(),
                    reason: payload.reason.clone(),
                    terminal: is_terminal,
                });
                if is_terminal {
                    state.terminal_rejections.push(TerminalRejectRecord {
                        proposal_id: payload.proposal_id,
                        sender: env.sender.clone(),
                        reason: payload.reason,
                    });
                }
                Self::refresh_phase(session, &mut state);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Withdraw" => {
                let payload = WithdrawPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if payload.proposal_id.trim().is_empty() {
                    return Err(MacpError::InvalidPayload);
                }
                let record = state
                    .proposals
                    .get_mut(&payload.proposal_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if record.proposer != env.sender {
                    return Err(MacpError::Forbidden);
                }
                if record.disposition == ProposalDisposition::Withdrawn {
                    return Err(MacpError::InvalidPayload);
                }
                record.disposition = ProposalDisposition::Withdrawn;
                state.accepts.retain(|_, pid| pid != &payload.proposal_id);
                state
                    .terminal_rejections
                    .retain(|r| r.proposal_id != payload.proposal_id);
                Self::refresh_phase(session, &mut state);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                let commitment = validate_commitment_payload_for_session(session, &env.payload)?;
                Self::refresh_phase(session, &mut state);
                if !Self::commitment_ready(&state) {
                    return Err(MacpError::InvalidPayload);
                }
                // Governance policy gate (shared): fail closed, only
                // an explicit Allow proceeds.
                let counter_count = state
                    .proposals
                    .values()
                    .filter(|p| p.supersedes_proposal_id.is_some())
                    .count();
                enforce_commitment_policy(
                    session,
                    macp_core::policy::CommitmentMode::Proposal {
                        counter_proposal_count: counter_count,
                    },
                    commitment.outcome_positive,
                    &*self.evaluator,
                )?;
                state.phase = ProposalPhase::Committed;
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
        Session::builder("s1", "macp.mode.proposal.v1", "agent://buyer")
            .ttl_ms(60_000)
            .participants(vec!["agent://buyer".into(), "agent://seller".into()])
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .policy_version("policy-1")
            .build()
    }

    fn decode(session: &Session) -> ProposalState {
        serde_json::from_slice(&session.mode_state).unwrap()
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.proposal.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 0,
            payload,
        }
    }

    fn commitment(session: &Session, action: &str) -> Vec<u8> {
        let outcome_positive = !action.contains("rejected")
            && !action.contains("failed")
            && !action.contains("declined");
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: action.into(),
            authority_scope: "commercial".into(),
            reason: "bound".into(),
            mode_version: session.mode_version.clone(),
            policy_version: session.policy_version.clone(),
            configuration_version: session.configuration_version.clone(),
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

    fn make_proposal(id: &str) -> Vec<u8> {
        ProposalPayload {
            proposal_id: id.into(),
            title: format!("offer-{id}"),
            summary: "summary".into(),
            details: vec![],
            tags: vec![],
        }
        .encode_to_vec()
    }

    fn make_accept(id: &str) -> Vec<u8> {
        AcceptPayload {
            proposal_id: id.into(),
            reason: String::new(),
        }
        .encode_to_vec()
    }

    fn make_reject(id: &str, terminal: bool) -> Vec<u8> {
        RejectPayload {
            proposal_id: id.into(),
            terminal,
            reason: "no".into(),
        }
        .encode_to_vec()
    }

    fn make_withdraw(id: &str) -> Vec<u8> {
        WithdrawPayload {
            proposal_id: id.into(),
            reason: "changed mind".into(),
        }
        .encode_to_vec()
    }

    #[test]
    fn session_start_requires_participants() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants.clear();
        assert_eq!(
            mode.on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn commitment_requires_acceptance_convergence() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);

        assert_eq!(
            mode.on_message(
                &session,
                &env(
                    "agent://buyer",
                    "Commitment",
                    commitment(&session, "proposal.accepted"),
                ),
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );

        let resp = mode
            .on_message(&session, &env("agent://buyer", "Accept", make_accept("p1")))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Accept", make_accept("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        mode.on_message(
            &session,
            &env(
                "agent://buyer",
                "Commitment",
                commitment(&session, "proposal.accepted"),
            ),
        )
        .unwrap();
    }

    #[test]
    fn terminal_rejection_allows_negative_commitment() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Reject", make_reject("p1", true)),
            )
            .unwrap();
        apply(&mut session, resp);
        mode.on_message(
            &session,
            &env(
                "agent://buyer",
                "Commitment",
                commitment(&session, "proposal.rejected"),
            ),
        )
        .unwrap();
    }

    #[test]
    fn withdraw_clears_terminal_rejections() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        // Seller proposes p1
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Buyer terminally rejects p1
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Reject", make_reject("p1", true)),
            )
            .unwrap();
        apply(&mut session, resp);
        // Seller withdraws p1 — terminal rejection should be cleared
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Withdraw", make_withdraw("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Seller proposes p2
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p2")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Commitment should NOT be ready (no acceptance convergence, no terminal rejection)
        let err = mode
            .on_message(
                &session,
                &env(
                    "agent://buyer",
                    "Commitment",
                    commitment(&session, "proposal.rejected"),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    fn make_counter_proposal(id: &str, supersedes: &str) -> Vec<u8> {
        CounterProposalPayload {
            proposal_id: id.into(),
            supersedes_proposal_id: supersedes.into(),
            title: format!("counter-{id}"),
            summary: "counter".into(),
            details: vec![],
        }
        .encode_to_vec()
    }

    #[test]
    fn empty_proposal_id_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let empty = ProposalPayload {
            proposal_id: "".into(),
            title: "title".into(),
            summary: "summary".into(),
            details: vec![],
            tags: vec![],
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://seller", "Proposal", empty))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn counterproposal_requires_valid_supersedes() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Supersedes non-existent proposal
        let bad_counter = make_counter_proposal("p2", "nonexistent");
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://buyer", "CounterProposal", bad_counter)
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn counterproposal_chain_works() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env(
                    "agent://buyer",
                    "CounterProposal",
                    make_counter_proposal("p2", "p1"),
                ),
            )
            .unwrap();
        apply(&mut session, resp);
        // Chain: p3 supersedes p2
        mode.on_message(
            &session,
            &env(
                "agent://seller",
                "CounterProposal",
                make_counter_proposal("p3", "p2"),
            ),
        )
        .unwrap();
    }

    #[test]
    fn non_terminal_reject_does_not_enable_commitment() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Non-terminal reject
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Reject", make_reject("p1", false)),
            )
            .unwrap();
        apply(&mut session, resp);
        // Verify the rejection is still recorded in state for audit
        let state = decode(&session);
        assert_eq!(state.rejections.len(), 1);
        assert_eq!(state.rejections[0].proposal_id, "p1");
        assert!(!state.rejections[0].terminal);
        assert!(state.terminal_rejections.is_empty());
        // But commitment should still be blocked
        assert_eq!(
            mode.on_message(
                &session,
                &env(
                    "agent://buyer",
                    "Commitment",
                    commitment(&session, "proposal.rejected"),
                ),
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn non_participant_cannot_propose() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let err = mode
            .authorize_sender(
                &session,
                &env("agent://outsider", "Proposal", make_proposal("p1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn commitment_version_mismatch_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(&session, &env("agent://buyer", "Accept", make_accept("p1")))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Accept", make_accept("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "proposal.accepted".into(),
            authority_scope: "commercial".into(),
            reason: "bound".into(),
            mode_version: "wrong".into(),
            policy_version: session.policy_version.clone(),
            configuration_version: session.configuration_version.clone(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://buyer", "Commitment", bad))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn accept_on_withdrawn_proposal_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Withdraw", make_withdraw("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(&session, &env("agent://buyer", "Accept", make_accept("p1")))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn commitment_from_non_initiator_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let err = mode
            .authorize_sender(
                &session,
                &env(
                    "agent://seller",
                    "Commitment",
                    commitment(&session, "proposal.accepted"),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn terminal_rejection_on_different_proposal_survives_withdraw() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        // Seller proposes p1 and p2
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p2")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Buyer terminally rejects p2
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Reject", make_reject("p2", true)),
            )
            .unwrap();
        apply(&mut session, resp);
        // Seller withdraws p1 — p2's terminal rejection survives
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Withdraw", make_withdraw("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Commitment should be ready because p2 still has a terminal rejection
        mode.on_message(
            &session,
            &env(
                "agent://buyer",
                "Commitment",
                commitment(&session, "proposal.rejected"),
            ),
        )
        .unwrap();
    }

    #[test]
    fn phase_becomes_converged_when_all_participants_accept_same_live_proposal() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, ProposalPhase::Negotiating);

        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, ProposalPhase::Negotiating);

        let resp = mode
            .on_message(&session, &env("agent://buyer", "Accept", make_accept("p1")))
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, ProposalPhase::Negotiating);

        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Accept", make_accept("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, ProposalPhase::Converged);
    }

    #[test]
    fn terminal_reject_sets_terminal_rejected_phase() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Reject", make_reject("p1", true)),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, ProposalPhase::TerminalRejected);
    }

    #[test]
    fn malformed_counterproposal_payload_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://seller", "CounterProposal", vec![0xff, 0x00])
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn policy_blocks_counter_proposal_at_submission_when_limit_exceeded() {
        // RFC-MACP-0012: max_rounds is enforced both at submission and commitment time.
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-limited".into(),
            mode: "macp.mode.proposal.v1".into(),
            description: "limited".into(),
            rules: serde_json::json!({
                "counter_proposal": { "max_rounds": 1 }
            }),
            schema_version: 1,
        });
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        // Seller proposes p1
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Buyer counter-proposes p2 (supersedes p1) -- 1st counter-proposal: allowed
        let resp = mode
            .on_message(
                &session,
                &env(
                    "agent://buyer",
                    "CounterProposal",
                    make_counter_proposal("p2", "p1"),
                ),
            )
            .unwrap();
        apply(&mut session, resp);
        // Seller counter-proposes p3 (supersedes p2) -- 2nd counter-proposal:
        // REJECTED at submission time (max_rounds=1, already 1 counter-proposal)
        let err = mode
            .on_message(
                &session,
                &env(
                    "agent://seller",
                    "CounterProposal",
                    make_counter_proposal("p3", "p2"),
                ),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- CounterProposal does NOT retire original ---

    #[test]
    fn counter_proposal_does_not_retire_original() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        // Seller proposes p1
        let resp = mode
            .on_message(
                &session,
                &env("agent://seller", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Buyer counter-proposes p2, superseding p1
        let resp = mode
            .on_message(
                &session,
                &env(
                    "agent://buyer",
                    "CounterProposal",
                    make_counter_proposal("p2", "p1"),
                ),
            )
            .unwrap();
        apply(&mut session, resp);
        // Verify both proposals are still live in the mode state
        let state = decode(&session);
        assert_eq!(state.proposals.len(), 2);
        assert_eq!(state.proposals["p1"].disposition, ProposalDisposition::Live);
        assert_eq!(state.proposals["p2"].disposition, ProposalDisposition::Live);
        assert_eq!(
            state.proposals["p2"].supersedes_proposal_id,
            Some("p1".into())
        );
    }

    #[test]
    fn reject_withdrawn_proposal_fails() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Proposal", make_proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Withdraw p1
        let resp = mode
            .on_message(
                &session,
                &env("agent://buyer", "Withdraw", make_withdraw("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Reject on withdrawn proposal should fail
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://seller", "Reject", make_reject("p1", false))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn withdraw_empty_proposal_id_rejected() {
        let mode = ProposalMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("agent://buyer", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let empty_withdraw = WithdrawPayload {
            proposal_id: String::new(),
            reason: "empty".into(),
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://buyer", "Withdraw", empty_withdraw))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }
}
