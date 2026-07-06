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

fn make_runtime() -> Arc<Runtime> {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    Arc::new(Runtime::new(storage, registry, log_store))
}

fn session_start(participants: Vec<String>) -> Vec<u8> {
    SessionStartPayload {
        intent: "concurrent-test".into(),
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
async fn concurrent_contributes_all_accepted() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "ext.multi_round.v1";

    // Create participants
    let n = 10;
    let participants: Vec<String> = (0..n).map(|i| format!("agent://p{i}")).collect();

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m0",
            &sid,
            "agent://coordinator",
            session_start(participants.clone()),
        ),
        None,
    )
    .await
    .unwrap();

    // Spawn N concurrent tasks, each sending a Contribute
    let mut handles = Vec::new();
    for i in 0..n {
        let rt = Arc::clone(&rt);
        let sid = sid.clone();
        let sender = format!("agent://p{i}");
        handles.push(tokio::spawn(async move {
            let payload = serde_json::json!({"value": format!("option_{i}")})
                .to_string()
                .into_bytes();
            rt.process(
                &envelope(
                    "ext.multi_round.v1",
                    "Contribute",
                    &format!("m{}", i + 1),
                    &sid,
                    &sender,
                    payload,
                ),
                None,
            )
            .await
        }));
    }

    let mut accepted = 0;
    for handle in handles {
        let result = handle.await.unwrap();
        if result.is_ok() {
            accepted += 1;
        }
    }

    assert_eq!(accepted, n, "All contributions should be accepted");

    // Verify dedup state
    let session = rt.get_session_checked(&sid).await.unwrap();
    // m0 (SessionStart) + N contributes
    assert_eq!(session.seen_message_ids.len(), n + 1);
}

#[tokio::test]
async fn concurrent_duplicate_messages_handled() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m0",
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

    // Send same message concurrently from multiple tasks
    let mut handles = Vec::new();
    for _ in 0..5 {
        let rt = Arc::clone(&rt);
        let sid = sid.clone();
        let proposal = proposal.clone();
        handles.push(tokio::spawn(async move {
            rt.process(
                &envelope(
                    "macp.mode.decision.v1",
                    "Proposal",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    proposal,
                ),
                None,
            )
            .await
        }));
    }

    let mut accepted = 0;
    let mut duplicates = 0;
    for handle in handles {
        if let Ok(result) = handle.await.unwrap() {
            if result.duplicate {
                duplicates += 1;
            } else {
                accepted += 1;
            }
        }
    }

    // Exactly one should be accepted, rest should be duplicates
    assert_eq!(accepted, 1, "Exactly one should be non-duplicate");
    assert_eq!(duplicates, 4, "Rest should be duplicates");
}
