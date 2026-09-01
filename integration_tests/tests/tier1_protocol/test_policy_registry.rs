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

// ── Reserved `policy.std.` namespace (RFC-MACP-0012 §2.2, §5.2, §7) ──

const STD_MAJORITY: &str = "policy.std.majority";
const STD_SUPERMAJORITY: &str = "policy.std.supermajority";
const STD_UNANIMOUS: &str = "policy.std.unanimous";

#[tokio::test]
async fn list_policies_includes_the_std_profiles() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-std-lister";

    let resp = client
        .list_policies(with_sender(
            agent,
            ListPoliciesRequest {
                mode: "macp.mode.decision.v1".into(),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    for expected in [STD_MAJORITY, STD_SUPERMAJORITY, STD_UNANIMOUS] {
        let found = resp
            .descriptors
            .iter()
            .find(|d| d.policy_id == expected)
            .unwrap_or_else(|| panic!("{expected} missing from ListPolicies"));
        assert_eq!(found.mode, "macp.mode.decision.v1", "{expected}");
        assert_eq!(found.schema_version, 1, "{expected}");
        let rules: serde_json::Value = serde_json::from_str(&found.rules).unwrap();
        assert_eq!(
            rules["commitment"]["require_vote_quorum"],
            serde_json::json!(true),
            "{expected} must set require_vote_quorum"
        );
    }

    // The §5.2 supermajority threshold survives the JSON round trip through
    // the wire as the binary64 value nearest two-thirds.
    let super_rules: serde_json::Value = serde_json::from_str(
        &resp
            .descriptors
            .iter()
            .find(|d| d.policy_id == STD_SUPERMAJORITY)
            .unwrap()
            .rules,
    )
    .unwrap();
    assert_eq!(
        super_rules["voting"]["threshold"].as_f64().unwrap(),
        2.0f64 / 3.0f64
    );
}

#[tokio::test]
async fn register_non_canonical_std_policy_is_rejected() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-std-hijack";

    // An assigned reserved identifier carrying rules that are not the
    // canonical §5.2 ones.
    let descriptor = test_descriptor(
        STD_MAJORITY,
        "macp.mode.decision.v1",
        serde_json::json!({
            "voting": { "algorithm": "plurality" },
            "commitment": { "require_vote_quorum": false }
        }),
    );

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
    assert!(!resp.ok, "reserved namespace registration should fail");
    assert!(
        resp.error.contains("INVALID_POLICY_DEFINITION"),
        "error should carry INVALID_POLICY_DEFINITION, got: {}",
        resp.error
    );

    // The pre-registered profile is untouched.
    let fetched = client
        .get_policy(with_sender(
            agent,
            GetPolicyRequest {
                policy_id: STD_MAJORITY.into(),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .policy_descriptor
        .expect("profile still present");
    let rules: serde_json::Value = serde_json::from_str(&fetched.rules).unwrap();
    assert_eq!(rules["voting"]["algorithm"], "majority");
}

#[tokio::test]
async fn register_unassigned_std_policy_is_rejected_and_does_not_resolve() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-std-unassigned";

    let descriptor = test_descriptor(
        "policy.std.nonesuch",
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "none" } }),
    );
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
    assert!(!resp.ok, "unassigned reserved id should not register");
    assert!(
        resp.error.contains("INVALID_POLICY_DEFINITION"),
        "error should carry INVALID_POLICY_DEFINITION, got: {}",
        resp.error
    );

    // And it must not resolve at SessionStart — §2.2 requires
    // UNKNOWN_POLICY_VERSION for a reserved id the runtime does not provide.
    let session_id = new_session_id();
    let sender = "agent://policy-std-unassigned-orch";
    let start_payload = SessionStartPayload {
        intent: "unassigned reserved policy".into(),
        participants: vec![sender.into()],
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: "policy.std.nonesuch".into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        sender,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &session_id,
            sender,
            start_payload,
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok);
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("UNKNOWN_POLICY_VERSION"),
        "got: {:?}",
        ack.error
    );
}

#[tokio::test]
async fn unregister_std_profile_fails() {
    let mut client = common::grpc_client().await;
    let agent = "agent://policy-std-unreg";

    for policy_id in [STD_MAJORITY, STD_SUPERMAJORITY, STD_UNANIMOUS] {
        let resp = client
            .unregister_policy(with_sender(
                agent,
                UnregisterPolicyRequest {
                    policy_id: policy_id.into(),
                },
            ))
            .await
            .unwrap()
            .into_inner();
        assert!(!resp.ok, "{policy_id} must not be unregisterable");

        // Still resolvable afterwards.
        assert!(
            client
                .get_policy(with_sender(
                    agent,
                    GetPolicyRequest {
                        policy_id: policy_id.into(),
                    },
                ))
                .await
                .is_ok(),
            "{policy_id} was removed"
        );
    }
}

// ── §5.2 outcome table, end to end through the gRPC boundary ────────

/// Run a Decision session bound to `policy_id`: the orchestrator proposes,
/// `approve` agents vote APPROVE, `reject` agents vote REJECT, `silent` agents
/// are declared participants that never vote. Returns the Commitment ack.
///
/// The orchestrator is itself a declared participant (RFC-MACP-0007 §2) and
/// stays silent unless `initiator_approves` is set — which matters only for
/// `policy.std.unanimous`, the one profile that counts declared participants
/// rather than decisive votes.
async fn commit_under_std_policy(
    policy_id: &str,
    approve: usize,
    reject: usize,
    silent: usize,
    initiator_approves: bool,
) -> macp_runtime::pb::Ack {
    let mut client = common::grpc_client().await;
    let run = uuid::Uuid::new_v4().as_simple().to_string();
    let orchestrator = format!("agent://std-orch-{run}");
    let session_id = new_session_id();

    let voters: Vec<(String, &str)> = (0..approve)
        .map(|i| (format!("agent://std-a{i}-{run}"), "approve"))
        .chain((0..reject).map(|i| (format!("agent://std-r{i}-{run}"), "reject")))
        .collect();
    let silent_agents: Vec<String> = (0..silent)
        .map(|i| format!("agent://std-s{i}-{run}"))
        .collect();

    let mut participants = vec![orchestrator.clone()];
    participants.extend(voters.iter().map(|(id, _)| id.clone()));
    participants.extend(silent_agents.iter().cloned());

    let start_payload = SessionStartPayload {
        intent: format!("std profile check: {policy_id}"),
        participants: participants.clone(),
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: policy_id.into(),
        ttl_ms: 120_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        &orchestrator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &session_id,
            &orchestrator,
            start_payload,
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "SessionStart rejected: {:?}", ack.error);

    let ack = send_as(
        &mut client,
        &orchestrator,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &session_id,
            &orchestrator,
            proposal_payload("p1", "ship", "ready"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "Proposal rejected: {:?}", ack.error);

    let mut voters = voters;
    if initiator_approves {
        voters.push((orchestrator.clone(), "approve"));
    }

    for (voter, vote) in &voters {
        let ack = send_as(
            &mut client,
            voter,
            envelope(
                MODE_DECISION,
                "Vote",
                &new_message_id(),
                &session_id,
                voter,
                vote_payload("p1", vote, "recorded"),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "Vote from {voter} rejected: {:?}", ack.error);
    }

    let commit_payload = macp_runtime::pb::CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "test".into(),
        reason: "bound".into(),
        mode_version: MODE_VERSION.into(),
        policy_version: policy_id.into(),
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();

    send_as(
        &mut client,
        &orchestrator,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &session_id,
            &orchestrator,
            commit_payload,
        ),
    )
    .await
    .unwrap()
}

fn assert_policy_denied(ack: &macp_runtime::pb::Ack, label: &str) {
    assert!(!ack.ok, "{label}: commitment should have been denied");
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("POLICY_DENIED"),
        "{label}: got {:?}",
        ack.error
    );
}

#[tokio::test]
async fn std_majority_approves_an_even_split() {
    // §5.2: the comparison is inclusive, so 1-of-2 decisive votes approves.
    let ack = commit_under_std_policy(STD_MAJORITY, 1, 1, 0, false).await;
    assert!(ack.ok, "even split should approve: {:?}", ack.error);
}

#[tokio::test]
async fn std_majority_denies_a_minority() {
    let ack = commit_under_std_policy(STD_MAJORITY, 1, 2, 0, false).await;
    assert_policy_denied(&ack, "1 of 3");
}

#[tokio::test]
async fn std_supermajority_matches_the_rfc_outcome_table() {
    // §5.2 determinism note: 2/3, 4/6, 20/30 and 67/100 clear the binary64
    // two-thirds bar; 66/100 does not.
    for (approve, total) in [(2usize, 3usize), (4, 6), (20, 30), (67, 100)] {
        let ack = commit_under_std_policy(STD_SUPERMAJORITY, approve, total - approve, 0, false).await;
        assert!(
            ack.ok,
            "{approve} of {total} should approve: {:?}",
            ack.error
        );
    }
    let ack = commit_under_std_policy(STD_SUPERMAJORITY, 66, 34, 0, false).await;
    assert_policy_denied(&ack, "66 of 100");
}

#[tokio::test]
async fn std_supermajority_denies_a_single_voter() {
    // The profile's `quorum: { count: 2 }` is binding because
    // `require_vote_quorum` is true.
    let ack = commit_under_std_policy(STD_SUPERMAJORITY, 1, 0, 0, false).await;
    assert_policy_denied(&ack, "single voter");
}

#[tokio::test]
async fn std_unanimous_is_blocked_by_a_silent_declared_participant() {
    // §4.1: unanimous is stricter than "all decisive votes approve" — a
    // declared participant who has not voted blocks the commitment.
    let ack = commit_under_std_policy(STD_UNANIMOUS, 2, 0, 1, true).await;
    assert_policy_denied(&ack, "silent declared participant");
}

#[tokio::test]
async fn std_unanimous_is_blocked_by_the_silent_initiator() {
    // The initiator is a declared participant too (RFC-MACP-0007 §2), so it
    // must vote for unanimity to hold. Here nobody is silent except it.
    let ack = commit_under_std_policy(STD_UNANIMOUS, 2, 0, 0, false).await;
    assert_policy_denied(&ack, "silent initiator");
}

#[tokio::test]
async fn std_unanimous_approves_when_every_participant_approves() {
    let ack = commit_under_std_policy(STD_UNANIMOUS, 2, 0, 0, true).await;
    assert!(
        ack.ok,
        "every declared participant approved: {:?}",
        ack.error
    );
}

#[tokio::test]
async fn std_unanimous_is_blocked_by_a_single_reject() {
    let ack = commit_under_std_policy(STD_UNANIMOUS, 2, 1, 0, true).await;
    assert_policy_denied(&ack, "one reject");
}
