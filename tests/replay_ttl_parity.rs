//! RFC-MACP-0003: a replayed session's bindings equal the ones the live kernel
//! bound — the TTL deadline (§2: the SessionStart envelope's
//! `timestamp_unix_ms` + `ttl_ms`) and the resolved policy (RFC-MACP-0012
//! §6.1: an empty `policy_version` binds `policy.default`).

use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::ManualClock;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::{Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use prost::Message;
use std::sync::Arc;

#[tokio::test]
async fn replay_binds_what_the_live_kernel_bound() {
    let clock = Arc::new(ManualClock::new(10_000));
    let rt = Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_clock(clock);
    let sid = uuid::Uuid::new_v4().as_hyphenated().to_string();
    let payload = SessionStartPayload {
        intent: "ttl parity".into(),
        participants: vec!["agent://a".into(), "agent://b".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: 60_000,
        ..Default::default()
    };
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: "macp.mode.decision.v1".into(),
        message_type: "SessionStart".into(),
        message_id: "start".into(),
        session_id: sid.clone(),
        sender: "agent://a".into(),
        // Client clock 5s behind the acceptance clock.
        timestamp_unix_ms: 5_000,
        payload: payload.encode_to_vec(),
    };
    rt.process(&env, None).await.expect("session starts");
    let live = rt.registry.get_session(&sid).await.expect("live session");
    assert_eq!(live.ttl_expiry, 65_000, "live deadline: envelope ts + ttl");

    let log = rt.log_store.get_log(&sid).await.expect("log");
    let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
    let replayed =
        replay_session(&sid, &log, &registry, Some(rt.policy_registry())).expect("replays");
    assert_eq!(replayed.ttl_expiry, live.ttl_expiry);
    assert_eq!(replayed.started_at_unix_ms, live.started_at_unix_ms);
    assert_eq!(live.policy_version, "policy.default");
    assert_eq!(replayed.policy_version, live.policy_version);
    assert_eq!(
        replayed.policy_definition.as_ref().map(|p| &p.policy_id),
        live.policy_definition.as_ref().map(|p| &p.policy_id)
    );
}
