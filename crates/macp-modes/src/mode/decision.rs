use crate::mode::util::{
    check_commitment_authority, is_declared_participant, validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::decision_pb::{EvaluationPayload, ObjectionPayload, ProposalPayload, VotePayload};
use macp_pb::pb::Envelope;
use prost::Message;
use std::collections::BTreeMap;

// The Decision mode's domain types now live in `macp-core` (the policy
// evaluation trait names them). Re-exported so `crate::mode::decision::*` and
// downstream `macp_runtime::mode::decision::*` paths keep resolving.
pub use macp_core::decision::{
    DecisionPhase, DecisionState, Evaluation, Objection, Proposal, Vote,
};

pub struct DecisionMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl DecisionMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn default_state() -> DecisionState {
        DecisionState {
            proposals: BTreeMap::new(),
            evaluations: Vec::new(),
            objections: Vec::new(),
            votes: BTreeMap::new(),
            phase: DecisionPhase::Proposal,
        }
    }

    fn encode_state(state: &DecisionState) -> Vec<u8> {
        serde_json::to_vec(state).expect("DecisionState is always serializable")
    }

    fn decode_state(data: &[u8]) -> Result<DecisionState, MacpError> {
        serde_json::from_slice(data).map_err(|_| MacpError::InvalidModeState)
    }

    fn ensure_not_committed(state: &DecisionState) -> Result<(), MacpError> {
        if state.phase == DecisionPhase::Committed {
            Err(MacpError::SessionNotOpen)
        } else {
            Ok(())
        }
    }

    fn ensure_known_proposal(state: &DecisionState, proposal_id: &str) -> Result<(), MacpError> {
        if !state.proposals.contains_key(proposal_id) {
            return Err(MacpError::InvalidPayload);
        }
        Ok(())
    }

    fn ensure_can_propose(state: &DecisionState) -> Result<(), MacpError> {
        Self::ensure_not_committed(state)?;
        if state.phase == DecisionPhase::Voting {
            return Err(MacpError::InvalidPayload);
        }
        Ok(())
    }

    fn ensure_can_deliberate(state: &DecisionState) -> Result<(), MacpError> {
        Self::ensure_not_committed(state)?;
        // RFC-MACP-0007 §5 does not require Evaluations/Objections to strictly
        // precede Votes — it only requires that they reference an existing
        // proposal and that the session is not yet committed. Autonomous
        // participants deliberate and vote concurrently, so an Evaluation or
        // Objection can legitimately arrive after another participant's Vote has
        // advanced the phase to Voting. Accept deliberation in both the
        // Evaluation and Voting phases (symmetric with `ensure_can_vote`, which
        // already accepts Votes during the Evaluation phase). Governance is
        // unaffected: the `evaluation.required_before_voting` /
        // `objection_handling` policy gates are still enforced against the full
        // accumulated state at commitment time.
        if matches!(
            state.phase,
            DecisionPhase::Proposal | DecisionPhase::Committed
        ) {
            return Err(MacpError::InvalidPayload);
        }
        Ok(())
    }

    fn ensure_can_vote(state: &DecisionState) -> Result<(), MacpError> {
        Self::ensure_not_committed(state)?;
        if matches!(
            state.phase,
            DecisionPhase::Proposal | DecisionPhase::Committed
        ) {
            return Err(MacpError::InvalidPayload);
        }
        Ok(())
    }

    fn commitment_ready(state: &DecisionState) -> bool {
        !state.proposals.is_empty()
    }
}

impl Mode for DecisionMode {
    /// Authorize the sender for decision mode messages.
    ///
    /// Authority matrix (RFC-MACP-0004):
    /// - Proposal, Evaluation, Objection, Vote → declared participant only
    /// - Commitment → initiator or policy-delegated role
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
            "Commitment" => check_commitment_authority(session, &env.sender),
            "Proposal" | "Evaluation" | "Objection" | "Vote"
                if is_declared_participant(&session.participants, &env.sender) =>
            {
                Ok(())
            }
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
            &Self::default_state(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            Self::default_state()
        } else {
            Self::decode_state(&session.mode_state)?
        };

        Self::ensure_not_committed(&state)?;

        match env.message_type.as_str() {
            "Proposal" => {
                Self::ensure_can_propose(&state)?;
                let payload = ProposalPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if payload.proposal_id.trim().is_empty()
                    || payload.option.trim().is_empty()
                    || state.proposals.contains_key(&payload.proposal_id)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.proposals.insert(
                    payload.proposal_id.clone(),
                    Proposal {
                        proposal_id: payload.proposal_id,
                        option: payload.option,
                        rationale: payload.rationale,
                        sender: env.sender.clone(),
                    },
                );
                state.phase = DecisionPhase::Evaluation;
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Evaluation" => {
                let payload = EvaluationPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0004: valid recommendation values
                match payload.recommendation.to_uppercase().as_str() {
                    "APPROVE" | "REVIEW" | "BLOCK" | "REJECT" => {}
                    _ => return Err(MacpError::InvalidPayload),
                }
                // RFC-MACP-0004 §2.2: confidence must be a normalized value in [0.0, 1.0]
                if payload.confidence < 0.0 || payload.confidence > 1.0 {
                    return Err(MacpError::InvalidPayload);
                }
                Self::ensure_can_deliberate(&state)?;
                Self::ensure_known_proposal(&state, &payload.proposal_id)?;
                // RFC-MACP-0007 §4: all enum-like values MUST be stored in UPPER_CASE
                let normalized_recommendation = payload.recommendation.to_uppercase();
                state.evaluations.push(Evaluation {
                    proposal_id: payload.proposal_id,
                    recommendation: normalized_recommendation,
                    confidence: payload.confidence,
                    reason: payload.reason,
                    sender: env.sender.clone(),
                });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Objection" => {
                let payload = ObjectionPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0004 §2.3: severity must be one of {critical, high, medium, low}
                let severity = if payload.severity.is_empty() {
                    "medium".into()
                } else {
                    match payload.severity.to_lowercase().as_str() {
                        "critical" | "high" | "medium" | "low" => payload.severity.to_lowercase(),
                        _ => return Err(MacpError::InvalidPayload),
                    }
                };
                Self::ensure_can_deliberate(&state)?;
                Self::ensure_known_proposal(&state, &payload.proposal_id)?;
                state.objections.push(Objection {
                    proposal_id: payload.proposal_id,
                    reason: payload.reason,
                    severity,
                    sender: env.sender.clone(),
                });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Vote" => {
                let payload =
                    VotePayload::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0007: valid vote values (case-insensitive input, stored UPPERCASE)
                let normalized_vote = payload.vote.to_uppercase();
                match normalized_vote.as_str() {
                    "APPROVE" | "REJECT" | "ABSTAIN" => {}
                    _ => return Err(MacpError::InvalidPayload),
                }
                Self::ensure_can_vote(&state)?;
                Self::ensure_known_proposal(&state, &payload.proposal_id)?;
                let proposal_votes = state.votes.entry(payload.proposal_id.clone()).or_default();
                if proposal_votes.contains_key(&env.sender) {
                    return Err(MacpError::InvalidPayload);
                }
                proposal_votes.insert(
                    env.sender.clone(),
                    Vote {
                        proposal_id: payload.proposal_id,
                        vote: normalized_vote,
                        reason: payload.reason,
                        sender: env.sender.clone(),
                    },
                );
                state.phase = DecisionPhase::Voting;
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                validate_commitment_payload_for_session(session, &env.payload)?;
                if !Self::commitment_ready(&state) {
                    return Err(MacpError::InvalidPayload);
                }
                // Evaluate governance policy if one is bound to the session.
                if let Some(ref policy) = session.policy_definition {
                    let decision = self.evaluator.evaluate_decision_commitment(
                        policy,
                        &state,
                        &session.participants,
                    );
                    if let macp_core::policy::PolicyDecision::Deny { reasons } = decision {
                        tracing::warn!(
                            session_id = %session.session_id,
                            policy_id = %policy.policy_id,
                            reasons = ?reasons,
                            "policy denied commitment"
                        );
                        return Err(MacpError::PolicyDenied { reasons });
                    }
                }
                state.phase = DecisionPhase::Committed;
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
    use macp_core::session::SessionState;
    use macp_pb::pb::CommitmentPayload;
    use std::collections::HashSet;

    fn test_session() -> Session {
        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry: i64::MAX,
            ttl_ms: 60_000,
            started_at_unix_ms: 0,
            resolution: None,
            mode: "macp.mode.decision.v1".into(),
            mode_state: vec![],
            participants: vec![
                "agent://orchestrator".into(),
                "agent://fraud".into(),
                "agent://growth".into(),
            ],
            seen_message_ids: HashSet::new(),
            intent: String::new(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: "policy-1".into(),
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            initiator_sender: "agent://orchestrator".into(),
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: None,
            suspended_at_ms: None,
            accumulated_suspended_ms: 0,
        }
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 0,
            payload,
        }
    }

    fn proposal(id: &str) -> Vec<u8> {
        ProposalPayload {
            proposal_id: id.into(),
            option: format!("option-{id}"),
            rationale: "because".into(),
            supporting_data: vec![],
        }
        .encode_to_vec()
    }

    fn vote(id: &str, value: &str) -> Vec<u8> {
        VotePayload {
            proposal_id: id.into(),
            vote: value.into(),
            reason: String::new(),
        }
        .encode_to_vec()
    }

    fn evaluation(proposal_id: &str) -> Vec<u8> {
        EvaluationPayload {
            proposal_id: proposal_id.into(),
            recommendation: "APPROVE".into(),
            confidence: 0.9,
            reason: "good".into(),
        }
        .encode_to_vec()
    }

    fn objection(proposal_id: &str) -> Vec<u8> {
        ObjectionPayload {
            proposal_id: proposal_id.into(),
            reason: "risky".into(),
            severity: "high".into(),
        }
        .encode_to_vec()
    }

    fn commitment(session: &Session) -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.selected".into(),
            authority_scope: "payments".into(),
            reason: "bound".into(),
            mode_version: session.mode_version.clone(),
            policy_version: session.policy_version.clone(),
            configuration_version: session.configuration_version.clone(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec()
    }

    fn apply(session: &mut Session, response: ModeResponse) {
        match response {
            ModeResponse::PersistState(data) => session.mode_state = data,
            ModeResponse::PersistAndResolve { state, .. } => session.mode_state = state,
            _ => {}
        }
    }

    fn decode(session: &Session) -> DecisionState {
        serde_json::from_slice(&session.mode_state).unwrap()
    }

    #[test]
    fn session_start_requires_declared_participants() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session.participants.clear();
        assert_eq!(
            mode.on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![])
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn initiator_not_in_participants_cannot_propose() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session.participants.retain(|p| p != "agent://orchestrator");
        let err = mode
            .authorize_sender(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn vote_with_invalid_value_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session
            .participants
            .push("agent://orchestrator".to_string());
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad_vote = VotePayload {
            proposal_id: "p1".into(),
            vote: "maybe".into(),
            reason: String::new(),
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("agent://fraud", "Vote", bad_vote))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn abstain_vote_accepted() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session
            .participants
            .push("agent://orchestrator".to_string());
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        mode.on_message(
            &session,
            &env("agent://fraud", "Vote", vote("p1", "abstain")),
        )
        .unwrap();
    }

    #[test]
    fn evaluation_with_invalid_recommendation_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session
            .participants
            .push("agent://orchestrator".to_string());
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad_eval = EvaluationPayload {
            proposal_id: "p1".into(),
            recommendation: "meh".into(),
            confidence: 0.5,
            reason: "unclear".into(),
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("agent://fraud", "Evaluation", bad_eval))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn duplicate_proposal_id_is_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(&session, &env("agent://fraud", "Proposal", proposal("p1")))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn vote_is_scoped_per_proposal() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(&session, &env("agent://fraud", "Proposal", proposal("p2")))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p2", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "reject"))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn evaluation_before_any_proposal_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://fraud", "Evaluation", evaluation("p1"))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn objection_before_any_proposal_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://fraud", "Objection", objection("p1"))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn vote_before_any_proposal_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve"))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn proposal_after_voting_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p2"))
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn evaluation_after_voting_accepted() {
        // RFC-MACP-0007 §5 imposes no ordering between Evaluation and Vote, so
        // an Evaluation arriving after voting has begun (phase == Voting) is
        // accepted and recorded. The `required_before_voting` policy gate is
        // enforced separately at commitment time against the accumulated state.
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://growth", "Evaluation", evaluation("p1")),
            )
            .expect("evaluation during voting phase should be accepted");
        apply(&mut session, resp);
        let state = decode(&session);
        assert!(
            state
                .evaluations
                .iter()
                .any(|e| e.sender == "agent://growth"),
            "late evaluation should be recorded"
        );
    }

    #[test]
    fn objection_after_voting_accepted() {
        // RFC-MACP-0007 §5 imposes no ordering between Objection and Vote, so an
        // Objection raised after voting has begun (phase == Voting) is accepted
        // and recorded. `objection_handling` veto rules are applied at
        // commitment time against the accumulated state.
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://growth", "Objection", objection("p1")),
            )
            .expect("objection during voting phase should be accepted");
        apply(&mut session, resp);
        let state = decode(&session);
        assert!(
            state
                .objections
                .iter()
                .any(|o| o.sender == "agent://growth"),
            "late objection should be recorded"
        );
    }

    #[test]
    fn commitment_from_non_initiator_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.authorize_sender(
                &session,
                &env("agent://fraud", "Commitment", commitment(&session))
            )
            .unwrap_err()
            .to_string(),
            "Forbidden"
        );
    }

    #[test]
    fn empty_proposal_id_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let empty_proposal = ProposalPayload {
            proposal_id: "".into(),
            option: "option".into(),
            rationale: "because".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://orchestrator", "Proposal", empty_proposal)
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn malformed_vote_payload_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(
            mode.on_message(&session, &env("agent://fraud", "Vote", vec![0xff, 0x00]))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn phase_advances_from_proposal_to_voting_to_committed() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, DecisionPhase::Proposal);

        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, DecisionPhase::Evaluation);

        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, DecisionPhase::Voting);

        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Commitment", commitment(&session)),
            )
            .unwrap();
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, DecisionPhase::Committed);
    }

    #[test]
    fn commitment_versions_must_match_session_bindings() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);

        let mut bad = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.selected".into(),
            authority_scope: "payments".into(),
            reason: "bound".into(),
            mode_version: "wrong".into(),
            policy_version: session.policy_version.clone(),
            configuration_version: session.configuration_version.clone(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();

        assert_eq!(
            mode.on_message(
                &session,
                &env("agent://orchestrator", "Commitment", bad.clone())
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload"
        );

        bad = commitment(&session);
        mode.on_message(&session, &env("agent://orchestrator", "Commitment", bad))
            .unwrap();
    }

    #[test]
    fn negative_outcome_commitment_succeeds() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        // Add a proposal
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Add a vote (reject)
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "reject")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Commit with negative outcome
        let negative_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.rejected".into(),
            authority_scope: "payments".into(),
            reason: "proposal rejected by voters".into(),
            mode_version: session.mode_version.clone(),
            policy_version: session.policy_version.clone(),
            configuration_version: session.configuration_version.clone(),
            outcome_positive: false,
            supersedes: None,
        }
        .encode_to_vec();
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Commitment", negative_commitment),
            )
            .unwrap();
        assert!(matches!(resp, ModeResponse::PersistAndResolve { .. }));
        apply(&mut session, resp);
        assert_eq!(decode(&session).phase, DecisionPhase::Committed);
    }

    #[test]
    fn policy_denies_commitment_when_vote_threshold_not_met() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-strict".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "strict".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "unanimous" }
            }),
            schema_version: 1,
        });
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Only one of the participants votes approve
        let resp = mode
            .on_message(
                &session,
                &env("agent://fraud", "Vote", vote("p1", "approve")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Commitment should be denied by policy (unanimous requires all participants)
        let err = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Commitment", commitment(&session)),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "PolicyDenied");
    }

    // A consumer-supplied evaluator that denies every decision commitment,
    // regardless of votes or rules. Used to prove that modes consult the
    // injected `PolicyEvaluator` rather than any built-in default — the seam
    // the workspace split exists to provide.
    struct DenyAllEvaluator;

    impl macp_core::policy::PolicyEvaluator for DenyAllEvaluator {
        fn evaluate_decision_commitment(
            &self,
            _policy: &macp_core::policy::PolicyDefinition,
            _state: &DecisionState,
            _participants: &[String],
        ) -> macp_core::policy::PolicyDecision {
            macp_core::policy::PolicyDecision::Deny {
                reasons: vec!["custom evaluator denies all".into()],
            }
        }

        fn evaluate_proposal_commitment(
            &self,
            _policy: &macp_core::policy::PolicyDefinition,
            _counter_proposal_count: usize,
        ) -> macp_core::policy::PolicyDecision {
            macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
        }

        fn evaluate_task_commitment(
            &self,
            _policy: &macp_core::policy::PolicyDefinition,
            _has_output: bool,
        ) -> macp_core::policy::PolicyDecision {
            macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
        }

        fn evaluate_handoff_commitment(
            &self,
            _policy: &macp_core::policy::PolicyDefinition,
        ) -> macp_core::policy::PolicyDecision {
            macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
        }

        fn evaluate_quorum_commitment(
            &self,
            _policy: &macp_core::policy::PolicyDefinition,
            _approve_count: usize,
            _reject_count: usize,
            _abstain_count: usize,
            _total_participants: usize,
        ) -> macp_core::policy::PolicyDecision {
            macp_core::policy::PolicyDecision::Allow { reasons: vec![] }
        }
    }

    // Drive a decision session up to (but not including) the commitment, using a
    // permissive policy the default evaluator would accept. Returns the session
    // and the commitment envelope so a test can swap evaluators and assert that
    // only the injected one changes the outcome.
    fn session_ready_for_commitment(mode: &DecisionMode) -> (Session, Envelope) {
        let mut session = test_session();
        // A policy with no additional voting constraints: the default evaluator
        // allows the commitment once a proposal exists, so any denial below can
        // only come from the injected evaluator.
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "permissive".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "no extra constraints".into(),
            rules: serde_json::json!({}),
            schema_version: 1,
        });
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let commit = env("agent://orchestrator", "Commitment", commitment(&session));
        (session, commit)
    }

    #[test]
    fn injected_evaluator_governs_commitment_outcome() {
        // Same permissive policy, same session timeline — only the injected
        // evaluator differs. This is the pluggable-policy seam introduced by the
        // workspace split: modes call through `macp_core::PolicyEvaluator`.

        // Default evaluator: permissive policy allows the commitment.
        let default_mode =
            DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let (session, commit) = session_ready_for_commitment(&default_mode);
        let resp = default_mode.on_message(&session, &commit).unwrap();
        assert!(matches!(resp, ModeResponse::PersistAndResolve { .. }));

        // Custom evaluator: identical inputs, but the consumer's policy denies.
        let custom_mode = DecisionMode::new(std::sync::Arc::new(DenyAllEvaluator));
        let (session, commit) = session_ready_for_commitment(&custom_mode);
        let err = custom_mode.on_message(&session, &commit).unwrap_err();
        assert_eq!(err.to_string(), "PolicyDenied");
    }

    #[test]
    fn evaluation_confidence_out_of_bounds_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad_eval = EvaluationPayload {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: 1.5,
            reason: "too confident".into(),
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://fraud", "Evaluation", bad_eval))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn evaluation_confidence_negative_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad_eval = EvaluationPayload {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: -0.1,
            reason: "negative".into(),
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://fraud", "Evaluation", bad_eval))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn evaluation_confidence_boundary_accepted() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        // confidence=0.0 should be accepted
        let eval_zero = EvaluationPayload {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: 0.0,
            reason: "zero".into(),
        }
        .encode_to_vec();
        let resp = mode
            .on_message(&session, &env("agent://fraud", "Evaluation", eval_zero))
            .unwrap();
        apply(&mut session, resp);
        // confidence=1.0 should be accepted
        let eval_one = EvaluationPayload {
            proposal_id: "p1".into(),
            recommendation: "REVIEW".into(),
            confidence: 1.0,
            reason: "one".into(),
        }
        .encode_to_vec();
        mode.on_message(&session, &env("agent://growth", "Evaluation", eval_one))
            .unwrap();
    }

    #[test]
    fn objection_invalid_severity_rejected() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let bad_objection = ObjectionPayload {
            proposal_id: "p1".into(),
            reason: "bad".into(),
            severity: "urgent".into(),
        }
        .encode_to_vec();
        assert_eq!(
            mode.on_message(&session, &env("agent://fraud", "Objection", bad_objection))
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn objection_valid_severities_accepted() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        for severity in &["critical", "high", "medium", "low"] {
            let obj = ObjectionPayload {
                proposal_id: "p1".into(),
                reason: "reason".into(),
                severity: severity.to_string(),
            }
            .encode_to_vec();
            let resp = mode
                .on_message(&session, &env("agent://fraud", "Objection", obj))
                .unwrap();
            apply(&mut session, resp);
        }
    }

    #[test]
    fn objection_severity_case_normalized() {
        let mode = DecisionMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = test_session();
        let resp = mode
            .on_session_start(
                &session,
                &env("agent://orchestrator", "SessionStart", vec![]),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("agent://orchestrator", "Proposal", proposal("p1")),
            )
            .unwrap();
        apply(&mut session, resp);
        let obj = ObjectionPayload {
            proposal_id: "p1".into(),
            reason: "reason".into(),
            severity: "CRITICAL".into(),
        }
        .encode_to_vec();
        let resp = mode
            .on_message(&session, &env("agent://fraud", "Objection", obj))
            .unwrap();
        apply(&mut session, resp);
        let state = decode(&session);
        assert_eq!(state.objections[0].severity, "critical");
    }
}
