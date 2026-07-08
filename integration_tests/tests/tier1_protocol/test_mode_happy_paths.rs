//! One gRPC-boundary happy path per mode (plus the StreamSession smoke test).
//!
//! Deeper per-mode behavior lives in the `macp-modes` unit tests and the
//! JSON conformance fixtures under `tests/conformance/`; these tests exist to
//! prove each mode works end-to-end through the real transport (auth header →
//! envelope validation → kernel → mode → ack). They were consolidated from
//! six single-test files to keep the suite navigable.

use crate::common;
use macp_integration_tests::helpers::*;

#[tokio::test]
async fn stream_receives_accepted_envelopes() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";

    // Start session via unary Send
    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("stream test", &[coord, voter], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Send a proposal to generate an accepted envelope
    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "option-A", "stream test proposal"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Verify session is still open and has accepted messages
    let resp = get_session_as(&mut client, coord, &sid).await.unwrap();
    let meta = resp.metadata.expect("metadata present");
    assert_eq!(meta.state, 1); // OPEN
}

#[tokio::test]
async fn proposal_happy_path() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let buyer = "agent://buyer";
    let seller = "agent://seller";

    // SessionStart
    let ack = send_as(
        &mut client,
        buyer,
        envelope(
            MODE_PROPOSAL,
            "SessionStart",
            &new_message_id(),
            &sid,
            buyer,
            session_start_payload("negotiate price", &[buyer, seller], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Proposal from seller
    let ack = send_as(
        &mut client,
        seller,
        envelope(
            MODE_PROPOSAL,
            "Proposal",
            &new_message_id(),
            &sid,
            seller,
            proposal_mode_payload("prop-1", "Initial offer", "$100"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // CounterProposal from buyer
    let ack = send_as(
        &mut client,
        buyer,
        envelope(
            MODE_PROPOSAL,
            "CounterProposal",
            &new_message_id(),
            &sid,
            buyer,
            counter_proposal_payload("prop-2", "prop-1", "Counter offer", "$80"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Accept from seller
    let ack = send_as(
        &mut client,
        seller,
        envelope(
            MODE_PROPOSAL,
            "Accept",
            &new_message_id(),
            &sid,
            seller,
            accept_proposal_payload("prop-2", "acceptable price"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Accept from buyer
    let ack = send_as(
        &mut client,
        buyer,
        envelope(
            MODE_PROPOSAL,
            "Accept",
            &new_message_id(),
            &sid,
            buyer,
            accept_proposal_payload("prop-2", "agreed"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Commitment from buyer (initiator)
    let ack = send_as(
        &mut client,
        buyer,
        envelope(
            MODE_PROPOSAL,
            "Commitment",
            &new_message_id(),
            &sid,
            buyer,
            commitment_payload("c1", "accept-counter", "negotiation", "both accepted", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);
    assert_eq!(ack.session_state, 2);
}

#[tokio::test]
async fn quorum_happy_path() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let requester = "agent://requester";
    let approver1 = "agent://approver1";
    let approver2 = "agent://approver2";
    let approver3 = "agent://approver3";

    // SessionStart
    let ack = send_as(
        &mut client,
        requester,
        envelope(
            MODE_QUORUM,
            "SessionStart",
            &new_message_id(),
            &sid,
            requester,
            session_start_payload(
                "approve deployment",
                &[requester, approver1, approver2, approver3],
                30_000,
            ),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // ApprovalRequest from requester (need 2 of 3 approvers)
    let ack = send_as(
        &mut client,
        requester,
        envelope(
            MODE_QUORUM,
            "ApprovalRequest",
            &new_message_id(),
            &sid,
            requester,
            approval_request_payload("r1", "deploy-prod", "Deploy v2 to production", 2),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Approve from approver1
    let ack = send_as(
        &mut client,
        approver1,
        envelope(
            MODE_QUORUM,
            "Approve",
            &new_message_id(),
            &sid,
            approver1,
            approve_payload("r1", "LGTM"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Approve from approver2 (quorum reached)
    let ack = send_as(
        &mut client,
        approver2,
        envelope(
            MODE_QUORUM,
            "Approve",
            &new_message_id(),
            &sid,
            approver2,
            approve_payload("r1", "Approved"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Commitment from requester
    let ack = send_as(
        &mut client,
        requester,
        envelope(
            MODE_QUORUM,
            "Commitment",
            &new_message_id(),
            &sid,
            requester,
            commitment_payload("c1", "deploy-prod", "ops-team", "quorum reached", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);
    assert_eq!(ack.session_state, 2);
}

#[tokio::test]
async fn multi_round_happy_path() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent_a = "agent://agent-a";
    let agent_b = "agent://agent-b";

    // SessionStart
    let ack = send_as(
        &mut client,
        agent_a,
        envelope(
            MODE_MULTI_ROUND,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent_a,
            session_start_payload("converge on value", &[agent_a, agent_b], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Round 1: Contribute from both agents
    let ack = send_as(
        &mut client,
        agent_a,
        envelope(
            MODE_MULTI_ROUND,
            "Contribute",
            &new_message_id(),
            &sid,
            agent_a,
            serde_json::to_vec(&serde_json::json!({
                "value": "alpha"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let ack = send_as(
        &mut client,
        agent_b,
        envelope(
            MODE_MULTI_ROUND,
            "Contribute",
            &new_message_id(),
            &sid,
            agent_b,
            serde_json::to_vec(&serde_json::json!({
                "value": "beta"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Round 2: Revised contributions
    let ack = send_as(
        &mut client,
        agent_a,
        envelope(
            MODE_MULTI_ROUND,
            "Contribute",
            &new_message_id(),
            &sid,
            agent_a,
            serde_json::to_vec(&serde_json::json!({
                "value": "converged"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let ack = send_as(
        &mut client,
        agent_b,
        envelope(
            MODE_MULTI_ROUND,
            "Contribute",
            &new_message_id(),
            &sid,
            agent_b,
            serde_json::to_vec(&serde_json::json!({
                "value": "converged"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Commitment
    let ack = send_as(
        &mut client,
        agent_a,
        envelope(
            MODE_MULTI_ROUND,
            "Commitment",
            &new_message_id(),
            &sid,
            agent_a,
            commitment_payload("c1", "converged", "group", "all agreed", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);
    assert_eq!(ack.session_state, 2);
}

#[tokio::test]
async fn handoff_happy_path() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let source = "agent://source";
    let target = "agent://target";

    // SessionStart
    let ack = send_as(
        &mut client,
        source,
        envelope(
            MODE_HANDOFF,
            "SessionStart",
            &new_message_id(),
            &sid,
            source,
            session_start_payload("handoff customer", &[source, target], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // HandoffOffer from source
    let ack = send_as(
        &mut client,
        source,
        envelope(
            MODE_HANDOFF,
            "HandoffOffer",
            &new_message_id(),
            &sid,
            source,
            handoff_offer_payload("h1", target, "customer-support", "escalation needed"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // HandoffContext from source
    let ack = send_as(
        &mut client,
        source,
        envelope(
            MODE_HANDOFF,
            "HandoffContext",
            &new_message_id(),
            &sid,
            source,
            handoff_context_payload("h1", "application/json", b"{\"customer_id\": 42}"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // HandoffAccept from target
    let ack = send_as(
        &mut client,
        target,
        envelope(
            MODE_HANDOFF,
            "HandoffAccept",
            &new_message_id(),
            &sid,
            target,
            handoff_accept_payload("h1", target, "ready to assist"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Commitment from source
    let ack = send_as(
        &mut client,
        source,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &sid,
            source,
            commitment_payload("c1", "handoff-complete", "support", "transferred", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);
    assert_eq!(ack.session_state, 2);
}

#[tokio::test]
async fn task_happy_path() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let planner = "agent://planner";
    let worker = "agent://worker";

    // SessionStart
    let ack = send_as(
        &mut client,
        planner,
        envelope(
            MODE_TASK,
            "SessionStart",
            &new_message_id(),
            &sid,
            planner,
            session_start_payload("delegate analysis", &[planner, worker], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // TaskRequest from planner
    let ack = send_as(
        &mut client,
        planner,
        envelope(
            MODE_TASK,
            "TaskRequest",
            &new_message_id(),
            &sid,
            planner,
            task_request_payload("t1", "Analyze data", "Run the analysis pipeline", worker),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // TaskAccept from worker
    let ack = send_as(
        &mut client,
        worker,
        envelope(
            MODE_TASK,
            "TaskAccept",
            &new_message_id(),
            &sid,
            worker,
            task_accept_payload("t1", worker),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // TaskUpdate from worker
    let ack = send_as(
        &mut client,
        worker,
        envelope(
            MODE_TASK,
            "TaskUpdate",
            &new_message_id(),
            &sid,
            worker,
            task_update_payload("t1", "in_progress", 0.5, "halfway done"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // TaskComplete from worker
    let ack = send_as(
        &mut client,
        worker,
        envelope(
            MODE_TASK,
            "TaskComplete",
            &new_message_id(),
            &sid,
            worker,
            task_complete_payload("t1", worker, "analysis complete"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Commitment from planner
    let ack = send_as(
        &mut client,
        planner,
        envelope(
            MODE_TASK,
            "Commitment",
            &new_message_id(),
            &sid,
            planner,
            commitment_payload("c1", "task-completed", "planner", "worker delivered", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);
    assert_eq!(ack.session_state, 2);
}
