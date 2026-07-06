use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{
    Ack, CancelSessionRequest, CommitmentPayload, Envelope, GetSessionRequest, GetSessionResponse,
    InitializeRequest, InitializeResponse, ListModesRequest, ListModesResponse, SendRequest,
    SessionStartPayload,
};
use prost::Message;
use tonic::transport::Channel;
use tonic::Request;

pub const MODE_DECISION: &str = "macp.mode.decision.v1";
pub const MODE_PROPOSAL: &str = "macp.mode.proposal.v1";
pub const MODE_TASK: &str = "macp.mode.task.v1";
pub const MODE_HANDOFF: &str = "macp.mode.handoff.v1";
pub const MODE_QUORUM: &str = "macp.mode.quorum.v1";
pub const MODE_MULTI_ROUND: &str = "ext.multi_round.v1";

pub const MODE_VERSION: &str = "1.0.0";
pub const CONFIG_VERSION: &str = "config.default";
pub const POLICY_VERSION: &str = "policy.default";

pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

pub fn new_message_id() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

/// Wrap a request with the dev-mode bearer identity (`Bearer <sender>`).
/// Public because streaming RPCs (e.g. authenticated `WatchSignals`) need it
/// outside this module.
pub fn with_sender<T>(sender: &str, inner: T) -> Request<T> {
    let mut request = Request::new(inner);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}")
            .parse()
            .expect("valid auth header"),
    );
    request
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

pub fn session_start_payload(intent: &str, participants: &[&str], ttl_ms: i64) -> Vec<u8> {
    SessionStartPayload {
        intent: intent.into(),
        participants: participants.iter().map(|p| (*p).to_string()).collect(),
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        ttl_ms,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

pub fn commitment_payload(
    commitment_id: &str,
    action: &str,
    authority_scope: &str,
    reason: &str,
    outcome_positive: bool,
) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: commitment_id.into(),
        action: action.into(),
        authority_scope: authority_scope.into(),
        reason: reason.into(),
        mode_version: MODE_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive,
        supersedes: None,
    }
    .encode_to_vec()
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
) -> Result<Ack, tonic::Status> {
    client
        .cancel_session(with_sender(
            sender,
            CancelSessionRequest {
                session_id: session_id.into(),
                reason: reason.into(),
            },
        ))
        .await
        .map(|r| r.into_inner().ack.expect("ack present"))
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

// ── Decision mode payload helpers ───────────────────────────────────────

pub fn proposal_payload(proposal_id: &str, option: &str, rationale: &str) -> Vec<u8> {
    macp_runtime::decision_pb::ProposalPayload {
        proposal_id: proposal_id.into(),
        option: option.into(),
        rationale: rationale.into(),
        supporting_data: vec![],
    }
    .encode_to_vec()
}

pub fn evaluation_payload(
    proposal_id: &str,
    recommendation: &str,
    confidence: f64,
    reason: &str,
) -> Vec<u8> {
    macp_runtime::decision_pb::EvaluationPayload {
        proposal_id: proposal_id.into(),
        recommendation: recommendation.into(),
        confidence,
        reason: reason.into(),
    }
    .encode_to_vec()
}

pub fn vote_payload(proposal_id: &str, vote: &str, reason: &str) -> Vec<u8> {
    macp_runtime::decision_pb::VotePayload {
        proposal_id: proposal_id.into(),
        vote: vote.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}

pub fn objection_payload(proposal_id: &str, reason: &str, severity: &str) -> Vec<u8> {
    macp_runtime::decision_pb::ObjectionPayload {
        proposal_id: proposal_id.into(),
        reason: reason.into(),
        severity: severity.into(),
    }
    .encode_to_vec()
}

// ── Proposal mode payload helpers ───────────────────────────────────────

pub fn proposal_mode_payload(proposal_id: &str, title: &str, summary: &str) -> Vec<u8> {
    macp_runtime::proposal_pb::ProposalPayload {
        proposal_id: proposal_id.into(),
        title: title.into(),
        summary: summary.into(),
        details: vec![],
        tags: vec![],
    }
    .encode_to_vec()
}

pub fn counter_proposal_payload(
    proposal_id: &str,
    supersedes: &str,
    title: &str,
    summary: &str,
) -> Vec<u8> {
    macp_runtime::proposal_pb::CounterProposalPayload {
        proposal_id: proposal_id.into(),
        supersedes_proposal_id: supersedes.into(),
        title: title.into(),
        summary: summary.into(),
        details: String::new().into_bytes(),
    }
    .encode_to_vec()
}

pub fn accept_proposal_payload(proposal_id: &str, reason: &str) -> Vec<u8> {
    macp_runtime::proposal_pb::AcceptPayload {
        proposal_id: proposal_id.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}

// ── Task mode payload helpers ───────────────────────────────────────────

pub fn task_request_payload(
    task_id: &str,
    title: &str,
    instructions: &str,
    assignee: &str,
) -> Vec<u8> {
    macp_runtime::task_pb::TaskRequestPayload {
        task_id: task_id.into(),
        title: title.into(),
        instructions: instructions.into(),
        requested_assignee: assignee.into(),
        input: vec![],
        deadline_unix_ms: 0,
    }
    .encode_to_vec()
}

pub fn task_accept_payload(task_id: &str, assignee: &str) -> Vec<u8> {
    macp_runtime::task_pb::TaskAcceptPayload {
        task_id: task_id.into(),
        assignee: assignee.into(),
        reason: String::new(),
    }
    .encode_to_vec()
}

pub fn task_update_payload(task_id: &str, status: &str, progress: f64, message: &str) -> Vec<u8> {
    macp_runtime::task_pb::TaskUpdatePayload {
        task_id: task_id.into(),
        status: status.into(),
        progress,
        message: message.into(),
        partial_output: vec![],
    }
    .encode_to_vec()
}

pub fn task_complete_payload(task_id: &str, assignee: &str, summary: &str) -> Vec<u8> {
    macp_runtime::task_pb::TaskCompletePayload {
        task_id: task_id.into(),
        assignee: assignee.into(),
        output: vec![],
        summary: summary.into(),
    }
    .encode_to_vec()
}

// ── Handoff mode payload helpers ────────────────────────────────────────

pub fn handoff_offer_payload(handoff_id: &str, target: &str, scope: &str, reason: &str) -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffOfferPayload {
        handoff_id: handoff_id.into(),
        target_participant: target.into(),
        scope: scope.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}

pub fn handoff_context_payload(handoff_id: &str, content_type: &str, context: &[u8]) -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffContextPayload {
        handoff_id: handoff_id.into(),
        content_type: content_type.into(),
        context: context.to_vec(),
    }
    .encode_to_vec()
}

pub fn handoff_accept_payload(handoff_id: &str, accepted_by: &str, reason: &str) -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffAcceptPayload {
        handoff_id: handoff_id.into(),
        accepted_by: accepted_by.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}

// ── Quorum mode payload helpers ─────────────────────────────────────────

pub fn approval_request_payload(
    request_id: &str,
    action: &str,
    summary: &str,
    required_approvals: u32,
) -> Vec<u8> {
    macp_runtime::quorum_pb::ApprovalRequestPayload {
        request_id: request_id.into(),
        action: action.into(),
        summary: summary.into(),
        details: vec![],
        required_approvals,
    }
    .encode_to_vec()
}

pub fn approve_payload(request_id: &str, reason: &str) -> Vec<u8> {
    macp_runtime::quorum_pb::ApprovePayload {
        request_id: request_id.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}

pub fn quorum_reject_payload(request_id: &str, reason: &str) -> Vec<u8> {
    macp_runtime::quorum_pb::RejectPayload {
        request_id: request_id.into(),
        reason: reason.into(),
    }
    .encode_to_vec()
}
