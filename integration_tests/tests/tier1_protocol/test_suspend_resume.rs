//! SuspendSession / ResumeSession (RFC-MACP-0001 §7.5): same authority model
//! as CancelSession — initiator or policy-delegated roles only.

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::{ResumeSessionRequest, SuspendSessionRequest};

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
