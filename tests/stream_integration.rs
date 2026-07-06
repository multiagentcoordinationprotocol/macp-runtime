use macp_runtime::log_store::LogStore;
use macp_runtime::pb::{Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::storage::MemoryBackend;
use prost::Message;
use std::sync::Arc;

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn make_runtime() -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    Runtime::new(storage, registry, log_store)
}

fn session_start(participants: Vec<String>) -> Vec<u8> {
    SessionStartPayload {
        intent: "stream-test".into(),
        participants,
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: String::new(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
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

#[tokio::test]
async fn stream_receives_accepted_envelopes() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    let mut rx = rt.subscribe_session_stream(&sid);

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

    let env = rx.recv().await.unwrap();
    assert_eq!(env.message_id, "m1");
    assert_eq!(env.message_type, "SessionStart");
}

#[tokio::test]
async fn stream_ordering_matches_processing_order() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    let mut rx = rt.subscribe_session_stream(&sid);

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

    let first = rx.recv().await.unwrap();
    let second = rx.recv().await.unwrap();
    assert_eq!(first.message_id, "m1");
    assert_eq!(second.message_id, "m2");
}

#[tokio::test]
async fn concurrent_subscribers_both_receive_events() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    let mut rx1 = rt.subscribe_session_stream(&sid);
    let mut rx2 = rt.subscribe_session_stream(&sid);

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

    let env1 = rx1.recv().await.unwrap();
    let env2 = rx2.recv().await.unwrap();
    assert_eq!(env1.message_id, "m1");
    assert_eq!(env2.message_id, "m1");
}
