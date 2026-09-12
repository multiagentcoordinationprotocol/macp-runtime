use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::Envelope;

#[tokio::test]
async fn invalid_macp_version_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    let env = Envelope {
        macp_version: "2.0".into(), // unsupported
        mode: MODE_DECISION.into(),
        message_type: "SessionStart".into(),
        message_id: new_message_id(),
        session_id: sid,
        sender: agent.into(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: session_start_payload("bad version", &[agent], 30_000),
    };

    let ack = send_as(&mut client, agent, env).await.unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn empty_mode_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    let env = envelope(
        "",
        "SessionStart",
        &new_message_id(),
        &sid,
        agent,
        session_start_payload("empty mode", &[agent], 30_000),
    );

    let ack = send_as(&mut client, agent, env).await.unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn session_start_without_participants_rejected() {
    // Re-based from `MODE_DECISION` to `MODE_PROPOSAL`: Decision now accepts
    // an empty roster (see `decision_session_start_with_zero_participants` in
    // `test_decision_mode.rs`), so this test would otherwise have inverted.
    // Kept on a sibling standards-track mode because the rule it pins — an
    // empty `participants` list is refused at `SessionStart` — still holds for
    // every mode except the one carve-out, and this is the only place the wire
    // asserts it.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    let env = envelope(
        MODE_PROPOSAL,
        "SessionStart",
        &new_message_id(),
        &sid,
        agent,
        session_start_payload("no participants", &[], 30_000),
    );

    let ack = send_as(&mut client, agent, env).await.unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn session_start_without_ttl_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    let env = envelope(
        MODE_DECISION,
        "SessionStart",
        &new_message_id(),
        &sid,
        agent,
        session_start_payload("no ttl", &[agent], 0), // zero TTL
    );

    let ack = send_as(&mut client, agent, env).await.unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn non_participant_cannot_send() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";
    let outsider = "agent://outsider";

    // Start session with coord + voter
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("restricted", &[coord, voter], 30_000),
        ),
    )
    .await
    .unwrap();

    // Outsider tries to send
    let ack = send_as(
        &mut client,
        outsider,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            outsider,
            proposal_payload("p1", "sneaky", "unauthorized"),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn send_to_nonexistent_session_fails() {
    let mut client = common::grpc_client().await;
    let agent = "agent://test";

    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &new_session_id(), // session doesn't exist
            agent,
            proposal_payload("p1", "orphan", "no session"),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn unknown_mode_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    let env = envelope(
        "macp.mode.nonexistent.v99",
        "SessionStart",
        &new_message_id(),
        &sid,
        agent,
        session_start_payload("unknown mode", &[agent], 30_000),
    );

    let ack = send_as(&mut client, agent, env).await.unwrap();
    assert!(!ack.ok);
}
