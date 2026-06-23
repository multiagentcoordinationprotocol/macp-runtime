use macp_runtime::log_store::LogStore;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::SessionState;
use macp_runtime::storage::FileBackend;
use prost::Message;
use std::sync::Arc;

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn session_start(participants: Vec<String>) -> Vec<u8> {
    SessionStartPayload {
        intent: "file-backend-test".into(),
        participants,
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: String::new(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
    }
    .encode_to_vec()
}

fn envelope(
    mode: &str,
    message_type: &str,
    message_id: &str,
    session_id: &str,
    sender: &str,
    payload: Vec<u8>,
) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: mode.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: session_id.into(),
        sender: sender.into(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload,
    }
}

fn commitment(action: &str) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: "c1".into(),
        action: action.into(),
        authority_scope: "test".into(),
        reason: "done".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy.default".into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

#[tokio::test]
async fn file_backend_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let rt = Runtime::new(Arc::clone(&storage), registry, log_store);

    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://orchestrator",
            session_start(vec!["agent://orchestrator".into(), "agent://a".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    let proposal = macp_runtime::decision_pb::ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    rt.process(
        &envelope(
            mode,
            "Proposal",
            "m2",
            &sid,
            "agent://orchestrator",
            proposal,
        ),
        None,
    )
    .await
    .unwrap();

    let vote = macp_runtime::decision_pb::VotePayload {
        proposal_id: "p1".into(),
        vote: "approve".into(),
        reason: "good".into(),
    }
    .encode_to_vec();
    rt.process(&envelope(mode, "Vote", "m3", &sid, "agent://a", vote), None)
        .await
        .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m4",
                &sid,
                "agent://orchestrator",
                commitment("decision.selected"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);

    // After resolution, the log is compacted to a single checkpoint entry
    let log = storage.load_log(&sid).await.unwrap();
    assert!(
        !log.is_empty(),
        "log should have at least one entry after compaction"
    );
    // Verify the session can be replayed from the compacted log
    let replayed = macp_runtime::replay::replay_session(
        &sid,
        &log,
        &macp_runtime::mode_registry::ModeRegistry::build_default(std::sync::Arc::new(
            macp_runtime::policy::DefaultPolicyEvaluator,
        )),
        None,
    )
    .unwrap();
    assert_eq!(replayed.state, SessionState::Resolved);
}

#[tokio::test]
async fn file_backend_crash_recovery_via_replay() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let rt = Runtime::new(Arc::clone(&storage), registry, log_store);

    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://orchestrator",
            session_start(vec!["agent://orchestrator".into(), "agent://a".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    let proposal = macp_runtime::decision_pb::ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    rt.process(
        &envelope(
            mode,
            "Proposal",
            "m2",
            &sid,
            "agent://orchestrator",
            proposal,
        ),
        None,
    )
    .await
    .unwrap();

    // "Crash": discard in-memory state, replay from disk
    drop(rt);

    let log_entries = storage.load_log(&sid).await.unwrap();
    assert_eq!(log_entries.len(), 2);

    let mode_registry = ModeRegistry::build_default(std::sync::Arc::new(
        macp_runtime::policy::DefaultPolicyEvaluator,
    ));
    let session = replay_session(&sid, &log_entries, &mode_registry, None).unwrap();
    assert_eq!(session.state, SessionState::Open);
    assert_eq!(session.seen_message_ids.len(), 2);
    assert!(session.seen_message_ids.contains("m1"));
    assert!(session.seen_message_ids.contains("m2"));
}
