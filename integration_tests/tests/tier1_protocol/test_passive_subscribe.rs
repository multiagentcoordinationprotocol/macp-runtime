//! RFC-MACP-0006-A1: StreamSession passive subscribe.
//!
//! A client opens a StreamSession and sends a subscribe-only frame
//! (subscribe_session_id + after_sequence, no envelope) to replay accepted
//! history and then receive live envelopes.

use std::time::Duration;

use macp_integration_tests::helpers::*;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::stream_session_response::Response as StreamResp;
use macp_runtime::pb::{Envelope, StreamSessionRequest, StreamSessionResponse};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Streaming;

use crate::common;

const COORD: &str = "agent://coordinator";
const PEER: &str = "agent://voter";
const OUTSIDER: &str = "agent://outsider";

fn subscribe_frame(session_id: &str, after_sequence: u64) -> StreamSessionRequest {
    StreamSessionRequest {
        subscribe_session_id: session_id.into(),
        after_sequence,
        envelope: None,
    }
}

fn envelope_frame(env: Envelope) -> StreamSessionRequest {
    StreamSessionRequest {
        subscribe_session_id: String::new(),
        after_sequence: 0,
        envelope: Some(env),
    }
}

/// Open a stream bound to `sender` and return the response stream plus the
/// request-sender channel. Dropping the sender closes the client side cleanly.
async fn open_stream(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
) -> (
    mpsc::Sender<StreamSessionRequest>,
    Streaming<StreamSessionResponse>,
) {
    let (tx, rx) = mpsc::channel::<StreamSessionRequest>(8);
    let request_stream = ReceiverStream::new(rx);
    let mut req = tonic::Request::new(request_stream);
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}").parse().expect("valid auth"),
    );
    let response = client
        .stream_session(req)
        .await
        .expect("stream_session opened");
    (tx, response.into_inner())
}

async fn next_envelope(stream: &mut Streaming<StreamSessionResponse>) -> Envelope {
    let resp = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for envelope")
        .expect("stream returned error")
        .expect("stream ended unexpectedly");
    match resp.response.expect("response variant") {
        StreamResp::Envelope(env) => env,
        StreamResp::Error(err) => panic!("expected envelope, got error: {err:?}"),
    }
}

async fn next_error_code(stream: &mut Streaming<StreamSessionResponse>) -> String {
    let resp = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for error")
        .expect("stream returned error")
        .expect("stream ended unexpectedly");
    match resp.response.expect("response variant") {
        StreamResp::Error(err) => err.code,
        StreamResp::Envelope(env) => panic!("expected error, got envelope: {}", env.message_type),
    }
}

async fn start_decision_session_with_proposal(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sid: &str,
    proposal_id: &str,
    proposal_message_id: &str,
) {
    let ack = send_as(
        client,
        COORD,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            sid,
            COORD,
            session_start_payload("subscribe test", &[COORD, PEER], 60_000),
        ),
    )
    .await
    .expect("SessionStart Send");
    assert!(ack.ok, "SessionStart rejected: {:?}", ack.error);

    let ack = send_as(
        client,
        COORD,
        envelope(
            MODE_DECISION,
            "Proposal",
            proposal_message_id,
            sid,
            COORD,
            proposal_payload(proposal_id, "option-A", "initial"),
        ),
    )
    .await
    .expect("Proposal Send");
    assert!(ack.ok, "Proposal rejected: {:?}", ack.error);
}

#[tokio::test]
async fn subscribe_replays_full_history_from_zero() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    start_decision_session_with_proposal(&mut client, &sid, "p1", "msg-proposal").await;

    let (tx, mut stream) = open_stream(&mut client, PEER).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");

    let first = next_envelope(&mut stream).await;
    assert_eq!(first.message_type, "SessionStart");
    assert_eq!(first.session_id, sid);

    let second = next_envelope(&mut stream).await;
    assert_eq!(second.message_type, "Proposal");
    assert_eq!(second.message_id, "msg-proposal");

    // Drop the request sender — the server stream should drain and close.
    drop(tx);
    let trailing = tokio::time::timeout(Duration::from_secs(2), stream.message()).await;
    assert!(
        matches!(trailing, Ok(Ok(None)) | Err(_)),
        "expected stream end after client close, got {trailing:?}"
    );
}

#[tokio::test]
async fn subscribe_with_after_sequence_skips_replayed_entries() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    start_decision_session_with_proposal(&mut client, &sid, "p1", "msg-proposal").await;

    let (tx, mut stream) = open_stream(&mut client, PEER).await;
    // SessionStart is at log index 0, Proposal is at log index 1 — skip the
    // SessionStart by asking for entries after_sequence=1.
    tx.send(subscribe_frame(&sid, 1))
        .await
        .expect("send subscribe");

    let env = next_envelope(&mut stream).await;
    assert_eq!(env.message_type, "Proposal");
    assert_eq!(env.message_id, "msg-proposal");

    drop(tx);
}

#[tokio::test]
async fn subscribe_unknown_session_terminates_stream() {
    let mut client = common::grpc_client().await;
    let (tx, mut stream) = open_stream(&mut client, COORD).await;
    tx.send(subscribe_frame("no-such-session", 0))
        .await
        .expect("send subscribe");

    // NOT_FOUND is a terminal error — the stream closes with a tonic Status.
    let result = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for stream termination");
    let status = result.expect_err("expected NotFound status on unknown session");
    assert_eq!(status.code(), tonic::Code::NotFound, "{status:?}");
}

#[tokio::test]
async fn subscribe_as_non_participant_yields_inline_error() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    start_decision_session_with_proposal(&mut client, &sid, "p1", "msg-proposal").await;

    let (tx, mut stream) = open_stream(&mut client, OUTSIDER).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");

    // PERMISSION_DENIED is non-terminal: the server sends an inline error and
    // keeps the stream open.
    let code = next_error_code(&mut stream).await;
    assert!(
        code.contains("FORBIDDEN") || code.to_uppercase().contains("PERMISSION"),
        "expected forbidden error code, got {code}"
    );

    drop(tx);
}

#[tokio::test]
async fn subscribe_replays_history_then_receives_live_envelope() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    start_decision_session_with_proposal(&mut client, &sid, "p1", "msg-proposal").await;

    let (tx, mut stream) = open_stream(&mut client, PEER).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");

    // Drain replayed history (SessionStart + Proposal).
    assert_eq!(
        next_envelope(&mut stream).await.message_type,
        "SessionStart"
    );
    assert_eq!(next_envelope(&mut stream).await.message_type, "Proposal");

    // Send an Evaluation via a unary Send from PEER. It should arrive over the
    // live stream as well.
    let eval_id = "msg-evaluation";
    let ack = send_as(
        &mut client,
        PEER,
        envelope(
            MODE_DECISION,
            "Evaluation",
            eval_id,
            &sid,
            PEER,
            evaluation_payload("p1", "APPROVE", 0.9, "looks good"),
        ),
    )
    .await
    .expect("Evaluation Send");
    assert!(ack.ok, "Evaluation rejected: {:?}", ack.error);

    let live = next_envelope(&mut stream).await;
    assert_eq!(live.message_type, "Evaluation");
    assert_eq!(live.message_id, eval_id);

    drop(tx);
}

#[tokio::test]
async fn stream_request_with_both_envelope_and_subscribe_is_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let (tx, mut stream) = open_stream(&mut client, COORD).await;

    // Build a malformed request that sets both fields.
    let env = envelope(
        MODE_DECISION,
        "SessionStart",
        &new_message_id(),
        &sid,
        COORD,
        session_start_payload("bad", &[COORD, PEER], 60_000),
    );
    let mut bad = envelope_frame(env);
    bad.subscribe_session_id = sid.clone();

    tx.send(bad).await.expect("send frame");

    let result = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for termination");
    let status = result.expect_err("expected InvalidArgument status");
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");
}

/// RFC-MACP-0006 §3.2: a resume below the compacted base is refused as a
/// terminal gRPC status, FAILED_PRECONDITION with `macp-error-code:
/// HISTORY_UNAVAILABLE` and `macp-error-details-bin` =
/// `{"lowest_available_sequence":N}`. Compaction needs a persistent backend
/// (the memory backend skips it), so this test runs its own server.
#[tokio::test]
async fn subscribe_below_compacted_base_ends_stream_with_history_unavailable() {
    const BUYER: &str = "agent://buyer";
    const SELLER: &str = "agent://seller";
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_dir_str = data_dir.path().to_string_lossy().to_string();
    let manager = macp_integration_tests::server_manager::ServerManager::start_with_env(
        &binary,
        &[
            ("MACP_MEMORY_ONLY", "0"),
            ("MACP_DATA_DIR", data_dir_str.as_str()),
        ],
    )
    .await
    .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    // SessionStart(1) → Proposal(2) → Accept(3) → Accept(4) → Commitment(5):
    // resolving compacts the log to a checkpoint whose base is 5.
    let sid = new_session_id();
    let steps = [
        (
            BUYER,
            "SessionStart",
            session_start_payload("buy", &[BUYER, SELLER], 300_000),
        ),
        (
            SELLER,
            "Proposal",
            proposal_mode_payload("p1", "offer", "terms"),
        ),
        (SELLER, "Accept", accept_proposal_payload("p1", "ok")),
        (BUYER, "Accept", accept_proposal_payload("p1", "ok")),
        (
            BUYER,
            "Commitment",
            commitment_payload("c1", "proposal.accepted", "commercial", "bound", true),
        ),
    ];
    for (sender, message_type, payload) in steps {
        let ack = send_as(
            &mut client,
            sender,
            envelope(
                MODE_PROPOSAL,
                message_type,
                &new_message_id(),
                &sid,
                sender,
                payload,
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "{message_type}: {:?}", ack.error);
    }

    let (tx, mut stream) = open_stream(&mut client, BUYER).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");
    let status = tokio::time::timeout(Duration::from_secs(2), stream.message())
        .await
        .expect("timed out waiting for termination")
        .expect_err("a compacted resume must end the stream with a status");
    assert_eq!(status.code(), tonic::Code::FailedPrecondition, "{status:?}");
    assert_eq!(
        status
            .metadata()
            .get("macp-error-code")
            .and_then(|v| v.to_str().ok()),
        Some("HISTORY_UNAVAILABLE")
    );
    let details = status
        .metadata()
        .get_bin("macp-error-details-bin")
        .expect("details present")
        .to_bytes()
        .expect("valid base64");
    assert_eq!(&details[..], br#"{"lowest_available_sequence":5}"#);
    assert!(
        status
            .message()
            .contains("session history before ordinal 5 was compacted;"),
        "{}",
        status.message()
    );
    drop(tx);

    // At the base, the resume is served: nothing to replay, and the stream
    // stays open rather than failing.
    let (tx, mut stream) = open_stream(&mut client, BUYER).await;
    tx.send(subscribe_frame(&sid, 5))
        .await
        .expect("send subscribe");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), stream.message())
            .await
            .is_err(),
        "a resume at the compacted base must not fail the stream"
    );
    drop(tx);
}
