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

    /// Resolve the effective approval threshold, considering policy overrides.
    ///
    /// RFC-MACP-0011: "When policy specifies a threshold override, it replaces
    /// (not supplements) the required_approvals value from ApprovalRequest."
    fn effective_threshold(session: &Session, request: &ApprovalRequestRecord) -> u32 {
        if let Some(ref policy) = session.policy_definition {
            let rules: macp_core::policy::rules::QuorumPolicyRules =
                serde_json::from_value(policy.rules.clone()).unwrap_or_default();
            if rules.threshold.value > 0.0 {
                return match rules.threshold.threshold_type.as_str() {
                    "percentage" => {
                        let n = session.participants.len() as f64;
                        (rules.threshold.value / 100.0 * n).ceil() as u32
                    }
                    // "n_of_m" or "count" — use value directly
                    _ => rules.threshold.value as u32,
                };
            }
        }
        request.required_approvals
    }

    fn commitment_ready(session: &Session, state: &QuorumState) -> bool {
        let request = match &state.request {
            Some(request) => request,
            None => return false,
        };
        let required = Self::effective_threshold(session, request);
        let approvals = state
            .ballots
            .values()
            .filter(|ballot| ballot.choice == BallotChoice::Approve)
            .count() as u32;
        let total_eligible = session.participants.len() as u32;
        let counted = state.ballots.len() as u32;
        let remaining = total_eligible.saturating_sub(counted);
        // Commitment is ready if threshold reached OR threshold is mathematically unreachable
        approvals >= required || approvals + remaining < required
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
                state.request = Some(ApprovalRequestRecord {
                    request_id: payload.request_id,
                    action: payload.action,
                    summary: payload.summary,
                    details: payload.details,
                    required_approvals: payload.required_approvals,
                    requested_by: env.sender.clone(),
                });
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
}
