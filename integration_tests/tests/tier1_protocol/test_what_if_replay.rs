//! `WhatIfReplay` (RFC-MACP-0012 §8.1) through the real gRPC boundary, on the
//! durable file backend — the backend where a terminal session's log is
//! compacted away unless `MACP_COMPACT_TERMINAL_LOGS=0` keeps it.

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{PolicyDescriptor, WhatIfReplayRequest, WhatIfReplayResponse};
use tonic::transport::Channel;

const BUYER: &str = "agent://buyer";
const SELLER: &str = "agent://seller";

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

/// SessionStart(0) → Proposal(1) → Accept(2) → Accept(3) → Commitment(4).
async fn resolved_proposal_session(client: &mut MacpRuntimeServiceClient<Channel>) -> String {
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
            client,
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
    sid
}

async fn what_if(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sid: &str,
) -> Result<WhatIfReplayResponse, tonic::Status> {
    let candidate = PolicyDescriptor {
        policy_id: "what-if.seller-commits".into(),
        mode: MODE_PROPOSAL.into(),
        description: "only the seller may commit".into(),
        rules: serde_json::json!({
            "commitment": { "authority": "designated_role", "designated_roles": [SELLER] }
        })
        .to_string(),
        schema_version: 1,
        ..Default::default()
    };
    client
        .what_if_replay(with_sender(
            BUYER,
            WhatIfReplayRequest {
                session_id: sid.into(),
                candidate: Some(candidate),
            },
        ))
        .await
        .map(|r| r.into_inner())
}

#[tokio::test]
async fn what_if_replays_a_resolved_session_kept_across_a_restart() {
    let binary = test_binary();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_dir_str = data_dir.path().to_string_lossy().to_string();
    let env = [
        ("MACP_MEMORY_ONLY", "0"),
        ("MACP_DATA_DIR", data_dir_str.as_str()),
        ("MACP_COMPACT_TERMINAL_LOGS", "0"),
    ];

    let sid = {
        let mut manager = ServerManager::start_with_env(&binary, &env)
            .await
            .expect("first server must start");
        let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
            .await
            .expect("connect");
        let sid = resolved_proposal_session(&mut client).await;
        manager.stop();
        sid
    };

    let manager = ServerManager::start_with_env(&binary, &env)
        .await
        .expect("second server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");
    let resp = what_if(&mut client, &sid).await.expect("what-if");

    assert_eq!(resp.recorded_policy_id, "policy.default");
    assert_eq!(resp.candidate_policy_id, "what-if.seller-commits");
    assert_eq!(resp.first_divergence, Some(4));
    let recorded = resp.recorded.unwrap();
    assert_eq!(recorded.state, 2, "recorded: RESOLVED");
    assert_eq!(recorded.resolved_by_history_index, 4);
    assert_eq!(resp.candidate.unwrap().state, 1, "candidate: still OPEN");
    let indices: Vec<_> = resp.entries.iter().map(|e| e.history_index).collect();
    assert_eq!(indices, vec![0, 1, 2, 3, 4]);

    // Read-only: the stored session is unchanged.
    let meta = get_session_as(&mut client, BUYER, &sid)
        .await
        .unwrap()
        .metadata
        .unwrap();
    assert_eq!(meta.state, 2);
}

#[tokio::test]
async fn what_if_refuses_a_compacted_session() {
    let binary = test_binary();
    let data_dir = tempfile::tempdir().expect("tempdir");
    let data_dir_str = data_dir.path().to_string_lossy().to_string();
    let manager = ServerManager::start_with_env(
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
    let sid = resolved_proposal_session(&mut client).await;
    let err = what_if(&mut client, &sid).await.unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::FailedPrecondition,
        "{}",
        err.message()
    );
    assert_eq!(
        err.metadata()
            .get("macp-error-code")
            .and_then(|v| v.to_str().ok()),
        Some("HISTORY_UNAVAILABLE")
    );
}
