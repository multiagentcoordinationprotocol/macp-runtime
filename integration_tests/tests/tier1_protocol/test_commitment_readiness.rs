//! Behaviour changed in 0.13.2 and 0.14.0, proven through the real gRPC
//! boundary rather than only by the in-process conformance fixtures:
//!
//! * RFC-MACP-0010 §6.1 (0.14.0): a positive Handoff Commitment needs an
//!   accepted offer; a negative one needs the session settled without a
//!   transfer (no offer, or every offer declined). Otherwise INVALID_ENVELOPE.
//! * RFC-MACP-0001 §7.6 item 9 (0.14.0): SessionCancel / SessionSuspend /
//!   SessionResume are runtime-emitted annotations, and submitting one via
//!   `Send` is FORBIDDEN in every mode.
//! * RFC-MACP-0009 §5 rule 3c (0.13.2): the active assignee's TaskReject with
//!   no reassignment policy is INVALID_ENVELOPE, not POLICY_DENIED (RFC-MACP-0002
//!   §6.1 reserves POLICY_DENIED for Commitments).
//!
//! Every rejection is also checked to leave the session OPEN: a refused
//! message must not move the lifecycle.

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{Ack, SessionState};
use tonic::transport::Channel;

const OWNER: &str = "agent://owner";
const TARGET: &str = "agent://target";
const TARGET2: &str = "agent://target2";

async fn send(
    client: &mut MacpRuntimeServiceClient<Channel>,
    mode: &str,
    sid: &str,
    sender: &str,
    message_type: &str,
    payload: Vec<u8>,
) -> Ack {
    send_as(
        client,
        sender,
        envelope(mode, message_type, &new_message_id(), sid, sender, payload),
    )
    .await
    .unwrap_or_else(|s| panic!("{message_type} from {sender}: transport error {s:?}"))
}

fn assert_rejected(ack: &Ack, code: &str, what: &str) {
    assert!(!ack.ok, "{what}: must be rejected, got ok=true");
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some(code),
        "{what}: wrong error, got {:?}",
        ack.error
    );
}

fn assert_accepted(ack: &Ack, what: &str) {
    assert!(ack.ok, "{what}: must be accepted, got {:?}", ack.error);
}

async fn state(client: &mut MacpRuntimeServiceClient<Channel>, sender: &str, sid: &str) -> i32 {
    get_session_as(client, sender, sid)
        .await
        .expect("GetSession")
        .metadata
        .expect("session metadata")
        .state
}

async fn start(
    client: &mut MacpRuntimeServiceClient<Channel>,
    mode: &str,
    initiator: &str,
    participants: &[&str],
) -> String {
    let sid = new_session_id();
    let ack = send(
        client,
        mode,
        &sid,
        initiator,
        "SessionStart",
        session_start_payload("readiness", participants, 60_000),
    )
    .await;
    assert_accepted(&ack, "SessionStart");
    sid
}

fn handoff_commit(id: &str, positive: bool) -> Vec<u8> {
    let action = if positive {
        "handoff.accepted"
    } else {
        "handoff.declined"
    };
    commitment_payload(id, action, "test", "readiness", positive)
}

// ── Handoff Commitment readiness (RFC-MACP-0010 §6.1) ───────────────────

#[tokio::test]
async fn handoff_commitment_needs_settled_offers() {
    let mut c = common::grpc_client().await;
    let sid = start(&mut c, MODE_HANDOFF, OWNER, &[OWNER, TARGET, TARGET2]).await;

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "HandoffOffer",
        handoff_offer_payload("h1", TARGET, "support", "escalate"),
    )
    .await;
    assert_accepted(&ack, "HandoffOffer h1");

    // h1 pending: neither outcome is ready.
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c1", true),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "positive Commitment with h1 pending",
    );
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c2", false),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "negative Commitment with h1 pending",
    );

    // h1 declined: no accepted offer, so still not positive-ready.
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        TARGET,
        "HandoffDecline",
        handoff_decline_payload("h1", TARGET, "out of scope"),
    )
    .await;
    assert_accepted(&ack, "HandoffDecline h1");
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c3", true),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "positive Commitment after only a decline",
    );

    // h2 accepted: an offer was accepted, so the negative outcome is refused
    // and the positive one resolves the session.
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "HandoffOffer",
        handoff_offer_payload("h2", TARGET2, "support", "escalate"),
    )
    .await;
    assert_accepted(&ack, "HandoffOffer h2");
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        TARGET2,
        "HandoffAccept",
        handoff_accept_payload("h2", TARGET2, "ready"),
    )
    .await;
    assert_accepted(&ack, "HandoffAccept h2");
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c4", false),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "negative Commitment after an accept",
    );
    assert_eq!(state(&mut c, OWNER, &sid).await, SessionState::Open as i32);

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c5", true),
    )
    .await;
    assert_accepted(&ack, "positive Commitment after an accept");
    assert_eq!(
        state(&mut c, OWNER, &sid).await,
        SessionState::Resolved as i32
    );
}

#[tokio::test]
async fn handoff_negative_commitment_when_every_offer_declined() {
    let mut c = common::grpc_client().await;
    let sid = start(&mut c, MODE_HANDOFF, OWNER, &[OWNER, TARGET]).await;

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "HandoffOffer",
        handoff_offer_payload("h1", TARGET, "support", "escalate"),
    )
    .await;
    assert_accepted(&ack, "HandoffOffer");
    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        TARGET,
        "HandoffDecline",
        handoff_decline_payload("h1", TARGET, "out of scope"),
    )
    .await;
    assert_accepted(&ack, "HandoffDecline");

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c1", false),
    )
    .await;
    assert_accepted(&ack, "negative Commitment with every offer declined");
    assert_eq!(
        state(&mut c, OWNER, &sid).await,
        SessionState::Resolved as i32
    );
}

#[tokio::test]
async fn handoff_commitment_with_no_offers() {
    let mut c = common::grpc_client().await;
    let sid = start(&mut c, MODE_HANDOFF, OWNER, &[OWNER, TARGET]).await;

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c1", true),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "positive Commitment with no offer",
    );
    assert_eq!(state(&mut c, OWNER, &sid).await, SessionState::Open as i32);

    let ack = send(
        &mut c,
        MODE_HANDOFF,
        &sid,
        OWNER,
        "Commitment",
        handoff_commit("c2", false),
    )
    .await;
    assert_accepted(&ack, "negative Commitment with no offer");
    assert_eq!(
        state(&mut c, OWNER, &sid).await,
        SessionState::Resolved as i32
    );
}

// ── Annotations via Send are FORBIDDEN (RFC-MACP-0001 §7.6 item 9) ──────

#[tokio::test]
async fn runtime_annotations_via_send_are_forbidden_in_every_mode() {
    let mut c = common::grpc_client().await;
    let initiator = "agent://initiator";
    let peer = "agent://peer";

    for mode in [
        MODE_DECISION,
        MODE_PROPOSAL,
        MODE_TASK,
        MODE_HANDOFF,
        MODE_QUORUM,
    ] {
        let sid = start(&mut c, mode, initiator, &[initiator, peer]).await;

        for (message_type, payload) in [
            ("SessionCancel", session_cancel_payload("abort", initiator)),
            (
                "SessionSuspend",
                session_suspend_payload("pause", initiator),
            ),
            (
                "SessionResume",
                session_resume_payload("continue", initiator),
            ),
        ] {
            let ack = send(&mut c, mode, &sid, initiator, message_type, payload).await;
            assert_rejected(
                &ack,
                "FORBIDDEN",
                &format!("{message_type} via Send in {mode}"),
            );
        }
        assert_eq!(
            state(&mut c, initiator, &sid).await,
            SessionState::Open as i32,
            "{mode}: a refused annotation must not move the lifecycle"
        );

        // The RPC path is the one that appends the annotation, and still works.
        let ack = cancel_session_as(&mut c, initiator, &sid, "done")
            .await
            .expect("CancelSession transport");
        assert_accepted(&ack, &format!("{mode}: CancelSession RPC"));
        assert_ne!(
            state(&mut c, initiator, &sid).await,
            SessionState::Open as i32,
            "{mode}: CancelSession must end the session"
        );
    }
}

// ── Task reassignment denial (RFC-MACP-0009 §5 rule 3c, 0.13.2) ─────────

#[tokio::test]
async fn active_assignee_reject_without_reassignment_policy_is_invalid_envelope() {
    let mut c = common::grpc_client().await;
    let planner = "agent://planner";
    let worker = "agent://w1";
    let sid = start(&mut c, MODE_TASK, planner, &[planner, worker]).await;

    let ack = send(
        &mut c,
        MODE_TASK,
        &sid,
        planner,
        "TaskRequest",
        task_request_payload("t1", "Build", "Do it", worker),
    )
    .await;
    assert_accepted(&ack, "TaskRequest");
    let ack = send(
        &mut c,
        MODE_TASK,
        &sid,
        worker,
        "TaskAccept",
        task_accept_payload("t1", worker),
    )
    .await;
    assert_accepted(&ack, "TaskAccept");

    // No policy is bound, so allow_reassignment_on_reject is false and the
    // active assignee cannot hand the task back. This is a mode-rule refusal,
    // not a policy denial.
    let ack = send(
        &mut c,
        MODE_TASK,
        &sid,
        worker,
        "TaskReject",
        task_reject_payload("t1", worker, "busy"),
    )
    .await;
    assert_rejected(
        &ack,
        "INVALID_ENVELOPE",
        "active assignee TaskReject without reassignment policy",
    );
    assert_eq!(
        state(&mut c, planner, &sid).await,
        SessionState::Open as i32
    );
}
