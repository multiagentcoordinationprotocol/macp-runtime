use crate::mode::util::{
    check_commitment_authority, is_declared_participant, validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;
use macp_pb::task_pb::{
    TaskAcceptPayload, TaskCompletePayload, TaskFailPayload, TaskRejectPayload, TaskRequestPayload,
    TaskUpdatePayload,
};
use prost::Message;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub title: String,
    pub instructions: String,
    pub requested_assignee: String,
    pub input: Vec<u8>,
    pub deadline_unix_ms: i64,
    pub requester: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRejectRecord {
    pub task_id: String,
    pub assignee: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskUpdateRecord {
    pub task_id: String,
    pub status: String,
    pub progress: f64,
    pub message: String,
    pub partial_output: Vec<u8>,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCompleteRecord {
    pub task_id: String,
    pub assignee: String,
    pub output: Vec<u8>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFailRecord {
    pub task_id: String,
    pub assignee: String,
    pub error_code: String,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskTerminalReport {
    Complete(TaskCompleteRecord),
    Fail(TaskFailRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskState {
    pub task: Option<TaskRecord>,
    pub active_assignee: Option<String>,
    pub rejections: Vec<TaskRejectRecord>,
    pub updates: Vec<TaskUpdateRecord>,
    pub terminal_report: Option<TaskTerminalReport>,
}

pub struct TaskMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl TaskMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn encode_state(state: &TaskState) -> Vec<u8> {
        serde_json::to_vec(state).expect("TaskState is always serializable")
    }

    fn decode_state(data: &[u8]) -> Result<TaskState, MacpError> {
        serde_json::from_slice(data).map_err(|_| MacpError::InvalidModeState)
    }

    fn ensure_task_matches(expected_task_id: &str, actual_task_id: &str) -> Result<(), MacpError> {
        if expected_task_id.is_empty() || expected_task_id != actual_task_id {
            return Err(MacpError::InvalidPayload);
        }
        Ok(())
    }

    fn can_assignee_respond(session: &Session, task: &TaskRecord, sender: &str) -> bool {
        if !task.requested_assignee.is_empty() {
            sender == task.requested_assignee
        } else {
            is_declared_participant(&session.participants, sender)
                && sender != session.initiator_sender
        }
    }
}

impl Mode for TaskMode {
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
            "TaskRequest" if env.sender == session.initiator_sender => Ok(()),
            "TaskRequest" => Err(MacpError::Forbidden),
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
            &TaskState::default(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            TaskState::default()
        } else {
            Self::decode_state(&session.mode_state)?
        };

        match env.message_type.as_str() {
            "TaskRequest" => {
                if env.sender != session.initiator_sender {
                    return Err(MacpError::Forbidden);
                }
                let payload = TaskRequestPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if state.task.is_some() || payload.task_id.is_empty() {
                    return Err(MacpError::InvalidPayload);
                }
                if !payload.requested_assignee.is_empty()
                    && !is_declared_participant(&session.participants, &payload.requested_assignee)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.task = Some(TaskRecord {
                    task_id: payload.task_id,
                    title: payload.title,
                    instructions: payload.instructions,
                    requested_assignee: payload.requested_assignee,
                    input: payload.input,
                    deadline_unix_ms: payload.deadline_unix_ms,
                    requester: env.sender.clone(),
                });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "TaskAccept" => {
                let payload = TaskAcceptPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let task = state.task.as_ref().ok_or(MacpError::InvalidPayload)?;
                Self::ensure_task_matches(&payload.task_id, &task.task_id)?;
                if state.active_assignee.is_some() {
                    return Err(MacpError::InvalidPayload);
                }
                // RFC-MACP-0012: check allow_reassignment_on_reject if prior rejections exist
                if !state.rejections.is_empty() {
                    let allow = session.policy_definition.as_ref().is_some_and(|p| {
                        serde_json::from_value::<macp_core::policy::rules::TaskPolicyRules>(
                            p.rules.clone(),
                        )
                        .unwrap_or_default()
                        .assignment
                        .allow_reassignment_on_reject
                    });
                    if !allow {
                        return Err(MacpError::PolicyDenied {
                            reasons: vec![
                                "reassignment after rejection not allowed by policy".into()
                            ],
                        });
                    }
                }
                if !payload.assignee.is_empty() && payload.assignee != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if !Self::can_assignee_respond(session, task, &env.sender) {
                    return Err(MacpError::Forbidden);
                }
                state.active_assignee = Some(env.sender.clone());
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "TaskReject" => {
                let payload = TaskRejectPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let task = state.task.as_ref().ok_or(MacpError::InvalidPayload)?;
                Self::ensure_task_matches(&payload.task_id, &task.task_id)?;
                // RFC-MACP-0009 §5.3b: TaskAccept is irrevocable unless policy
                // permits reassignment. §5.3c: when allow_reassignment_on_reject
                // is true, the active assignee may send TaskReject to return the
                // session to the pre-assignment state.
                if let Some(ref active) = state.active_assignee {
                    if active == &env.sender {
                        let allow = session.policy_definition.as_ref().is_some_and(|p| {
                            serde_json::from_value::<macp_core::policy::rules::TaskPolicyRules>(
                                p.rules.clone(),
                            )
                            .unwrap_or_default()
                            .assignment
                            .allow_reassignment_on_reject
                        });
                        if !allow {
                            return Err(MacpError::PolicyDenied {
                                reasons: vec![
                                    "active assignee cannot reject without allow_reassignment_on_reject policy".into(),
                                ],
                            });
                        }
                        // Policy permits: clear assignee, return to pre-assignment state
                    } else {
                        // Someone other than the active assignee trying to reject
                        return Err(MacpError::InvalidPayload);
                    }
                }
                if !payload.assignee.is_empty() && payload.assignee != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if state.active_assignee.is_none()
                    && !Self::can_assignee_respond(session, task, &env.sender)
                {
                    return Err(MacpError::Forbidden);
                }
                if state.rejections.iter().any(|r| r.assignee == env.sender) {
                    return Err(MacpError::InvalidPayload);
                }
                // If active assignee is rejecting with policy permission, clear assignment
                if state.active_assignee.as_deref() == Some(env.sender.as_str()) {
                    state.active_assignee = None;
                }
                state.rejections.push(TaskRejectRecord {
                    task_id: payload.task_id,
                    assignee: env.sender.clone(),
                    reason: payload.reason,
                });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "TaskUpdate" => {
                let payload = TaskUpdatePayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let task = state.task.as_ref().ok_or(MacpError::InvalidPayload)?;
                Self::ensure_task_matches(&payload.task_id, &task.task_id)?;
                if state.terminal_report.is_some()
                    || state.active_assignee.as_deref() != Some(env.sender.as_str())
                {
                    return Err(MacpError::Forbidden);
                }
                state.updates.push(TaskUpdateRecord {
                    task_id: payload.task_id,
                    status: payload.status,
                    progress: payload.progress,
                    message: payload.message,
                    partial_output: payload.partial_output,
                    sender: env.sender.clone(),
                });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "TaskComplete" => {
                let payload = TaskCompletePayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let task = state.task.as_ref().ok_or(MacpError::InvalidPayload)?;
                Self::ensure_task_matches(&payload.task_id, &task.task_id)?;
                if state.terminal_report.is_some()
                    || state.active_assignee.as_deref() != Some(env.sender.as_str())
                {
                    return Err(MacpError::Forbidden);
                }
                if !payload.assignee.is_empty() && payload.assignee != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                state.terminal_report = Some(TaskTerminalReport::Complete(TaskCompleteRecord {
                    task_id: payload.task_id,
                    assignee: env.sender.clone(),
                    output: payload.output,
                    summary: payload.summary,
                }));
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "TaskFail" => {
                let payload = TaskFailPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let task = state.task.as_ref().ok_or(MacpError::InvalidPayload)?;
                Self::ensure_task_matches(&payload.task_id, &task.task_id)?;
                if state.terminal_report.is_some()
                    || state.active_assignee.as_deref() != Some(env.sender.as_str())
                {
                    return Err(MacpError::Forbidden);
                }
                if !payload.assignee.is_empty() && payload.assignee != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                state.terminal_report = Some(TaskTerminalReport::Fail(TaskFailRecord {
                    task_id: payload.task_id,
                    assignee: env.sender.clone(),
                    error_code: payload.error_code,
                    reason: payload.reason,
                    retryable: payload.retryable,
                }));
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                validate_commitment_payload_for_session(session, &env.payload)?;
                if state.terminal_report.is_none() {
                    return Err(MacpError::InvalidPayload);
                }
                // Evaluate governance policy if one is bound to the session.
                if let Some(ref policy) = session.policy_definition {
                    let has_output = matches!(
                        &state.terminal_report,
                        Some(TaskTerminalReport::Complete(record)) if !record.output.is_empty()
                    );
                    let decision = self.evaluator.evaluate_task_commitment(policy, has_output);
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
    use macp_core::session::{Session, SessionState};
    use macp_pb::pb::CommitmentPayload;
    use std::collections::HashSet;

    fn base_session() -> Session {
        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry: i64::MAX,
            ttl_ms: 60_000,
            started_at_unix_ms: 0,
            resolution: None,
            mode: "macp.mode.task.v1".into(),
            mode_state: vec![],
            participants: vec!["planner".into(), "worker".into()],
            seen_message_ids: HashSet::new(),
            intent: String::new(),
            mode_version: "1.0.0".into(),
            configuration_version: "config".into(),
            policy_version: "policy".into(),
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            initiator_sender: "planner".into(),
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: None,
        }
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.task.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 0,
            payload,
        }
    }

    fn commitment_payload() -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "task.completed".into(),
            authority_scope: "ops".into(),
            reason: "done".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
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

    fn make_task_request(task_id: &str, assignee: &str) -> Vec<u8> {
        TaskRequestPayload {
            task_id: task_id.into(),
            title: "Test Task".into(),
            instructions: "Do the thing".into(),
            requested_assignee: assignee.into(),
            input: vec![],
            deadline_unix_ms: 0,
        }
        .encode_to_vec()
    }

    fn make_task_accept(task_id: &str, assignee: &str) -> Vec<u8> {
        TaskAcceptPayload {
            task_id: task_id.into(),
            assignee: assignee.into(),
            reason: "ready".into(),
        }
        .encode_to_vec()
    }

    fn make_task_reject(task_id: &str, assignee: &str) -> Vec<u8> {
        TaskRejectPayload {
            task_id: task_id.into(),
            assignee: assignee.into(),
            reason: "busy".into(),
        }
        .encode_to_vec()
    }

    fn make_task_update(task_id: &str) -> Vec<u8> {
        TaskUpdatePayload {
            task_id: task_id.into(),
            status: "in_progress".into(),
            progress: 0.5,
            message: "halfway done".into(),
            partial_output: vec![],
        }
        .encode_to_vec()
    }

    fn make_task_complete(task_id: &str, assignee: &str) -> Vec<u8> {
        TaskCompletePayload {
            task_id: task_id.into(),
            assignee: assignee.into(),
            output: b"result".to_vec(),
            summary: "done".into(),
        }
        .encode_to_vec()
    }

    fn make_task_fail(task_id: &str, assignee: &str) -> Vec<u8> {
        TaskFailPayload {
            task_id: task_id.into(),
            assignee: assignee.into(),
            error_code: "E001".into(),
            reason: "failed".into(),
            retryable: true,
        }
        .encode_to_vec()
    }

    // --- Session Start ---

    #[test]
    fn session_start_initializes_state() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert!(state.task.is_none());
                assert!(state.active_assignee.is_none());
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn session_start_requires_at_least_two_participants() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["planner".into()]; // only 1
        let err = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn session_start_rejects_empty_participants() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants.clear();
        let err = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn session_start_rejects_when_initiator_not_participant() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["worker".into(), "other".into()]; // planner not included
        let err = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- TaskRequest ---

    #[test]
    fn task_request_from_initiator() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert!(state.task.is_some());
                assert_eq!(state.task.unwrap().task_id, "t1");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn duplicate_task_request_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t2", "worker")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn non_initiator_task_request_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("worker", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- TaskAccept / TaskReject ---

    #[test]
    fn correct_assignee_can_accept() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.active_assignee, Some("worker".into()));
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn wrong_assignee_cannot_accept() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("planner", "TaskAccept", make_task_accept("t1", "planner")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn assignee_can_reject() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskReject", make_task_reject("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.rejections.len(), 1);
                assert!(state.active_assignee.is_none()); // still unassigned
            }
            _ => panic!("Expected PersistState"),
        }
    }

    // --- TaskUpdate ---

    #[test]
    fn active_assignee_can_update() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskUpdate", make_task_update("t1")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.updates.len(), 1);
                assert_eq!(state.updates[0].task_id, "t1");
                assert_eq!(state.updates[0].sender, "worker");
                assert_eq!(state.updates[0].status, "in_progress");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn non_assignee_cannot_update() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("planner", "TaskUpdate", make_task_update("t1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- TaskComplete / TaskFail ---

    #[test]
    fn assignee_can_complete() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert!(matches!(
                    state.terminal_report,
                    Some(TaskTerminalReport::Complete(_))
                ));
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn assignee_can_fail() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskFail", make_task_fail("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert!(matches!(
                    state.terminal_report,
                    Some(TaskTerminalReport::Fail(_))
                ));
            }
            _ => panic!("Expected PersistState"),
        }
    }

    // --- Commitment ---

    #[test]
    fn commitment_after_complete() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_after_fail() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskFail", make_task_fail("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_before_terminal_report_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn non_initiator_commitment_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let commit_env = env("worker", "Commitment", commitment_payload());
        let err = mode.authorize_sender(&session, &commit_env).unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- Full lifecycle ---

    #[test]
    fn full_task_lifecycle() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskUpdate", make_task_update("t1")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Negative outcome commitment ---

    #[test]
    fn negative_outcome_task_failed() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // Send TaskRequest
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Worker accepts
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Worker reports failure
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskFail", make_task_fail("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Commit with negative outcome
        let negative_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "task.failed".into(),
            authority_scope: "ops".into(),
            reason: "task failed".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: false,
        }
        .encode_to_vec();
        let result = mode
            .on_message(&session, &env("planner", "Commitment", negative_commitment))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Duplicate rejection ---

    #[test]
    fn duplicate_rejection_from_same_sender_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["planner".into(), "w1".into(), "w2".into()];
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("w1", "TaskReject", make_task_reject("t1", "w1")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("w1", "TaskReject", make_task_reject("t1", "w1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn different_senders_can_both_reject() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["planner".into(), "w1".into(), "w2".into()];
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("w1", "TaskReject", make_task_reject("t1", "w1")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("w2", "TaskReject", make_task_reject("t1", "w2")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.rejections.len(), 2);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    // --- Commitment version mismatch ---

    #[test]
    fn commitment_version_mismatch_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let bad_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "task.completed".into(),
            authority_scope: "ops".into(),
            reason: "done".into(),
            mode_version: "wrong".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("planner", "Commitment", bad_commitment))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Unknown message type ---

    #[test]
    fn unknown_message_type_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("worker", "CustomType", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Policy ---

    #[test]
    fn policy_allows_commitment_when_output_present() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-strict".into(),
            mode: "macp.mode.task.v1".into(),
            description: "strict".into(),
            rules: serde_json::json!({ "completion": { "require_output": true } }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Commitment should succeed: output is required and task completion has output
        let result = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn policy_with_no_output_requirement_allows_commitment() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-permissive".into(),
            mode: "macp.mode.task.v1".into(),
            description: "permissive".into(),
            rules: serde_json::json!({ "completion": { "require_output": false } }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskComplete", make_task_complete("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Commitment should succeed: require_output is false
        let result = mode
            .on_message(
                &session,
                &env("planner", "Commitment", commitment_payload()),
            )
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Competing TaskAccept ---

    #[test]
    fn competing_task_accept_second_rejected() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // Three participants: planner (initiator), w1, w2
        session.participants = vec!["planner".into(), "w1".into(), "w2".into()];
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // Open-assignee task (no specific requested_assignee)
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "")),
            )
            .unwrap();
        apply(&mut session, result);
        // First accept from w1 succeeds
        let result = mode
            .on_message(
                &session,
                &env("w1", "TaskAccept", make_task_accept("t1", "w1")),
            )
            .unwrap();
        apply(&mut session, result);
        let state: TaskState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(state.active_assignee, Some("w1".into()));
        // Second accept from w2 is rejected (active_assignee already set)
        let err = mode
            .on_message(
                &session,
                &env("w2", "TaskAccept", make_task_accept("t1", "w2")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Only active assignee can send TaskUpdate ---

    #[test]
    fn only_active_assignee_can_send_task_update() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["planner".into(), "agentA".into(), "agentB".into()];
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // Open-assignee task
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "")),
            )
            .unwrap();
        apply(&mut session, result);
        // agentA accepts the task
        let result = mode
            .on_message(
                &session,
                &env("agentA", "TaskAccept", make_task_accept("t1", "agentA")),
            )
            .unwrap();
        apply(&mut session, result);
        // agentB (non-assignee) attempts to send TaskUpdate — expect Forbidden
        let err = mode
            .on_message(
                &session,
                &env("agentB", "TaskUpdate", make_task_update("t1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- TaskReject with reassignment policy (RFC-MACP-0009 §5.3c) ---

    #[test]
    fn active_assignee_can_reject_with_reassignment_policy() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test".into(),
            mode: "macp.mode.task.v1".into(),
            description: "allows reassignment".into(),
            rules: serde_json::json!({
                "assignment": { "allow_reassignment_on_reject": true },
                "completion": { "require_output": false }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Active assignee rejects with policy permission — returns to pre-assignment
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskReject", make_task_reject("t1", "worker")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: TaskState = serde_json::from_slice(&data).unwrap();
                assert!(
                    state.active_assignee.is_none(),
                    "should clear active assignee"
                );
                assert_eq!(state.rejections.len(), 1);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn active_assignee_cannot_reject_without_reassignment_policy() {
        let mode = TaskMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // No policy = no reassignment
        let result = mode
            .on_session_start(&session, &env("planner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("planner", "TaskRequest", make_task_request("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("worker", "TaskAccept", make_task_accept("t1", "worker")),
            )
            .unwrap();
        apply(&mut session, result);
        // Active assignee rejects without policy permission — denied
        let err = mode
            .on_message(
                &session,
                &env("worker", "TaskReject", make_task_reject("t1", "worker")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "PolicyDenied");
    }
}
