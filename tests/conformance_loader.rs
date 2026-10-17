use chrono::Utc;
use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::{Clock, ManualClock};
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
    /// Exact assertion on the fixture session's accepted history after its
    /// `SessionStart`, synthetic entries included (README `expected_history`).
    #[serde(default)]
    expected_history: Option<Vec<ExpectedHistoryEntry>>,
}

#[derive(Deserialize)]
struct ExpectedHistoryEntry {
    message_type: String,
    sender: String,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    synthetic: bool,
}

/// Per-step Envelope overrides (README `messages[].envelope`).
#[derive(Default, Deserialize)]
struct EnvelopeOverride {
    session_id: Option<String>,
    macp_version: Option<String>,
    mode: Option<String>,
    timestamp_unix_ms: Option<i64>,
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
    /// The runtime's acceptance clock for this step (RFC-MACP-0001 §7.6
    /// item 4), distinct from the Envelope timestamp. All-or-none.
    #[serde(default)]
    accepted_at_ms: Option<i64>,
    #[serde(default)]
    envelope: Option<EnvelopeOverride>,
}

fn default_true() -> bool {
    true
}

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn make_runtime(clock: Option<Arc<ManualClock>>) -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let rt = Runtime::new(storage, registry, log_store);
    match clock {
        Some(clock) => rt.with_clock(clock),
        None => rt,
    }
}

fn encode_payload(fixture: &ConformanceFixture, msg: &ConformanceMessage) -> Vec<u8> {
    match msg.payload_type.as_str() {
        // Canonical fully-qualified protobuf message names (conformance-pack
        // format): mode payloads are macp.modes.<mode>.v1.<Type>Payload and
        // the shared terminal is macp.v1.CommitmentPayload.
        "macp.v1.SessionStartPayload" => {
            let p = &msg.payload;
            SessionStartPayload {
                intent: p["intent"].as_str().unwrap_or_default().into(),
                participants: p["participants"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default(),
                mode_version: p["mode_version"].as_str().unwrap_or_default().into(),
                configuration_version: p["configuration_version"]
                    .as_str()
                    .unwrap_or_default()
                    .into(),
                policy_version: p["policy_version"].as_str().unwrap_or_default().into(),
                ttl_ms: p["ttl_ms"].as_i64().unwrap_or_default(),
                context_id: String::new(),
                extensions: std::collections::HashMap::new(),
                roots: vec![],
                max_suspend_ms: 0,
            }
            .encode_to_vec()
        }
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
        "TaskReject" => macp_runtime::task_pb::TaskRejectPayload {
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
        "TaskFail" => macp_runtime::task_pb::TaskFailPayload {
            task_id: p["task_id"].as_str().unwrap_or_default().into(),
            assignee: p["assignee"].as_str().unwrap_or_default().into(),
            error_code: p["error_code"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
            retryable: p["retryable"].as_bool().unwrap_or(false),
        }
        .encode_to_vec(),
        "TaskUpdate" => macp_runtime::task_pb::TaskUpdatePayload {
            task_id: p["task_id"].as_str().unwrap_or_default().into(),
            status: p["status"].as_str().unwrap_or_default().into(),
            progress: p["progress"].as_f64().unwrap_or_default(),
            message: p["message"].as_str().unwrap_or_default().into(),
            partial_output: vec![],
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
            implicit: p["implicit"].as_bool().unwrap_or(false),
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
        "Reject" => macp_runtime::quorum_pb::RejectPayload {
            request_id: p["request_id"].as_str().unwrap_or_default().into(),
            reason: p["reason"].as_str().unwrap_or_default().into(),
        }
        .encode_to_vec(),
        "Abstain" => macp_runtime::quorum_pb::AbstainPayload {
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
        "Suspended" => SessionState::Suspended,
        "Cancelled" => SessionState::Cancelled,
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

/// Decodes an accepted-history payload for `expected_history`'s subset match.
/// Covers the shared terminal and every Handoff message (the only mode whose
/// fixtures assert history today); anything else fails loudly rather than
/// matching vacuously.
fn history_payload_to_json(message_type: &str, raw: &[u8]) -> Value {
    use macp_runtime::handoff_pb as h;
    match message_type {
        "Commitment" => resolution_to_json(raw).expect("Commitment payload decodes"),
        "HandoffOffer" => {
            let p = h::HandoffOfferPayload::decode(raw).unwrap();
            json!({"handoff_id": p.handoff_id, "target_participant": p.target_participant,
                   "scope": p.scope, "reason": p.reason})
        }
        "HandoffAccept" => {
            let p = h::HandoffAcceptPayload::decode(raw).unwrap();
            json!({"handoff_id": p.handoff_id, "accepted_by": p.accepted_by,
                   "reason": p.reason, "implicit": p.implicit})
        }
        "HandoffDecline" => {
            let p = h::HandoffDeclinePayload::decode(raw).unwrap();
            json!({"handoff_id": p.handoff_id, "declined_by": p.declined_by, "reason": p.reason})
        }
        "HandoffContext" => {
            let p = h::HandoffContextPayload::decode(raw).unwrap();
            json!({"handoff_id": p.handoff_id, "content_type": p.content_type,
                   "context": String::from_utf8_lossy(&p.context)})
        }
        other => panic!("expected_history payload decoding not implemented for {other}"),
    }
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

    // `accepted_at_ms` is all-or-none (README). When present the harness
    // drives the runtime's acceptance clock and opens the fixture session at
    // acceptance time 0 with Envelope timestamp 0; otherwise the runtime's own
    // wall clock runs, as it always has.
    let stepped = fixture
        .messages
        .iter()
        .filter(|m| m.accepted_at_ms.is_some())
        .count();
    assert!(
        stepped == 0 || stepped == fixture.messages.len(),
        "accepted_at_ms is all-or-none in {}",
        path.display()
    );
    let clock = (stepped > 0).then(|| Arc::new(ManualClock::new(0)));
    let rt = make_runtime(clock.clone());
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
            timestamp_unix_ms: if clock.is_some() {
                0
            } else {
                Utc::now().timestamp_millis()
            },
            payload: start_payload,
        },
        None,
    )
    .await
    .unwrap_or_else(|e| panic!("SessionStart failed for {}: {e}", path.display()));

    for (i, msg) in fixture.messages.iter().enumerate() {
        let payload = encode_payload(&fixture, msg);
        if let (Some(clock), Some(at)) = (&clock, msg.accepted_at_ms) {
            assert!(
                at >= clock.now_ms(),
                "accepted_at_ms must be non-decreasing in {}",
                path.display()
            );
            clock.set(at);
        }
        let no_override = EnvelopeOverride::default();
        let ov = msg.envelope.as_ref().unwrap_or(&no_override);
        let env = Envelope {
            macp_version: ov.macp_version.clone().unwrap_or_else(|| "1.0".into()),
            mode: ov.mode.clone().unwrap_or_else(|| fixture.mode.clone()),
            message_type: msg.message_type.clone(),
            message_id: format!("m{}", i + 1),
            session_id: ov.session_id.clone().unwrap_or_else(|| sid.clone()),
            sender: msg.sender.clone(),
            timestamp_unix_ms: ov.timestamp_unix_ms.unwrap_or_else(|| {
                if clock.is_some() {
                    0
                } else {
                    Utc::now().timestamp_millis()
                }
            }),
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

    if let Some(expected_history) = &fixture.expected_history {
        let submitted: std::collections::HashSet<String> = (1..=fixture.messages.len())
            .map(|i| format!("m{i}"))
            .collect();
        let log = rt.log_store.get_log(&sid).await.unwrap_or_default();
        let accepted: Vec<_> = log
            .iter()
            .filter(|e| e.entry_kind == macp_runtime::log_store::EntryKind::Incoming)
            .filter(|e| e.message_id != "m0")
            .collect();
        let shape: Vec<String> = accepted
            .iter()
            .map(|e| format!("{} from {}", e.message_type, e.sender))
            .collect();
        assert_eq!(
            accepted.len(),
            expected_history.len(),
            "accepted history length mismatch in {}: {shape:?}",
            path.display()
        );
        for (idx, (actual, expected)) in accepted.iter().zip(expected_history).enumerate() {
            let ctx = format!("history entry {idx} in {}", path.display());
            assert_eq!(actual.message_type, expected.message_type, "{ctx}");
            assert_eq!(actual.sender, expected.sender, "{ctx}");
            assert_eq!(
                !submitted.contains(&actual.message_id),
                expected.synthetic,
                "synthetic flag mismatch at {ctx} (message_id {})",
                actual.message_id
            );
            if let Some(expected_payload) = &expected.payload {
                let actual_payload =
                    history_payload_to_json(&actual.message_type, &actual.raw_payload);
                assert_json_contains(&actual_payload, expected_payload);
            }
        }
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
conformance_test!(
    conformance_proposal_negative_outcome,
    "proposal_negative_outcome.json"
);
conformance_test!(conformance_task_reject_paths, "task_reject_paths.json");
conformance_test!(
    conformance_task_negative_outcome,
    "task_negative_outcome.json"
);
conformance_test!(
    conformance_handoff_reject_paths,
    "handoff_reject_paths.json"
);
conformance_test!(
    conformance_handoff_negative_outcome,
    "handoff_negative_outcome.json"
);
conformance_test!(conformance_quorum_reject_paths, "quorum_reject_paths.json");
conformance_test!(
    conformance_handoff_implicit_accept,
    "handoff_implicit_accept.json"
);
conformance_test!(
    conformance_session_start_reject_paths,
    "session_start_reject_paths.json"
);
conformance_test!(
    conformance_quorum_unreachable_override,
    "quorum_unreachable_override.json"
);
conformance_test!(
    conformance_quorum_percentage_override_reachable,
    "quorum_percentage_override_reachable.json"
);
conformance_test!(
    conformance_quorum_negative_outcome,
    "quorum_negative_outcome.json"
);
conformance_test!(
    conformance_multi_round_reject_paths,
    "multi_round_reject_paths.json"
);
conformance_test!(
    conformance_decision_critical_objection_veto,
    "decision_critical_objection_veto.json"
);
conformance_test!(
    conformance_decision_critical_objection_finalize_decline,
    "decision_critical_objection_finalize_decline.json"
);

// Added upstream by spec #126 (RFC-MACP-0007 Section 6.2), which settled the
// decline-guard waiver question this runtime raised as spec issue #117: a
// critical objection authorizes a DECLINE even when the vote tally would not
// otherwise permit one, and the waiver covers the decline guard WHOLE -- it
// does not additionally require commitment.require_vote_quorum, nor the
// evaluation.* prerequisites of the same voting pipeline, because all three
// gate an outcome deriving its authority from the voting result and this
// decline derives none. The same commit closed deliberation at the
// first accepted Vote (Section 5 rule 6), which this runtime already enforced
// via DecisionMode::ensure_can_deliberate's phase check.
conformance_test!(
    conformance_decision_finalize_decline_quorum_waiver,
    "decision_finalize_decline_quorum_waiver.json"
);

// The `schema_version` 3 / empty-tally corpus added upstream by spec #99
// (RFC-MACP-0012 Sections 4.1 and 8, RFC-MACP-0007 Section 6.2). Two of these
// are legacy-arm regression guards rather than new coverage:
// `decision_empty_tally_legacy` and `decision_legacy_require_vote_quorum` pin
// the fail-open rule that schema versions 1 and 2 keep forever, so they must
// pass both before and after the fail-closed work.
conformance_test!(
    conformance_decision_empty_tally_binding,
    "decision_empty_tally_binding.json"
);
conformance_test!(
    conformance_decision_empty_tally_legacy,
    "decision_empty_tally_legacy.json"
);
conformance_test!(
    conformance_decision_finalize_decline_empty_tally,
    "decision_finalize_decline_empty_tally.json"
);
conformance_test!(
    conformance_decision_legacy_require_vote_quorum,
    "decision_legacy_require_vote_quorum.json"
);
conformance_test!(
    conformance_decision_majority_empty_tally,
    "decision_majority_empty_tally.json"
);
conformance_test!(
    conformance_decision_none_v3_empty_tally,
    "decision_none_v3_empty_tally.json"
);
conformance_test!(
    conformance_decision_none_v3_empty_tally_positive,
    "decision_none_v3_empty_tally_positive.json"
);
conformance_test!(conformance_decision_plurality, "decision_plurality.json");
conformance_test!(
    conformance_decision_supermajority_empty_tally,
    "decision_supermajority_empty_tally.json"
);
conformance_test!(
    conformance_decision_weighted_zero_weight,
    "decision_weighted_zero_weight.json"
);
conformance_test!(
    conformance_decision_weighted_zero_weight_v1,
    "decision_weighted_zero_weight_v1.json"
);
conformance_test!(
    conformance_decision_zero_participants,
    "decision_zero_participants.json"
);

// Spec #111 (RFC-MACP-0002 Section 6.1, RFC-MACP-0007 1.2.0-draft Section 5).
// Rule 5 — "the Session MUST NOT resolve before at least one proposal exists"
// — lost its "unless policy explicitly allows a no-go outcome with zero
// proposals" clause, and RFC-MACP-0002 Section 6.1 newly pins the rejection
// code for a Mode validation-rule breach at `INVALID_ENVELOPE`. Together those
// made the rule fixturable in both directions; it was previously unfixtured.
// Both Commitments come from the initiator, who is a declared participant and
// the commitment authority, so `FORBIDDEN` is impossible and rule 5 is the only
// reason either can be rejected.
conformance_test!(
    conformance_decision_zero_proposal_commitment,
    "decision_zero_proposal_commitment.json"
);

/// Guard: every vendored fixture must be registered with `conformance_test!`.
///
/// Unlike both SDK harnesses, which discover fixtures dynamically, the
/// registration list above is explicit. A fixture synced from the spec repo
/// without a matching `conformance_test!` entry is therefore silently never
/// replayed: it lints clean, it vendors byte-identically, the fixture-oracle
/// job passes — and it asserts nothing. That failure mode is invisible
/// precisely because every other signal stays green.
///
/// This test reads its own source and fails if any fixture file is missing
/// from it. `fixtures_conform_to_canonical_format`'s `checked >= 32` is a
/// floor on files *seen*, not on files *replayed*, so it cannot catch this.
#[test]
fn every_fixture_is_registered() {
    let src = include_str!("conformance_loader.rs");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut unregistered: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "schema.json" {
            continue;
        }
        if !src.contains(&format!("\"{name}\"")) {
            unregistered.push(name);
        }
    }
    unregistered.sort();
    assert!(
        unregistered.is_empty(),
        "fixture(s) present in tests/conformance but never registered with \
         conformance_test!, so they are never replayed: {unregistered:?}"
    );
}

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
    // Exact count of vendored fixtures (`*.json` under tests/conformance/ less
    // schema.json). Raised from 17 alongside the spec #99 corpus: the old floor
    // had drifted two fixtures below the real count, so it would have passed
    // with a fixture silently unvendored. Raised again to 32 by spec #111's
    // `decision_zero_proposal_commitment.json`, and to 37 by the Seam-driven
    // corpus (spec #192/#193: quorum overrides, handoff implicit accept,
    // SessionStart obligations).
    assert!(
        checked >= 37,
        "expected all fixtures checked, got {checked}"
    );
}
