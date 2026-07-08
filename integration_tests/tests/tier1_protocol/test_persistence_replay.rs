//! Persistence and restart-replay through the real gRPC boundary: sessions,
//! accepted history, and dedup state must survive a hard server restart
//! (RFC-MACP-0003). Every other tier-1 test runs MACP_MEMORY_ONLY=1; this
//! file is the one place the durable file backend is exercised end-to-end.

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

#[tokio::test]
async fn session_history_and_dedup_survive_restart() {
    let binary = test_binary();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_dir_str = data_dir.path().to_string_lossy().to_string();
    // ServerManager defaults to MACP_MEMORY_ONLY=1; extra_env is applied
    // after the defaults, so this override enables the file backend.
    let persist_env = [
        ("MACP_MEMORY_ONLY", "0"),
        ("MACP_DATA_DIR", data_dir_str.as_str()),
    ];

    let sid = new_session_id();
    let initiator = "agent://persistent";
    let partner = "agent://partner";
    let proposal_msg_id = new_message_id();

    // ── First server lifetime: create durable state. ──────────────────────
    {
        let mut manager = ServerManager::start_with_env(&binary, &persist_env)
            .await
            .expect("first server must start");
        let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
            .await
            .expect("connect");

        let ack = send_as(
            &mut client,
            initiator,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &sid,
                initiator,
                session_start_payload("persistence test", &[initiator, partner], 300_000),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);

        let ack = send_as(
            &mut client,
            initiator,
            envelope(
                MODE_DECISION,
                "Proposal",
                &proposal_msg_id,
                &sid,
                initiator,
                proposal_payload("p1", "durable", "must survive restart"),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);

        // Hard stop (SIGKILL): durability must come from the already-acked
        // append-only log, not from any graceful-shutdown flush.
        manager.stop();
    }

    // ── Second server lifetime: replay from the same data dir. ────────────
    let manager = ServerManager::start_with_env(&binary, &persist_env)
        .await
        .expect("second server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    // Session metadata was rebuilt by replay.
    let resp = get_session_as(&mut client, initiator, &sid)
        .await
        .expect("replayed session must be queryable");
    let meta = resp.metadata.expect("metadata present");
    assert_eq!(meta.state, 1, "replayed session must still be OPEN");
    assert_eq!(meta.mode, MODE_DECISION);

    // Dedup state was rebuilt: replaying the same message_id is flagged as a
    // duplicate instead of being accepted twice.
    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &proposal_msg_id,
            &sid,
            initiator,
            proposal_payload("p1", "durable", "must survive restart"),
        ),
    )
    .await
    .unwrap();
    assert!(
        ack.duplicate,
        "message_id accepted before the restart must be deduplicated after replay"
    );

    // And the session still accepts fresh traffic.
    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            initiator,
            proposal_payload("p2", "post-restart", "new message"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "replayed session must accept new messages");
}
