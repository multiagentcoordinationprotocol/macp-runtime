use chrono::Utc;
use macp_runtime::log_store::LogStore;
use macp_runtime::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::{Session, SessionState};
use macp_runtime::storage::MemoryBackend;
use prost::Message;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

#[derive(Deserialize)]
struct ConformanceFixture {
    mode: String,
    initiator: String,
    participants: Vec<String>,
    mode_version: String,
    configuration_version: String,
    policy_version: String,
    /// Optional inline governance policy bound to the session. When present it
    /// is registered before `SessionStart` so a non-default `policy_version`
    /// resolves (RFC-MACP-0012). Deserializes straight into `PolicyDefinition`.
    #[serde(default)]
    policy: Option<Value>,
    ttl_ms: i64,
    messages: Vec<ConformanceMessage>,
    expected_final_state: String,
    #[serde(default)]
    expected_mode_state: Option<Value>,
    #[serde(default)]
    expected_resolution: Option<Value>,
    #[serde(default)]
    expect_resolution_present: Option<bool>,
    #[serde(default = "default_true")]
    verify_replay_equivalence: bool,
}

#[derive(Deserialize)]
struct ConformanceMessage {
    sender: String,
    message_type: String,
    payload_type: String,
    payload: Value,
    expect: String,
    #[serde(default)]
    expected_error_code: Option<String>,
}

fn default_true() -> bool {
    true
}

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn make_runtime() -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    Runtime::new(storage, registry, log_store)
}

fn encode_payload(fixture: &ConformanceFixture, msg: &ConformanceMessage) -> Vec<u8> {
    match msg.payload_type.as_str() {
        // Canonical fully-qualified protobuf message names (conformance-pack
        // format): mode payloads are macp.modes.<mode>.v1.<Type>Payload and
        // the shared terminal is macp.v1.CommitmentPayload.
        "macp.v1.CommitmentPayload" => {
            let p = &msg.payload;
            CommitmentPayload {
                commitment_id: p["commitment_id"].as_str().unwrap_or_default().into(),
                action: p["action"].as_str().unwrap_or_default().into(),
                authority_scope: p["authority_scope"].as_str().unwrap_or_default().into(),
                reason: p["reason"].as_str().unwrap_or_default().into(),
                mode_version: p["mode_version"].as_str().unwrap_or_default().into(),
                policy_version: p["policy_version"].as_str().unwrap_or_default().into(),
                configuration_version: p["configuration_version"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                outcome_positive: p["outcome_positive"].as_bool().unwrap_or(true),
                supersedes: None,
            }
            .encode_to_vec()
        }
        t if t.starts_with("macp.modes.decision.v1.") => encode_decision_payload(msg),
        t if t.starts_with("macp.modes.proposal.v1.") => encode_proposal_payload(msg),
        t if t.starts_with("macp.modes.task.v1.") => encode_task_payload(msg),
        t if t.starts_with("macp.modes.handoff.v1.") => encode_handoff_payload(msg),
        t if t.starts_with("macp.modes.quorum.v1.") => encode_quorum_payload(msg),
        t if t.starts_with("macp.modes.multi_round.v1.") => encode_multi_round_payload(msg),
        _ => panic!(
            "Unknown payload_type: {} in fixture for mode {}",
            msg.payload_type, fixture.mode
        ),
    }
}

fn encode_decision_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "Proposal" => macp_runtime::decision_pb::ProposalPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            option: p["option"].as_str().unwrap_or_default().into(),
            rationale: p["rationale"].as_str().unwrap_or_default().into(),
            supporting_data: vec![],
        }
        .encode_to_vec(),
        "Evaluation" => macp_runtime::decision_pb::EvaluationPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            recommendation: p["recommendation"].as_str().unwrap_or_default().into(),
            confidence: p["confidence"].as_f64().unwrap_or_default(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "Objection" => macp_runtime::decision_pb::ObjectionPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
            severity: p["severity"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "Vote" => macp_runtime::decision_pb::VotePayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            vote: p["vote"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled decision message: {}", msg.message_type),
    }
}

fn encode_proposal_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "Proposal" => macp_runtime::proposal_pb::ProposalPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            title: p["title"].as_str().unwrap_or_default().into(),
            summary: p["summary"].as_str().unwrap_or_default().into(),
            details: vec![],
            tags: vec![],
        }
        .encode_to_vec(),
        "CounterProposal" => macp_runtime::proposal_pb::CounterProposalPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            supersedes_proposal_id: p["supersedes_proposal_id"]
                .as_str()
                .unwrap_or_default()
                .into(),
            title: p["title"].as_str().unwrap_or_default().into(),
            summary: p["summary"].as_str().unwrap_or_default().into(),
            details: vec![],
        }
        .encode_to_vec(),
        "Accept" => macp_runtime::proposal_pb::AcceptPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "Reject" => macp_runtime::proposal_pb::RejectPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
            terminal: p["terminal"].as_bool().unwrap_or(false),
        }
        .encode_to_vec(),
        "Withdraw" => macp_runtime::proposal_pb::WithdrawPayload {
            proposal_id: p["proposal_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled proposal message: {}", msg.message_type),
    }
}

fn encode_task_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "TaskRequest" => macp_runtime::task_pb::TaskRequestPayload {
            task_id: p["task_id"].as_str().unwrap_or_default().into(),
            title: p["title"].as_str().unwrap_or_default().into(),
            instructions: p["instructions"].as_str().unwrap_or_default().into(),
            requested_assignee: p["requested_assignee"].as_str().unwrap_or_default().into(),
            input: vec![],
            deadline_unix_ms: p["deadline_unix_ms"].as_i64().unwrap_or(0),
        }
        .encode_to_vec(),
        "TaskAccept" => macp_runtime::task_pb::TaskAcceptPayload {
            task_id: p["task_id"].as_str().unwrap_or_default().into(),
            assignee: p["assignee"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "TaskComplete" => macp_runtime::task_pb::TaskCompletePayload {
            task_id: p["task_id"].as_str().unwrap_or_default().into(),
            assignee: p["assignee"].as_str().unwrap_or_default().into(),
            output: vec![],
            summary: p["summary"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled task message: {}", msg.message_type),
    }
}

fn encode_handoff_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "HandoffOffer" => macp_runtime::handoff_pb::HandoffOfferPayload {
            handoff_id: p["handoff_id"].as_str().unwrap_or_default().into(),
            target_participant: p["target_participant"].as_str().unwrap_or_default().into(),
            scope: p["scope"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "HandoffAccept" => macp_runtime::handoff_pb::HandoffAcceptPayload {
            handoff_id: p["handoff_id"].as_str().unwrap_or_default().into(),
            accepted_by: p["accepted_by"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "HandoffDecline" => macp_runtime::handoff_pb::HandoffDeclinePayload {
            handoff_id: p["handoff_id"].as_str().unwrap_or_default().into(),
            declined_by: p["declined_by"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "HandoffContext" => macp_runtime::handoff_pb::HandoffContextPayload {
            handoff_id: p["handoff_id"].as_str().unwrap_or_default().into(),
            content_type: p["content_type"].as_str().unwrap_or_default().into(),
            context: p["context"]
                .as_str()
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled handoff message: {}", msg.message_type),
    }
}

fn encode_quorum_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "ApprovalRequest" => macp_runtime::quorum_pb::ApprovalRequestPayload {
            request_id: p["request_id"].as_str().unwrap_or_default().into(),
            action: p["action"].as_str().unwrap_or_default().into(),
            summary: p["summary"].as_str().unwrap_or_default().into(),
            details: vec![],
            required_approvals: p["required_approvals"].as_u64().unwrap_or(0) as u32,
        }
        .encode_to_vec(),
        "Approve" => macp_runtime::quorum_pb::ApprovePayload {
            request_id: p["request_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled quorum message: {}", msg.message_type),
    }
}

fn encode_multi_round_payload(msg: &ConformanceMessage) -> Vec<u8> {
    let p = &msg.payload;
    match msg.message_type.as_str() {
        "Contribute" => macp_runtime::multi_round_pb::ContributePayload {
            value: p["value"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        _ => panic!("Unhandled multi_round message: {}", msg.message_type),
    }
}

fn expected_state(name: &str) -> SessionState {
    match name {
        "Open" => SessionState::Open,
        "Resolved" => SessionState::Resolved,
        "Expired" => SessionState::Expired,
        other => panic!("Unknown expected_final_state: {other}"),
    }
}

fn resolution_to_json(resolution: &[u8]) -> Option<Value> {
    CommitmentPayload::decode(resolution)
        .ok()
        .map(|commitment| {
            json!({
                "commitment_id": commitment.commitment_id,
                "action": commitment.action,
                "authority_scope": commitment.authority_scope,
                "reason": commitment.reason,
                "mode_version": commitment.mode_version,
                "policy_version": commitment.policy_version,
                "configuration_version": commitment.configuration_version,
                "outcome_positive": commitment.outcome_positive,
            })
        })
}

fn mode_state_to_json(session: &Session) -> Option<Value> {
    if session.mode_state.is_empty() {
        return None;
    }
    serde_json::from_slice(&session.mode_state).ok()
}

fn assert_json_contains(actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Object(actual_map), Value::Object(expected_map)) => {
            for (key, expected_value) in expected_map {
                let actual_value = actual_map
                    .get(key)
                    .unwrap_or_else(|| panic!("missing key '{key}' in actual json {actual:?}"));
                assert_json_contains(actual_value, expected_value);
            }
        }
        (Value::Array(actual_items), Value::Array(expected_items)) => {
            assert!(
                actual_items.len() >= expected_items.len(),
                "actual array shorter than expected: {actual_items:?} vs {expected_items:?}"
            );
            for (idx, expected_item) in expected_items.iter().enumerate() {
                assert_json_contains(&actual_items[idx], expected_item);
            }
        }
        _ => assert_eq!(actual, expected),
    }
}

async fn assert_replay_equivalence(rt: &Runtime, sid: &str, live_session: &Session) {
    let log = rt
        .log_store
        .get_log(sid)
        .await
        .unwrap_or_else(|| panic!("missing log entries for {sid}"));
    // Pass the runtime's policy registry so replay re-resolves any bound policy
    // and re-evaluates commitments through it, exercising policy-bound replay
    // (not just log-trusting replay). For a policy-bound decline the votes are in
    // the log ahead of the Commitment, so re-evaluation is deterministic.
    let replayed = replay_session(
        sid,
        &log,
        rt.mode_registry().as_ref(),
        Some(rt.policy_registry().as_ref()),
    )
    .unwrap_or_else(|e| panic!("replay failed for {sid}: {e}"));
    assert_eq!(replayed.state, live_session.state, "replay state mismatch");
    assert_eq!(
        replayed.resolution, live_session.resolution,
        "replay resolution mismatch"
    );
    assert_eq!(
        replayed.mode_state, live_session.mode_state,
        "replay mode_state mismatch"
    );
    assert_eq!(
        replayed.seen_message_ids, live_session.seen_message_ids,
        "replay dedup state mismatch"
    );
}

async fn run_conformance_fixture(path: &Path) {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read fixture {}: {e}", path.display()));
    let fixture: ConformanceFixture = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("Failed to parse fixture {}: {e}", path.display()));

    let rt = make_runtime();
    let sid = new_sid();

    // Register a fixture's inline policy (if any) before SessionStart, so a
    // non-empty policy_version resolves instead of failing UNKNOWN_POLICY_VERSION.
    if let Some(policy) = &fixture.policy {
        let def: macp_runtime::policy::PolicyDefinition = serde_json::from_value(policy.clone())
            .unwrap_or_else(|e| panic!("bad inline policy in {}: {e}", path.display()));
        rt.register_policy(def)
            .unwrap_or_else(|e| panic!("register_policy failed in {}: {e}", path.display()));
    }

    let start_payload = SessionStartPayload {
        intent: "conformance".into(),
        participants: fixture.participants.clone(),
        mode_version: fixture.mode_version.clone(),
        configuration_version: fixture.configuration_version.clone(),
        policy_version: fixture.policy_version.clone(),
        ttl_ms: fixture.ttl_ms,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    rt.process(
        &Envelope {
            macp_version: "1.0".into(),
            mode: fixture.mode.clone(),
            message_type: "SessionStart".into(),
            message_id: "m0".into(),
            session_id: sid.clone(),
            sender: fixture.initiator.clone(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload: start_payload,
        },
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("SessionStart failed for {}: {e}", path.display()));

    for (i, msg) in fixture.messages.iter().enumerate() {
        let payload = encode_payload(&fixture, msg);
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: fixture.mode.clone(),
            message_type: msg.message_type.clone(),
            message_id: format!("m{}", i + 1),
            session_id: sid.clone(),
            sender: msg.sender.clone(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload,
        };

        let result = rt.process(&env, None).await;
        match msg.expect.as_str() {
            "accept" => {
                result.unwrap_or_else(|e| {
                    panic!(
                        "Message {} ({}) expected accept but got error: {e} in {}",
                        i + 1,
                        msg.message_type,
                        path.display()
                    )
                });
            }
            "reject" => {
                assert!(
                    result.is_err(),
                    "Message {} ({}) expected reject but succeeded in {}",
                    i + 1,
                    msg.message_type,
                    path.display()
                );
                if let Some(expected_error_code) = &msg.expected_error_code {
                    let err = result.unwrap_err();
                    assert_eq!(
                        err.error_code(),
                        expected_error_code,
                        "reject error code mismatch at message {} ({}) in {}",
                        i + 1,
                        msg.message_type,
                        path.display()
                    );
                }
            }
            other => panic!("Unknown expect value: {other}"),
        }
    }

    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(
        session.state,
        expected_state(&fixture.expected_final_state),
        "Final state mismatch for {}",
        path.display()
    );

    if let Some(expect_resolution_present) = fixture.expect_resolution_present {
        assert_eq!(
            session.resolution.is_some(),
            expect_resolution_present,
            "resolution presence mismatch for {}",
            path.display()
        );
    }

    if let Some(expected_resolution) = &fixture.expected_resolution {
        let actual_resolution = session
            .resolution
            .as_ref()
            .and_then(|resolution| resolution_to_json(resolution))
            .unwrap_or_else(|| {
                panic!("resolution missing or not decodable for {}", path.display())
            });
        assert_json_contains(&actual_resolution, expected_resolution);
    }

    if let Some(expected_mode_state) = &fixture.expected_mode_state {
        let actual_mode_state = mode_state_to_json(&session)
            .unwrap_or_else(|| panic!("mode state missing or not json for {}", path.display()));
        assert_json_contains(&actual_mode_state, expected_mode_state);
    }

    if fixture.verify_replay_equivalence {
        assert_replay_equivalence(&rt, &sid, &session).await;
    }
}

/// Directory the conformance fixtures load from. Defaults to the vendored
/// copies in `tests/conformance/`; the CI oracle job overrides it with
/// `MACP_CONFORMANCE_FIXTURES_DIR` to run this same suite against the spec
/// repo's canonical `schemas/conformance/` — the single fixture source. The
/// oracle also byte-compares the vendored copies against canonical, so the
/// two locations cannot drift silently.
fn fixtures_dir() -> std::path::PathBuf {
    match std::env::var("MACP_CONFORMANCE_FIXTURES_DIR") {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance"),
    }
}

macro_rules! conformance_test {
    ($name:ident, $file:expr) => {
        #[tokio::test]
        async fn $name() {
            let path = fixtures_dir().join($file);
            run_conformance_fixture(&path).await;
        }
    };
}

conformance_test!(conformance_decision_happy_path, "decision_happy_path.json");
conformance_test!(conformance_proposal_happy_path, "proposal_happy_path.json");
conformance_test!(conformance_task_happy_path, "task_happy_path.json");
conformance_test!(conformance_handoff_happy_path, "handoff_happy_path.json");
conformance_test!(conformance_quorum_happy_path, "quorum_happy_path.json");
conformance_test!(
    conformance_multi_round_happy_path,
    "multi_round_happy_path.json"
);
conformance_test!(
    conformance_decision_reject_paths,
    "decision_reject_paths.json"
);
conformance_test!(
    conformance_decision_negative_outcome,
    "decision_negative_outcome.json"
);
conformance_test!(
    conformance_proposal_reject_paths,
    "proposal_reject_paths.json"
);
conformance_test!(conformance_task_reject_paths, "task_reject_paths.json");
conformance_test!(
    conformance_handoff_reject_paths,
    "handoff_reject_paths.json"
);
conformance_test!(conformance_quorum_reject_paths, "quorum_reject_paths.json");
conformance_test!(
    conformance_multi_round_reject_paths,
    "multi_round_reject_paths.json"
);

/// Structural guard for the conformance-pack format: every fixture must use
/// canonical fully-qualified protobuf payload names and valid enums, matching
/// `tests/conformance/schema.json`. Keeps local fixtures from drifting back
/// to runtime-internal shorthand.
#[test]
fn fixtures_conform_to_canonical_format() {
    fn canonical_payload_type(pt: &str) -> bool {
        pt.starts_with("macp.v1.") || (pt.starts_with("macp.modes.") && pt.ends_with("Payload"))
    }
    fn canonical_mode(m: &str) -> bool {
        m.starts_with("macp.mode.") || m.starts_with("ext.")
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json")
            || path.file_name().and_then(|n| n.to_str()) == Some("schema.json")
        {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();

        assert!(
            canonical_mode(v["mode"].as_str().unwrap_or("")),
            "{name}: invalid mode id"
        );
        assert!(
            v["ttl_ms"].as_i64().unwrap_or(0) >= 1,
            "{name}: ttl_ms >= 1"
        );
        for (i, msg) in v["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .enumerate()
        {
            let pt = msg["payload_type"].as_str().unwrap_or("");
            assert!(
                canonical_payload_type(pt),
                "{name} message {i}: non-canonical payload_type {pt:?}"
            );
            let expect = msg["expect"].as_str().unwrap_or("");
            assert!(
                expect == "accept" || expect == "reject",
                "{name} message {i}: expect must be accept|reject"
            );
        }
        checked += 1;
    }
    assert!(
        checked >= 13,
        "expected all fixtures checked, got {checked}"
    );
}
