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

// ---------------------------------------------------------------------------
// RFC-MACP-0010 §5.1(2) — the synthetic implicit accept on the wire.
// ---------------------------------------------------------------------------
//
// The synthetic accept is `EntryKind::Incoming`, so unlike the runtime's
// internal lifecycle entries it is an *accepted* envelope and reaches
// `StreamSession` subscribers like any other. That is one of the two honest
// deltas the freeze-profile carve-out names (the other being that it consumes
// an accepted ordinal), so it is pinned here rather than left implicit.

const HANDOFF_MODE: &str = "macp.mode.handoff.v1";
const HANDOFF_OWNER: &str = "agent://owner";
const HANDOFF_TARGET: &str = "agent://target";
const HANDOFF_POLICY: &str = "handoff-auto-accept";
const HANDOFF_TIMEOUT_MS: i64 = 60;

fn handoff_runtime() -> Runtime {
    let rt = make_runtime();
    rt.register_policy(macp_runtime::macp_core::policy::PolicyDefinition {
        policy_id: HANDOFF_POLICY.into(),
        mode: HANDOFF_MODE.into(),
        description: "implicit accept after a short timeout".into(),
        rules: serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": HANDOFF_TIMEOUT_MS },
            "commitment": { "authority": "initiator_only" }
        }),
        schema_version: 1,
    })
    .expect("policy registers");
    rt
}

fn handoff_start() -> Vec<u8> {
    SessionStartPayload {
        intent: "escalate".into(),
        participants: vec![HANDOFF_OWNER.into(), HANDOFF_TARGET.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: HANDOFF_POLICY.into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

/// Criterion 8: a subscriber sees the synthetic envelope in emission order —
/// after the last message accepted before the deadline, and before the trigger
/// that provoked it.
#[tokio::test]
async fn stream_subscribers_see_the_synthetic_envelope_in_order() {
    let rt = handoff_runtime();
    let sid = new_sid();
    let mut rx = rt.subscribe_session_stream(&sid);

    rt.process(
        &envelope(
            HANDOFF_MODE,
            "SessionStart",
            "start-1",
            &sid,
            HANDOFF_OWNER,
            handoff_start(),
        ),
        None,
    )
    .await
    .unwrap();
    rt.process(
        &envelope(
            HANDOFF_MODE,
            "HandoffOffer",
            "offer-1",
            &sid,
            HANDOFF_OWNER,
            macp_runtime::handoff_pb::HandoffOfferPayload {
                handoff_id: "h1".into(),
                target_participant: HANDOFF_TARGET.into(),
                scope: "support".into(),
                reason: "escalate".into(),
            }
            .encode_to_vec(),
        ),
        None,
    )
    .await
    .unwrap();
    // The pre-deadline message the synthetic must come *after*.
    rt.process(
        &envelope(
            HANDOFF_MODE,
            "HandoffContext",
            "ctx-1",
            &sid,
            HANDOFF_OWNER,
            macp_runtime::handoff_pb::HandoffContextPayload {
                handoff_id: "h1".into(),
                content_type: "text/plain".into(),
                context: b"background".to_vec(),
            }
            .encode_to_vec(),
        ),
        None,
    )
    .await
    .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(
        HANDOFF_TIMEOUT_MS as u64 + 40,
    ))
    .await;

    rt.process(
        &envelope(
            HANDOFF_MODE,
            "Commitment",
            "commit-1",
            &sid,
            HANDOFF_OWNER,
            macp_runtime::pb::CommitmentPayload {
                commitment_id: "c1".into(),
                action: "handoff.accepted".into(),
                authority_scope: "support".into(),
                reason: "bound".into(),
                mode_version: "1.0.0".into(),
                policy_version: HANDOFF_POLICY.into(),
                configuration_version: "cfg-1".into(),
                outcome_positive: true,
                supersedes: None,
            }
            .encode_to_vec(),
        ),
        None,
    )
    .await
    .expect("resolves");

    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(rx.recv().await.expect("subscriber must not lag"));
    }
    let ids: Vec<&str> = seen.iter().map(|e| e.message_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["start-1", "offer-1", "ctx-1", "implicit-accept:h1"],
        "the synthetic must follow the last pre-deadline message"
    );

    // The fifth is the trigger, strictly after the synthetic.
    let commit = rx.recv().await.unwrap();
    assert_eq!(commit.message_id, "commit-1");

    // The synthetic on the wire is the target's accept, not a runtime message.
    let syn = &seen[3];
    assert_eq!(syn.sender, HANDOFF_TARGET);
    assert_eq!(syn.message_type, "HandoffAccept");
    assert_eq!(syn.mode, HANDOFF_MODE);
    assert_eq!(syn.session_id, sid);
    let payload = macp_runtime::handoff_pb::HandoffAcceptPayload::decode(&*syn.payload).unwrap();
    assert!(payload.implicit);
    assert_eq!(payload.accepted_by, HANDOFF_TARGET);
    // Its envelope clock is the deadline, not the publish time.
    assert!(syn.timestamp_unix_ms < commit.timestamp_unix_ms);
}
