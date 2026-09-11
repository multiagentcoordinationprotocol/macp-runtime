//! Issue #146: the quorum mode's effective approval threshold must be readable
//! from outside the crate.
//!
//! A downstream runtime had re-implemented `QuorumMode::effective_threshold`
//! because it was private, and a mirror of a governance rule drifts — which is
//! exactly what issue #145 turned out to be, one rule derived twice. These
//! tests are the compile-time guard on the replacement: every path below goes
//! through `macp_runtime::mode::quorum::...` and nothing else, so the file
//! stops compiling if the accessor, the record type, or the outcome enum loses
//! its `pub`. In particular there is **no `decode_mode_state` call here** —
//! making the caller decode `session.mode_state` itself would hand back the
//! internals the accessor exists to hide.

use macp_runtime::log_store::LogStore;
use macp_runtime::mode::quorum::{ApprovalRequestRecord, ApprovalThreshold, QuorumMode};
use macp_runtime::pb::{Envelope, SessionStartPayload};
use macp_runtime::quorum_pb::ApprovalRequestPayload;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::Session;
use macp_runtime::storage::MemoryBackend;
use prost::Message;
use std::sync::Arc;

const MODE: &str = "macp.mode.quorum.v1";
const COORDINATOR: &str = "agent://coordinator";

fn make_runtime() -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
}

fn participants() -> Vec<String> {
    vec![
        COORDINATOR.into(),
        "agent://a".into(),
        "agent://b".into(),
        "agent://c".into(),
    ]
}

fn envelope(message_type: &str, message_id: &str, session_id: &str, payload: Vec<u8>) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: MODE.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: session_id.into(),
        sender: COORDINATOR.into(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload,
    }
}

fn session_start(policy_version: &str) -> Vec<u8> {
    SessionStartPayload {
        intent: "threshold-accessor".into(),
        participants: participants(),
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: policy_version.into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

fn approval_request(required_approvals: u32) -> Vec<u8> {
    ApprovalRequestPayload {
        request_id: "r1".into(),
        action: "deploy.production".into(),
        summary: "Deploy v2".into(),
        details: vec![],
        required_approvals,
    }
    .encode_to_vec()
}

/// Drive a real session to the point where an `ApprovalRequest` is accepted and
/// hand back the `Session` the kernel materialized, with the threshold read at
/// each step through the public accessor alone.
async fn threshold_after_request(
    policy: Option<macp_runtime::macp_core::policy::PolicyDefinition>,
    required_approvals: u32,
) -> Option<ApprovalThreshold> {
    let rt = make_runtime();
    let policy_version = match policy {
        Some(definition) => {
            let id = definition.policy_id.clone();
            rt.register_policy(definition).expect("policy registers");
            id
        }
        None => String::new(),
    };
    let session_id = uuid::Uuid::new_v4().as_hyphenated().to_string();
    rt.process(
        &envelope(
            "SessionStart",
            "m1",
            &session_id,
            session_start(&policy_version),
        ),
        None,
    )
    .await
    .expect("session starts");

    // Criterion: no accepted ApprovalRequest yet is its own answer, and it is
    // NOT the same answer as an unsatisfiable policy.
    let session = rt
        .get_session_checked(&session_id)
        .await
        .expect("session exists");
    assert_eq!(
        QuorumMode::effective_threshold_for_session(&session).expect("state decodes"),
        None,
        "a session with no accepted ApprovalRequest has no threshold to report"
    );

    rt.process(
        &envelope(
            "ApprovalRequest",
            "m2",
            &session_id,
            approval_request(required_approvals),
        ),
        None,
    )
    .await
    .expect("approval request is accepted");

    let session = rt
        .get_session_checked(&session_id)
        .await
        .expect("session exists");
    QuorumMode::effective_threshold_for_session(&session).expect("state decodes")
}

fn quorum_policy(
    policy_id: &str,
    rules: serde_json::Value,
) -> macp_runtime::policy::PolicyDefinition {
    macp_runtime::macp_core::policy::PolicyDefinition {
        policy_id: policy_id.into(),
        mode: MODE.into(),
        description: "threshold accessor fixture".into(),
        rules,
        schema_version: 1,
    }
}

#[tokio::test]
async fn ungoverned_session_reports_the_requests_own_required_approvals() {
    // The common case, and the one a caller most needs: with no policy bound,
    // the bar is the ApprovalRequest payload's own value — already resolved,
    // so the fallback rule is not left for the caller to re-implement.
    assert_eq!(
        threshold_after_request(None, 3).await,
        Some(ApprovalThreshold::Approvals(3))
    );
}

#[tokio::test]
async fn policy_threshold_replaces_the_payload_value_and_ceils() {
    // RFC-MACP-0011 §6: a policy threshold *replaces* `required_approvals`.
    // 50% of 4 participants is 2, not the 4 the payload asked for, and the
    // caller learns that without touching policy rules or mode state.
    assert_eq!(
        threshold_after_request(
            Some(quorum_policy(
                "threshold-half",
                serde_json::json!({ "threshold": { "type": "percentage", "value": 50 } }),
            )),
            4,
        )
        .await,
        Some(ApprovalThreshold::Approvals(2))
    );
    // 33% of 4 is 1.32 — ceiling, so 2, and never a bar of zero that would be
    // met before any ballot was cast (issue #145).
    assert_eq!(
        threshold_after_request(
            Some(quorum_policy(
                "threshold-third",
                serde_json::json!({ "threshold": { "type": "percentage", "value": 33 } }),
            )),
            4,
        )
        .await,
        Some(ApprovalThreshold::Approvals(2))
    );
}

#[test]
fn request_level_form_is_public_and_reports_unsatisfiable_separately() {
    // The `&ApprovalRequestRecord` form is public too, for a caller that
    // already holds the record. `RegisterPolicy` refuses a `weighted`
    // threshold, so this outcome needs a directly-constructed
    // `PolicyDefinition` — which is precisely why it must be distinguishable
    // from "no request yet" rather than folded into one `None`.
    let mut session = Session::builder("s1", MODE, COORDINATOR)
        .ttl_ms(60_000)
        .participants(participants())
        .mode_version("1.0.0")
        .configuration_version("cfg-1")
        .policy_version("threshold-weighted")
        .build();
    let record = ApprovalRequestRecord {
        request_id: "r1".into(),
        action: "deploy.production".into(),
        summary: "Deploy v2".into(),
        details: vec![],
        required_approvals: 3,
        requested_by: COORDINATOR.into(),
    };

    session.policy_definition = Some(quorum_policy(
        "threshold-weighted",
        serde_json::json!({ "threshold": { "type": "weighted", "value": 3 } }),
    ));
    assert_eq!(
        QuorumMode::effective_threshold(&session, &record),
        ApprovalThreshold::Unsatisfiable,
        "an unimplementable policy seals no outcome, and says so"
    );

    session.policy_definition = Some(quorum_policy(
        "threshold-two",
        serde_json::json!({ "threshold": { "type": "n_of_m", "value": 2 } }),
    ));
    assert_eq!(
        QuorumMode::effective_threshold(&session, &record),
        ApprovalThreshold::Approvals(2)
    );

    session.policy_definition = None;
    assert_eq!(
        QuorumMode::effective_threshold(&session, &record),
        ApprovalThreshold::Approvals(3)
    );
}
