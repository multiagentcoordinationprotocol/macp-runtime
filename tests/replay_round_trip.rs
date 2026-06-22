use macp_runtime::log_store::{EntryKind, LogEntry};
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::{CommitmentPayload, SessionStartPayload};
use macp_runtime::replay::replay_session;
use macp_runtime::session::SessionState;
use prost::Message;

fn make_registry() -> ModeRegistry {
    ModeRegistry::build_default(std::sync::Arc::new(
        macp_runtime::policy::DefaultPolicyEvaluator,
    ))
}

fn start_payload(participants: Vec<&str>) -> Vec<u8> {
    SessionStartPayload {
        intent: "replay-test".into(),
        participants: participants.into_iter().map(String::from).collect(),
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: "policy-1".into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
    }
    .encode_to_vec()
}

fn incoming(
    id: &str,
    msg_type: &str,
    sender: &str,
    payload: Vec<u8>,
    mode: &str,
    ts: i64,
) -> LogEntry {
    LogEntry {
        message_id: id.into(),
        received_at_ms: ts,
        sender: sender.into(),
        message_type: msg_type.into(),
        raw_payload: payload,
        entry_kind: EntryKind::Incoming,
        session_id: "s1".into(),
        mode: mode.into(),
        macp_version: "1.0".into(),
        timestamp_unix_ms: ts,
    }
}

fn commitment(action: &str) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: "c1".into(),
        action: action.into(),
        authority_scope: "test".into(),
        reason: "done".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy-1".into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
    }
    .encode_to_vec()
}

#[test]
fn replay_decision_session() {
    use macp_runtime::decision_pb::{ProposalPayload, VotePayload};

    let registry = make_registry();
    let mode = "macp.mode.decision.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://orchestrator",
            start_payload(vec!["agent://orchestrator", "agent://a", "agent://b"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "Proposal",
            "agent://orchestrator",
            ProposalPayload {
                proposal_id: "p1".into(),
                option: "deploy".into(),
                rationale: "ready".into(),
                supporting_data: vec![],
            }
            .encode_to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "Vote",
            "agent://a",
            VotePayload {
                proposal_id: "p1".into(),
                vote: "approve".into(),
                reason: "good".into(),
            }
            .encode_to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "Commitment",
            "agent://orchestrator",
            commitment("decision.selected"),
            mode,
            4000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
    assert_eq!(session.seen_message_ids.len(), 4);
    let mode_state: serde_json::Value = serde_json::from_slice(&session.mode_state).unwrap();
    assert_eq!(mode_state["phase"], "Committed");
    assert_eq!(mode_state["votes"]["p1"]["agent://a"]["vote"], "APPROVE");
}

#[test]
fn replay_proposal_session() {
    use macp_runtime::proposal_pb::{AcceptPayload, ProposalPayload};

    let registry = make_registry();
    let mode = "macp.mode.proposal.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://buyer",
            start_payload(vec!["agent://buyer", "agent://seller"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "Proposal",
            "agent://seller",
            ProposalPayload {
                proposal_id: "p1".into(),
                title: "offer".into(),
                summary: "terms".into(),
                details: vec![],
                tags: vec![],
            }
            .encode_to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "Accept",
            "agent://buyer",
            AcceptPayload {
                proposal_id: "p1".into(),
                reason: String::new(),
            }
            .encode_to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "Accept",
            "agent://seller",
            AcceptPayload {
                proposal_id: "p1".into(),
                reason: String::new(),
            }
            .encode_to_vec(),
            mode,
            4000,
        ),
        incoming(
            "m5",
            "Commitment",
            "agent://buyer",
            commitment("proposal.accepted"),
            mode,
            5000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
    assert_eq!(session.seen_message_ids.len(), 5);
}

#[test]
fn replay_task_session() {
    use macp_runtime::task_pb::{TaskAcceptPayload, TaskCompletePayload, TaskRequestPayload};

    let registry = make_registry();
    let mode = "macp.mode.task.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://planner",
            start_payload(vec!["agent://planner", "agent://worker"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "TaskRequest",
            "agent://planner",
            TaskRequestPayload {
                task_id: "t1".into(),
                title: "Build".into(),
                instructions: "Do it".into(),
                requested_assignee: "agent://worker".into(),
                input: vec![],
                deadline_unix_ms: 0,
            }
            .encode_to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "TaskAccept",
            "agent://worker",
            TaskAcceptPayload {
                task_id: "t1".into(),
                assignee: "agent://worker".into(),
                reason: "ready".into(),
            }
            .encode_to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "TaskComplete",
            "agent://worker",
            TaskCompletePayload {
                task_id: "t1".into(),
                assignee: "agent://worker".into(),
                output: b"result".to_vec(),
                summary: "done".into(),
            }
            .encode_to_vec(),
            mode,
            4000,
        ),
        incoming(
            "m5",
            "Commitment",
            "agent://planner",
            commitment("task.completed"),
            mode,
            5000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
}

#[test]
fn replay_handoff_session() {
    use macp_runtime::handoff_pb::{HandoffAcceptPayload, HandoffOfferPayload};

    let registry = make_registry();
    let mode = "macp.mode.handoff.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://owner",
            start_payload(vec!["agent://owner", "agent://target"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "HandoffOffer",
            "agent://owner",
            HandoffOfferPayload {
                handoff_id: "h1".into(),
                target_participant: "agent://target".into(),
                scope: "support".into(),
                reason: "escalate".into(),
            }
            .encode_to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "HandoffAccept",
            "agent://target",
            HandoffAcceptPayload {
                handoff_id: "h1".into(),
                accepted_by: "agent://target".into(),
                reason: "ready".into(),
            }
            .encode_to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "Commitment",
            "agent://owner",
            commitment("handoff.accepted"),
            mode,
            4000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
}

#[test]
fn replay_quorum_session() {
    use macp_runtime::quorum_pb::{ApprovalRequestPayload, ApprovePayload};

    let registry = make_registry();
    let mode = "macp.mode.quorum.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://coordinator",
            start_payload(vec!["agent://alice", "agent://bob", "agent://carol"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "ApprovalRequest",
            "agent://coordinator",
            ApprovalRequestPayload {
                request_id: "r1".into(),
                action: "deploy".into(),
                summary: "Deploy v2".into(),
                details: vec![],
                required_approvals: 2,
            }
            .encode_to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "Approve",
            "agent://alice",
            ApprovePayload {
                request_id: "r1".into(),
                reason: "lgtm".into(),
            }
            .encode_to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "Approve",
            "agent://bob",
            ApprovePayload {
                request_id: "r1".into(),
                reason: "ship it".into(),
            }
            .encode_to_vec(),
            mode,
            4000,
        ),
        incoming(
            "m5",
            "Commitment",
            "agent://coordinator",
            commitment("quorum.approved"),
            mode,
            5000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
    assert_eq!(session.seen_message_ids.len(), 5);
}

#[test]
fn replay_multi_round_session() {
    let registry = make_registry();
    let mode = "ext.multi_round.v1";

    let entries = vec![
        incoming(
            "m1",
            "SessionStart",
            "agent://coordinator",
            start_payload(vec!["agent://alice", "agent://bob"]),
            mode,
            1000,
        ),
        incoming(
            "m2",
            "Contribute",
            "agent://alice",
            br#"{"value":"option_a"}"#.to_vec(),
            mode,
            2000,
        ),
        incoming(
            "m3",
            "Contribute",
            "agent://bob",
            br#"{"value":"option_a"}"#.to_vec(),
            mode,
            3000,
        ),
        incoming(
            "m4",
            "Commitment",
            "agent://coordinator",
            commitment("multi_round.converged"),
            mode,
            4000,
        ),
    ];

    let session = replay_session("s1", &entries, &registry, None).unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    assert!(session.resolution.is_some());
    assert_eq!(session.seen_message_ids.len(), 4);
}
