use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::{
    ListExtModesRequest, ModeDescriptor, RegisterExtModeRequest, UnregisterExtModeRequest,
};
use tonic::Request;

fn with_sender<T>(sender: &str, inner: T) -> Request<T> {
    let mut request = Request::new(inner);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}")
            .parse()
            .expect("valid auth header"),
    );
    request
}

#[tokio::test]
async fn list_modes_returns_standard_modes() {
    let mut client = common::grpc_client().await;
    let resp = list_modes(&mut client).await.unwrap();
    assert_eq!(resp.modes.len(), 5);
}

#[tokio::test]
async fn list_ext_modes_includes_multi_round() {
    let mut client = common::grpc_client().await;
    let resp = client
        .list_ext_modes(ListExtModesRequest {})
        .await
        .unwrap()
        .into_inner();
    let ext_ids: Vec<&str> = resp.modes.iter().map(|m| m.mode.as_str()).collect();
    assert!(ext_ids.contains(&"ext.multi_round.v1"));
}

#[tokio::test]
async fn register_and_unregister_ext_mode() {
    let mut client = common::grpc_client().await;
    let agent = "agent://registry-admin";

    let descriptor = ModeDescriptor {
        mode: "ext.test.custom.v1".into(),
        mode_version: "1.0.0".into(),
        title: "Test Custom Extension".into(),
        description: "A test extension mode".into(),
        determinism_class: String::new(),
        participant_model: String::new(),
        message_types: vec!["Ping".into(), "Pong".into(), "Commitment".into()],
        terminal_message_types: vec!["Commitment".into()],
        schema_uris: Default::default(),
    };

    // Register
    let resp = client
        .register_ext_mode(with_sender(
            agent,
            RegisterExtModeRequest {
                mode_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);

    // Verify it appears in ext modes
    let resp = client
        .list_ext_modes(ListExtModesRequest {})
        .await
        .unwrap()
        .into_inner();
    let ext_ids: Vec<&str> = resp.modes.iter().map(|m| m.mode.as_str()).collect();
    assert!(ext_ids.contains(&"ext.test.custom.v1"));

    // Unregister
    let resp = client
        .unregister_ext_mode(with_sender(
            agent,
            UnregisterExtModeRequest {
                mode: "ext.test.custom.v1".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);

    // Verify it's gone
    let resp = client
        .list_ext_modes(ListExtModesRequest {})
        .await
        .unwrap()
        .into_inner();
    let ext_ids: Vec<&str> = resp.modes.iter().map(|m| m.mode.as_str()).collect();
    assert!(!ext_ids.contains(&"ext.test.custom.v1"));
}
