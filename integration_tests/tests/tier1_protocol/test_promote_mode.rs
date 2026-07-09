//! PromoteMode: extension → standards-track promotion. Runs against a
//! dedicated server because promotion permanently grows the standards-track
//! registry, which would break the shared server's `ListModes == 5`
//! invariant for other tests.

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{
    ListExtModesRequest, ListModesRequest, ModeDescriptor, PromoteModeRequest,
    RegisterExtModeRequest,
};

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

#[tokio::test]
async fn promote_registered_extension_to_standards_track() {
    let manager = ServerManager::start(&test_binary())
        .await
        .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");
    let agent = "agent://promoter";

    let resp = client
        .register_ext_mode(with_sender(
            agent,
            RegisterExtModeRequest {
                mode_descriptor: Some(ModeDescriptor {
                    mode: "ext.test.promotable.v1".into(),
                    mode_version: "1.0.0".into(),
                    title: "Promotable".into(),
                    description: "extension slated for promotion".into(),
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

    // Renaming into the reserved macp.mode.* namespace is refused
    // (RFC-MACP-0002 §12: promotion grants standards-track status on this
    // runtime, not RFC namespace membership).
    let resp = client
        .promote_mode(with_sender(
            agent,
            PromoteModeRequest {
                mode: "ext.test.promotable.v1".into(),
                promoted_mode_name: "macp.mode.promotable.v1".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.ok, "reserved-namespace rename must be refused");
    assert!(resp.error.contains("reserved"), "got: {}", resp.error);

    // Promotion under the existing identifier succeeds.
    let resp = client
        .promote_mode(with_sender(
            agent,
            PromoteModeRequest {
                mode: "ext.test.promotable.v1".into(),
                promoted_mode_name: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "promotion must succeed: {}", resp.error);
    assert_eq!(resp.mode, "ext.test.promotable.v1");

    // Promoted mode is now standards-track and no longer an extension.
    let modes = client
        .list_modes(with_sender(agent, ListModesRequest {}))
        .await
        .unwrap()
        .into_inner();
    let std_ids: Vec<&str> = modes.modes.iter().map(|m| m.mode.as_str()).collect();
    assert!(
        std_ids.contains(&"ext.test.promotable.v1"),
        "promoted mode must be listed as standards-track, got {std_ids:?}"
    );

    let ext = client
        .list_ext_modes(with_sender(agent, ListExtModesRequest {}))
        .await
        .unwrap()
        .into_inner();
    assert!(
        !ext.modes.iter().any(|m| m.mode == "ext.test.promotable.v1"),
        "promoted mode must leave the extension registry"
    );
}

#[tokio::test]
async fn promote_unknown_mode_fails() {
    let manager = ServerManager::start(&test_binary())
        .await
        .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    let resp = client
        .promote_mode(with_sender(
            "agent://promoter",
            PromoteModeRequest {
                mode: "ext.does.not.exist.v1".into(),
                promoted_mode_name: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.ok, "promoting an unknown mode must fail");
    assert!(!resp.error.is_empty());
}
