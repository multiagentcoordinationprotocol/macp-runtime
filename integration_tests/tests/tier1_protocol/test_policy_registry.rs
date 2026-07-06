use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::{
    GetPolicyRequest, ListPoliciesRequest, PolicyDescriptor, RegisterPolicyRequest,
    SessionStartPayload, UnregisterPolicyRequest,
};
use prost::Message;
use tonic::Request;

fn with_sender<T>(sender: &str, inner: T) -> Request<T> {
    let mut request = Request::new(inner);
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}")
            .parse()
            .expect("valid auth header"),
    );
    request
}

fn test_descriptor(policy_id: &str, mode: &str, rules_json: serde_json::Value) -> PolicyDescriptor {
    PolicyDescriptor {
        policy_id: policy_id.into(),
        mode: mode.into(),
        description: format!("test policy {}", policy_id),
        rules: serde_json::to_string(&rules_json).unwrap(),
        schema_version: 1,
        registered_at_unix_ms: 0,
    }
}

// ── RegisterPolicy / GetPolicy / ListPolicies / UnregisterPolicy ────

#[tokio::test]
async fn register_and_get_policy() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-admin";
    let policy_id = format!("policy.test.{}", uuid::Uuid::new_v4().as_hyphenated());

    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "majority", "threshold": 0.5 } }),
    );

    // Register
    let resp = client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "register failed: {}", resp.error);

    // Get
    let resp = client
        .get_policy(with_sender(
            agent,
            GetPolicyRequest {
                policy_id: policy_id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let fetched = resp.policy_descriptor.expect("descriptor present");
    assert_eq!(fetched.policy_id, policy_id);
    assert_eq!(fetched.mode, "macp.mode.decision.v1");
    assert_eq!(fetched.schema_version, 1);
}

#[tokio::test]
async fn list_policies_includes_default_and_registered() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-lister";
    let policy_id = format!("policy.list-test.{}", uuid::Uuid::new_v4().as_hyphenated());

    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "none" } }),
    );

    client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap();

    let resp = client
        .list_policies(with_sender(
            agent,
            ListPoliciesRequest {
                mode: String::new(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let ids: Vec<&str> = resp
        .descriptors
        .iter()
        .map(|d| d.policy_id.as_str())
        .collect();
    assert!(ids.contains(&"policy.default"), "default policy missing");
    assert!(
        ids.contains(&policy_id.as_str()),
        "registered policy missing"
    );
}

#[tokio::test]
async fn list_policies_filters_by_mode() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-filter";
    let policy_id = format!(
        "policy.filter-test.{}",
        uuid::Uuid::new_v4().as_hyphenated()
    );

    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.task.v1",
        serde_json::json!({ "completion": { "require_output": true } }),
    );

    client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap();

    // Filter by task mode
    let resp = client
        .list_policies(with_sender(
            agent,
            ListPoliciesRequest {
                mode: "macp.mode.task.v1".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let ids: Vec<&str> = resp
        .descriptors
        .iter()
        .map(|d| d.policy_id.as_str())
        .collect();
    assert!(ids.contains(&policy_id.as_str()));
    // Default policy (mode="*") should also appear
    assert!(ids.contains(&"policy.default"));
}

#[tokio::test]
async fn unregister_policy_removes_it() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-unregister";
    let policy_id = format!("policy.unreg-test.{}", uuid::Uuid::new_v4().as_hyphenated());

    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "none" } }),
    );

    client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap();

    // Unregister
    let resp = client
        .unregister_policy(with_sender(
            agent,
            UnregisterPolicyRequest {
                policy_id: policy_id.clone(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);

    // GetPolicy should now fail
    let err = client
        .get_policy(with_sender(
            agent,
            GetPolicyRequest {
                policy_id: policy_id.clone(),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn unregister_default_policy_fails() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-unreg-default";

    let resp = client
        .unregister_policy(with_sender(
            agent,
            UnregisterPolicyRequest {
                policy_id: "policy.default".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.ok);
}

#[tokio::test]
async fn register_duplicate_policy_fails() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-dup";
    let policy_id = format!("policy.dup-test.{}", uuid::Uuid::new_v4().as_hyphenated());

    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "none" } }),
    );

    let resp = client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor.clone()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok);

    let resp = client
        .register_policy(with_sender(
            agent,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!resp.ok, "duplicate registration should fail");
    assert!(resp.error.contains("already registered"));
}

// ── Unknown policy_version rejection at SessionStart ────────────────

#[tokio::test]
async fn unknown_policy_version_rejects_session_start() {
    let mut client = common::grpc_client().await;
    let session_id = new_session_id();
    let sender = "agent://policy-test-orchestrator";

    let start_payload = SessionStartPayload {
        intent: "test unknown policy".into(),
        participants: vec!["agent://participant".into()],
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: "policy.nonexistent.v999".into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    let env = envelope(
        MODE_DECISION,
        "SessionStart",
        &new_message_id(),
        &session_id,
        sender,
        start_payload,
    );

    let ack = send_as(&mut client, sender, env).await.unwrap();
    assert!(!ack.ok, "should reject unknown policy_version");
    assert!(
        ack.error.as_ref().map(|e| e.code.as_str()) == Some("UNKNOWN_POLICY_VERSION"),
        "error code should be UNKNOWN_POLICY_VERSION, got: {:?}",
        ack.error
    );
}

// ── Policy enforcement: register policy → start session → verify ────

#[tokio::test]
async fn policy_enforcement_blocks_commitment_in_decision_mode() {
    let mut client = common::grpc_client().await;
    let admin = "agent://policy-enforce-admin";
    let orchestrator = "agent://policy-enforce-orchestrator";
    let participant = "agent://policy-enforce-participant";
    let session_id = new_session_id();
    let policy_id = format!("policy.enforce.{}", uuid::Uuid::new_v4().as_hyphenated());

    // 1. Register a strict policy that requires unanimous voting
    let descriptor = test_descriptor(
        &policy_id,
        "macp.mode.decision.v1",
        serde_json::json!({
            "voting": { "algorithm": "unanimous" },
            "commitment": { "require_vote_quorum": false }
        }),
    );
    let resp = client
        .register_policy(with_sender(
            admin,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(resp.ok, "policy registration failed: {}", resp.error);

    // 2. Start a decision session bound to that policy
    let start_payload = SessionStartPayload {
        intent: "test enforcement".into(),
        participants: vec![orchestrator.into(), participant.into()],
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: policy_id.clone(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &session_id,
            orchestrator,
            start_payload,
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "SessionStart should be accepted");

    // 3. Orchestrator proposes
    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &session_id,
            orchestrator,
            proposal_payload("p1", "deploy", "ready to deploy"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // 4. Participant votes REJECT
    let ack = send_as(
        &mut client,
        participant,
        envelope(
            MODE_DECISION,
            "Vote",
            &new_message_id(),
            &session_id,
            participant,
            vote_payload("p1", "reject", "not ready"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // 5. Commitment should be DENIED by policy (unanimous requires all approve)
    let commit_payload = macp_runtime::pb::CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "test".into(),
        reason: "bound".into(),
        mode_version: MODE_VERSION.into(),
        policy_version: policy_id,
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &session_id,
            orchestrator,
            commit_payload,
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "commitment should be denied by unanimous policy");
    assert!(
        ack.error.as_ref().map(|e| e.code.as_str()) == Some("POLICY_DENIED"),
        "error code should be POLICY_DENIED, got: {:?}",
        ack.error
    );
}

#[tokio::test]
async fn default_policy_allows_commitment() {
    let mut client = common::grpc_client().await;
    let orchestrator = "agent://default-pol-orch";
    let participant = "agent://default-pol-part";
    let session_id = new_session_id();

    // Start session with default policy (policy.default)
    let start_payload =
        session_start_payload("test default policy", &[orchestrator, participant], 60_000);
    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &session_id,
            orchestrator,
            start_payload,
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Proposal + Vote + Commitment (standard happy path)
    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &session_id,
            orchestrator,
            proposal_payload("p1", "deploy", "go"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let ack = send_as(
        &mut client,
        participant,
        envelope(
            MODE_DECISION,
            "Vote",
            &new_message_id(),
            &session_id,
            participant,
            vote_payload("p1", "approve", "ok"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let ack = send_as(
        &mut client,
        orchestrator,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &session_id,
            orchestrator,
            commitment_payload("c1", "decision.selected", "test", "bound", true),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "default policy should allow commitment");
}
