//! Discovery/observation streams that previously had no gRPC-boundary
//! coverage: WatchModeRegistry, WatchPolicies, WatchRoots, and ListRoots.

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::{
    ListRootsRequest, ModeDescriptor, PolicyDescriptor, RegisterExtModeRequest,
    RegisterPolicyRequest, UnregisterExtModeRequest, UnregisterPolicyRequest,
    WatchModeRegistryRequest, WatchPoliciesRequest, WatchRootsRequest,
};

#[tokio::test]
async fn watch_mode_registry_emits_on_register() {
    let mut client = common::grpc_client().await;
    let agent = "agent://registry-watcher";

    let mut stream = client
        .watch_mode_registry(with_sender(agent, WatchModeRegistryRequest {}))
        .await
        .unwrap()
        .into_inner();

    // Mutate the registry after subscribing.
    let mode_name = format!("ext.test.watch{}.v1", &new_session_id()[..8]);
    let resp = client
        .register_ext_mode(with_sender(
            agent,
            RegisterExtModeRequest {
                mode_descriptor: Some(ModeDescriptor {
                    mode: mode_name.clone(),
                    mode_version: "1.0.0".into(),
                    title: "Watch test".into(),
                    description: "registry watch test mode".into(),
                    determinism_class: String::new(),
                    participant_model: String::new(),
                    message_types: vec!["Ping".into(), "Commitment".into()],
                    terminal_message_types: vec!["Commitment".into()],
                    schema_uris: Default::default(),
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "register must succeed: {}", resp.error);

    // The watcher must observe a registry change.
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), stream.message())
        .await
        .expect("registry change must arrive within 5s")
        .expect("stream must stay healthy")
        .expect("stream must not end");
    assert!(
        event.change.is_some(),
        "watch event must carry a RegistryChanged payload"
    );

    // Cleanup so the shared registry stays predictable for other tests.
    let resp = client
        .unregister_ext_mode(with_sender(
            agent,
            UnregisterExtModeRequest { mode: mode_name },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);
}

#[tokio::test]
async fn watch_policies_emits_registered_policy() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-watcher";

    let mut stream = client
        .watch_policies(with_sender(agent, WatchPoliciesRequest {}))
        .await
        .unwrap()
        .into_inner();

    let policy_id = format!("policy.test.watch{}", &new_session_id()[..8]);
    let resp = client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(PolicyDescriptor {
                    policy_id: policy_id.clone(),
                    mode: "macp.mode.decision.v1".into(),
                    description: "watch test policy".into(),
                    rules: serde_json::json!({
                        "commitment": { "authority": "initiator_only" }
                    })
                    .to_string(),
                    schema_version: 1,
                    registered_at_unix_ms: 0,
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "register_policy must succeed: {}", resp.error);

    // Read watch responses until one lists the new policy (the first message
    // may be an initial snapshot taken before the registration).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = false;
    while tokio::time::Instant::now() < deadline {
        let next = tokio::time::timeout_at(deadline, stream.message()).await;
        let Ok(Ok(Some(resp))) = next else { break };
        if resp.descriptors.iter().any(|d| d.policy_id == policy_id) {
            seen = true;
            break;
        }
    }
    assert!(seen, "WatchPolicies must surface the registered policy");

    let resp = client
        .unregister_policy(with_sender(agent, UnregisterPolicyRequest { policy_id }))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);
}

#[tokio::test]
async fn list_roots_returns_advertised_roots() {
    let mut client = common::grpc_client().await;
    let resp = client
        .list_roots(with_sender("agent://roots-reader", ListRootsRequest {}))
        .await
        .expect("ListRoots must be served (the capability is advertised)")
        .into_inner();
    // The default deployment advertises no roots; the RPC contract is that
    // the call succeeds and returns the (possibly empty) set.
    assert!(resp.roots.is_empty() || resp.roots.iter().all(|r| !r.uri.is_empty()));
}

#[tokio::test]
async fn watch_roots_stream_opens_cleanly() {
    let mut client = common::grpc_client().await;
    let mut stream = client
        .watch_roots(with_sender("agent://roots-watcher", WatchRootsRequest {}))
        .await
        .expect("WatchRoots subscription must be accepted")
        .into_inner();

    // Roots never change in this deployment, so no event is expected; the
    // stream must simply stay open (a quick error would surface here).
    match tokio::time::timeout(std::time::Duration::from_millis(500), stream.message()).await {
        Err(_elapsed) => {} // healthy: no event within the window
        Ok(Ok(_)) => {}     // also fine: an initial event arrived
        Ok(Err(status)) => panic!("WatchRoots stream errored immediately: {status}"),
    }
}
