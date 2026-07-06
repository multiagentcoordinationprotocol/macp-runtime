//! Integration tests for RFC compliance validation gap fixes.
//! These tests validate input validation added during the production hardening audit.

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::decision_pb::EvaluationPayload;
use macp_runtime::pb::{Envelope, InitializeRequest, SessionStartPayload, SignalPayload};
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

// ── Decision Mode: Confidence Bounds (RFC-MACP-0004 §2.2) ─────────────

#[tokio::test]
async fn evaluation_confidence_out_of_bounds_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let evaluator = "agent://evaluator";

    // Start a decision session
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("confidence test", &[coord, evaluator], 30_000),
        ),
    )
    .await
    .unwrap();

    // Submit a proposal first
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "option-1", "testing"),
        ),
    )
    .await
    .unwrap();

    // Evaluation with confidence > 1.0 should be rejected
    let bad_eval = EvaluationPayload {
        proposal_id: "p1".into(),
        recommendation: "APPROVE".into(),
        confidence: 2.0,
        reason: "too confident".into(),
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        evaluator,
        envelope(
            MODE_DECISION,
            "Evaluation",
            &new_message_id(),
            &sid,
            evaluator,
            bad_eval,
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "Evaluation with confidence > 1.0 must be rejected");
}

// ── Decision Mode: Objection Severity Enum (RFC-MACP-0004 §2.3) ───────

#[tokio::test]
async fn objection_invalid_severity_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let objector = "agent://objector";

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("severity test", &[coord, objector], 30_000),
        ),
    )
    .await
    .unwrap();

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "option-1", "testing"),
        ),
    )
    .await
    .unwrap();

    // Objection with invalid severity should be rejected
    let bad_obj = macp_runtime::decision_pb::ObjectionPayload {
        proposal_id: "p1".into(),
        reason: "bad".into(),
        severity: "urgent".into(), // not a valid enum value
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        objector,
        envelope(
            MODE_DECISION,
            "Objection",
            &new_message_id(),
            &sid,
            objector,
            bad_obj,
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "Objection with invalid severity must be rejected");
}

// ── Signal Payload Validation (RFC-MACP-0001 §4) ──────────────────────

#[tokio::test]
async fn signal_empty_signal_type_rejected() {
    let mut client = common::grpc_client().await;

    // Signal with non-empty payload but empty signal_type should be rejected
    let signal_payload = SignalPayload {
        signal_type: String::new(),
        data: b"some data".to_vec(),
        confidence: 0.0,
        correlation_session_id: String::new(),
    }
    .encode_to_vec();

    let env = Envelope {
        macp_version: "1.0".into(),
        mode: String::new(),
        message_type: "Signal".into(),
        message_id: new_message_id(),
        session_id: String::new(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: signal_payload,
    };

    let ack = client
        .send(with_sender(
            "agent://signaler",
            macp_runtime::pb::SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert!(!ack.ok, "Signal with empty signal_type must be rejected");
}

// ── Max Participants (Safety Limit) ───────────────────────────────────

#[tokio::test]
async fn session_start_too_many_participants_rejected() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";

    // Create a participant list with 1001 entries
    let mut participants: Vec<String> = (0..1001).map(|i| format!("agent://p{i}")).collect();
    participants.insert(0, coord.to_string());

    let payload = SessionStartPayload {
        intent: "overflow test".into(),
        participants,
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        ttl_ms: 30_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            payload,
        ),
    )
    .await
    .unwrap();
    assert!(
        !ack.ok,
        "SessionStart with >1000 participants must be rejected"
    );
}

// ── Initialize RPC: Empty Versions (RFC-MACP-0001 §4.1) ──────────────

#[tokio::test]
async fn initialize_empty_versions_rejected() {
    let mut client = common::grpc_client().await;

    let err = client
        .initialize(Request::new(InitializeRequest {
            supported_protocol_versions: vec![],
            client_info: None,
            capabilities: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "Empty supported_protocol_versions must return InvalidArgument"
    );
}
