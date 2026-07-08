//! Storage backends exercised through the real gRPC boundary. Opt-in via
//! MACP_TEST_BACKEND=rocksdb|redis|file — the runtime binary must have been
//! built with the matching cargo feature (the CI `features` job does this;
//! the default tier-1 run skips silently). Redis additionally needs a live
//! server at MACP_TEST_REDIS_URL (default redis://127.0.0.1:6379).

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

#[tokio::test]
async fn configured_backend_persists_sessions_across_restart() {
    let Ok(backend) = std::env::var("MACP_TEST_BACKEND") else {
        eprintln!("skipping backend smoke test: MACP_TEST_BACKEND unset");
        return;
    };
    let binary = test_binary();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_dir_str = data_dir.path().to_string_lossy().to_string();
    let redis_url =
        std::env::var("MACP_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());

    let mut env: Vec<(&str, &str)> = vec![
        ("MACP_MEMORY_ONLY", "0"),
        ("MACP_STORAGE_BACKEND", backend.as_str()),
        ("MACP_DATA_DIR", data_dir_str.as_str()),
    ];
    if backend == "redis" {
        env.push(("MACP_REDIS_URL", redis_url.as_str()));
    }

    let sid = new_session_id();
    let initiator = "agent://backend-smoke";
    let partner = "agent://partner";

    {
        let mut manager = ServerManager::start_with_env(&binary, &env)
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "runtime with MACP_STORAGE_BACKEND={backend} must start \
                     (was the binary built with the matching feature?): {e}"
                )
            });
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
                session_start_payload("backend smoke", &[initiator, partner], 300_000),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "SessionStart on {backend} backend: {:?}", ack.error);

        let ack = send_as(
            &mut client,
            initiator,
            envelope(
                MODE_DECISION,
                "Proposal",
                &new_message_id(),
                &sid,
                initiator,
                proposal_payload("p1", "durable", "backend smoke"),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok);

        manager.stop();
    }

    // Restart on the same backend/data and confirm replay.
    let manager = ServerManager::start_with_env(&binary, &env)
        .await
        .expect("restarted server must come up");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");
    let resp = get_session_as(&mut client, initiator, &sid)
        .await
        .expect("session must be replayed from the backend");
    let meta = resp.metadata.expect("metadata present");
    assert_eq!(meta.state, 1, "session on {backend} must replay as OPEN");
    assert_eq!(meta.mode, MODE_DECISION);
}
