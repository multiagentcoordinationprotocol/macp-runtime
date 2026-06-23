#![allow(dead_code)]

use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{
    Ack, CancelSessionRequest, CancelSessionResponse, CommitmentPayload, Envelope,
    GetManifestRequest, GetManifestResponse, GetSessionRequest, GetSessionResponse,
    InitializeRequest, InitializeResponse, ListModesRequest, ListModesResponse, ListRootsRequest,
    ListRootsResponse, SendRequest, SessionStartPayload,
};
use prost::Message;
use tonic::transport::Channel;
use tonic::Request;

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

pub const DEV_ENDPOINT: &str = "http://127.0.0.1:50051";
pub const MODE_VERSION: &str = "1.0.0";
pub const CONFIG_VERSION: &str = "config.default";
pub const POLICY_VERSION: &str = "policy.default";

pub async fn connect_client(
) -> Result<MacpRuntimeServiceClient<Channel>, Box<dyn std::error::Error>> {
    Ok(MacpRuntimeServiceClient::connect(DEV_ENDPOINT).await?)
}

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

pub fn canonical_start_payload(intent: &str, participants: &[&str], ttl_ms: i64) -> Vec<u8> {
    SessionStartPayload {
        intent: intent.into(),
        participants: participants.iter().map(|p| (*p).to_string()).collect(),
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        ttl_ms,
        roots: vec![],
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
    }
    .encode_to_vec()
}

pub fn canonical_commitment_payload(
    commitment_id: &str,
    action: &str,
    authority_scope: &str,
    reason: &str,
) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: commitment_id.into(),
        action: action.into(),
        authority_scope: authority_scope.into(),
        reason: reason.into(),
        mode_version: MODE_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

pub fn envelope(
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

pub async fn initialize(
    client: &mut MacpRuntimeServiceClient<Channel>,
) -> Result<InitializeResponse, tonic::Status> {
    client
        .initialize(InitializeRequest {
            supported_protocol_versions: vec!["1.0".into()],
            client_info: None,
            capabilities: None,
        })
        .await
        .map(|r| r.into_inner())
}

pub async fn list_modes(
    client: &mut MacpRuntimeServiceClient<Channel>,
) -> Result<ListModesResponse, tonic::Status> {
    client
        .list_modes(ListModesRequest {})
        .await
        .map(|r| r.into_inner())
}

pub async fn list_roots(
    client: &mut MacpRuntimeServiceClient<Channel>,
) -> Result<ListRootsResponse, tonic::Status> {
    client
        .list_roots(ListRootsRequest {})
        .await
        .map(|r| r.into_inner())
}

pub async fn get_manifest(
    client: &mut MacpRuntimeServiceClient<Channel>,
    agent_id: &str,
) -> Result<GetManifestResponse, tonic::Status> {
    client
        .get_manifest(GetManifestRequest {
            agent_id: agent_id.into(),
        })
        .await
        .map(|r| r.into_inner())
}

pub async fn send_as(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    env: Envelope,
) -> Result<Ack, tonic::Status> {
    client
        .send(with_sender(
            sender,
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .map(|r| r.into_inner().ack.expect("ack present"))
}

pub async fn get_session_as(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    session_id: &str,
) -> Result<GetSessionResponse, tonic::Status> {
    client
        .get_session(with_sender(
            sender,
            GetSessionRequest {
                session_id: session_id.into(),
            },
        ))
        .await
        .map(|r| r.into_inner())
}

pub async fn cancel_session_as(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
    session_id: &str,
    reason: &str,
) -> Result<CancelSessionResponse, tonic::Status> {
    client
        .cancel_session(with_sender(
            sender,
            CancelSessionRequest {
                session_id: session_id.into(),
                reason: reason.into(),
            },
        ))
        .await
        .map(|r| r.into_inner())
}

pub fn print_ack(label: &str, ack: &Ack) {
    let err_code = ack.error.as_ref().map(|e| e.code.as_str()).unwrap_or("");
    println!(
        "[{label}] ok={} duplicate={} state={} error='{}'",
        ack.ok, ack.duplicate, ack.session_state, err_code
    );
}
