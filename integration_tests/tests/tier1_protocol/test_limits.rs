//! Resource-limit enforcement through the gRPC boundary: payload size caps
//! and per-sender rate limits (RFC-MACP-0004 §7 DoS defenses). Each test
//! spawns its own server so the limits don't throttle the shared suite.

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

#[tokio::test]
async fn oversized_payload_rejected_with_payload_too_large() {
    let manager =
        ServerManager::start_with_env(&test_binary(), &[("MACP_MAX_PAYLOAD_BYTES", "1024")])
            .await
            .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    let sid = new_session_id();
    let agent = "agent://oversize";
    let partner = "agent://partner";

    // A normal SessionStart fits under the 1 KiB cap.
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent,
            session_start_payload("limit test", &[agent, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // A proposal with a 4 KiB rationale exceeds it.
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            agent,
            proposal_payload("p1", "big", &"x".repeat(4096)),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "oversized payload must be rejected");
    assert_eq!(ack.error.expect("error present").code, "PAYLOAD_TOO_LARGE");
}

#[tokio::test]
async fn session_start_rate_limit_enforced_per_sender() {
    let manager = ServerManager::start_with_env(
        &test_binary(),
        &[("MACP_SESSION_START_LIMIT_PER_MINUTE", "2")],
    )
    .await
    .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    let agent = "agent://rate-limited";
    let partner = "agent://partner";

    for i in 0..2 {
        let ack = send_as(
            &mut client,
            agent,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &new_session_id(),
                agent,
                session_start_payload("rate test", &[agent, partner], 60_000),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "start #{i} must be under the limit");
    }

    // Third start within the same minute trips the limiter.
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &new_session_id(),
            agent,
            session_start_payload("rate test", &[agent, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "third session start must be rate limited");
    assert_eq!(ack.error.expect("error present").code, "RATE_LIMITED");

    // The limit is per-sender: a different identity is unaffected.
    let other = "agent://unthrottled";
    let ack = send_as(
        &mut client,
        other,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &new_session_id(),
            other,
            session_start_payload("rate test", &[other, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "rate limit must not leak across senders");
}
