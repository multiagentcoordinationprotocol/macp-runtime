//! SuspendSession / ResumeSession (RFC-MACP-0001 §7.5): same authority model
//! as CancelSession — initiator or policy-delegated roles only.

use std::time::Duration;

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::stream_session_response::Response as StreamResp;
use macp_runtime::pb::{
    Envelope, ResumeSessionRequest, StreamSessionRequest, StreamSessionResponse,
    SuspendSessionRequest,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Streaming;

#[tokio::test]
async fn suspend_resume_lifecycle() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let initiator = "agent://suspender";
    let partner = "agent://partner";

    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            initiator,
            session_start_payload("suspend test", &[initiator, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Suspend from the initiator.
    let ack = client
        .suspend_session(with_sender(
            initiator,
            SuspendSessionRequest {
                session_id: sid.clone(),
                reason: "pausing for maintenance".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "suspend must succeed: {:?}", ack.error);
    assert_eq!(ack.session_state, 4); // SUSPENDED

    // Mode traffic while suspended is rejected.
    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            initiator,
            proposal_payload("p1", "while-suspended", "must be rejected"),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "sends into a suspended session must be rejected");

    // Resume restores the session to OPEN and traffic flows again.
    let ack = client
        .resume_session(with_sender(
            initiator,
            ResumeSessionRequest {
                session_id: sid.clone(),
                reason: "maintenance done".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "resume must succeed: {:?}", ack.error);
    assert_eq!(ack.session_state, 1); // OPEN

    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            initiator,
            proposal_payload("p2", "after-resume", "accepted again"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "sends after resume must be accepted");
}

#[tokio::test]
async fn suspend_from_non_initiator_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let initiator = "agent://suspend-owner";
    let partner = "agent://suspend-peer";

    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            initiator,
            session_start_payload("authz test", &[initiator, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // A declared participant without commitment authority cannot suspend.
    let err = client
        .suspend_session(with_sender(
            partner,
            SuspendSessionRequest {
                session_id: sid.clone(),
                reason: "not my call".into(),
            },
        ))
        .await
        .expect_err("non-initiator suspend must be refused");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn suspend_unknown_session_not_found() {
    let mut client = common::grpc_client().await;
    let err = client
        .suspend_session(with_sender(
            "agent://nobody",
            SuspendSessionRequest {
                session_id: new_session_id(),
                reason: "no such session".into(),
            },
        ))
        .await
        .expect_err("suspending an unknown session must fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
}

// ── StreamSession plumbing (mirrors test_passive_subscribe.rs; local copy —
// these are private `fn`s in a sibling module, per the pattern already used
// in test_handoff_implicit_accept.rs) ──────────────────────────────────────

fn subscribe_frame(session_id: &str, after_sequence: u64) -> StreamSessionRequest {
    StreamSessionRequest {
        subscribe_session_id: session_id.into(),
        after_sequence,
        envelope: None,
    }
}

async fn open_stream(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
) -> (
    mpsc::Sender<StreamSessionRequest>,
    Streaming<StreamSessionResponse>,
) {
    let (tx, rx) = mpsc::channel::<StreamSessionRequest>(8);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
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
    let resp = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .expect("timed out waiting for envelope")
        .expect("stream returned error")
        .expect("stream ended unexpectedly");
    match resp.response.expect("response variant") {
        StreamResp::Envelope(env) => env,
        StreamResp::Error(err) => panic!("expected envelope, got error: {err:?}"),
    }
}

/// RFC-MACP-0006 §3.2:117/:122 over the real gRPC boundary: passive subscribe
/// with `after_sequence: 0` must replay exactly the client-accepted envelopes
/// — SessionStart, and the mode messages either side of the pause — never
/// SessionSuspend/SessionResume. `:122`'s "MUST NOT deliver an internal
/// annotation on this stream" is textually scoped to "a subscribe stream",
/// which is exactly the RPC path this test drives
/// (`process_subscribe_frame` -> `get_session_envelopes_after`) — the clause
/// that governs literally here, unlike the in-process
/// `stream_subscriber_never_sees_lifecycle_annotations` test (live
/// `StreamSession`, which follows from `:117` plus `:120`'s counting
/// argument instead).
#[tokio::test]
async fn subscribe_never_delivers_lifecycle_annotations() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let initiator = "agent://ordinal-initiator";
    let partner = "agent://ordinal-partner";

    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            initiator,
            session_start_payload("ordinal test", &[initiator, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let before_id = "m-before-suspend";
    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            before_id,
            &sid,
            initiator,
            proposal_payload("p1", "before-suspend", "ordinal check"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "Proposal rejected: {:?}", ack.error);

    let ack = client
        .suspend_session(with_sender(
            initiator,
            SuspendSessionRequest {
                session_id: sid.clone(),
                reason: "pause for ordinal test".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "suspend must succeed: {:?}", ack.error);

    let ack = client
        .resume_session(with_sender(
            initiator,
            ResumeSessionRequest {
                session_id: sid.clone(),
                reason: "resume for ordinal test".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "resume must succeed: {:?}", ack.error);

    let after_id = "m-after-resume";
    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            after_id,
            &sid,
            initiator,
            proposal_payload("p2", "after-resume", "ordinal check"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "Proposal rejected: {:?}", ack.error);

    let (tx, mut stream) = open_stream(&mut client, partner).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");

    let first = next_envelope(&mut stream).await;
    assert_eq!(first.message_type, "SessionStart");
    let second = next_envelope(&mut stream).await;
    assert_eq!(second.message_type, "Proposal");
    assert_eq!(second.message_id, before_id);
    let third = next_envelope(&mut stream).await;
    assert_eq!(third.message_type, "Proposal");
    assert_eq!(third.message_id, after_id);

    // The positive assertions above already pin the exact expected sequence
    // (3 envelopes); closing the client and expecting a clean end proves no
    // fourth envelope was queued — specifically, neither SessionSuspend nor
    // SessionResume was delivered in between.
    drop(tx);
    let trailing = tokio::time::timeout(Duration::from_secs(2), stream.message()).await;
    assert!(
        matches!(trailing, Ok(Ok(None)) | Err(_)),
        "expected stream end after replay + client close, got {trailing:?}"
    );
}
