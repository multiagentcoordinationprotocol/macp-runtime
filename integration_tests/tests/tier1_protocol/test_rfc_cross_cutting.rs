//! Cross-cutting RFC feature tests — validates protocol-level invariants that span all modes.
//! These cover: Signals, determinism/version binding, deduplication, append-only history,
//! CancelSession authorization, and discovery/manifests.

use crate::common;
use macp_integration_tests::helpers::*;
use macp_runtime::pb::{
    CommitmentPayload, Envelope, GetManifestRequest, InitializeRequest, SendRequest, SignalPayload,
    WatchSignalsRequest,
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

// ── Signals (RFC-MACP-0001 §5.1) ───────────────────────────────────────

#[tokio::test]
async fn signal_with_empty_session_and_mode_accepted() {
    // RFC-MACP-0001 §5.1: Signals MUST carry empty session_id and empty mode.
    // They are non-binding ambient messages on the ambient plane.
    // Signals do NOT enter any session's accepted history.
    // SignalPayload has: signal_type, data, confidence, correlation_session_id
    let mut client = common::grpc_client().await;

    // First start a real session so we can correlate the Signal with it
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let worker = "agent://worker";

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("decision in progress", &[coord, worker], 30_000),
        ),
    )
    .await
    .unwrap();

    // Now send a Signal from the worker to indicate progress.
    // This is an ambient message — it does NOT enter the session history.
    // It uses SignalPayload with correlation_session_id to link to the session.
    let signal_payload = macp_runtime::pb::SignalPayload {
        signal_type: "progress".into(),
        data: b"analyzing proposal options".to_vec(),
        confidence: 0.0,
        correlation_session_id: sid.clone(),
    };
    let mid = new_message_id();
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: String::new(), // empty — required for Signals
        message_type: "Signal".into(),
        message_id: mid.clone(),
        session_id: String::new(), // empty — required for Signals
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: signal_payload.encode_to_vec(),
    };

    eprintln!("── Signal Test: Valid Signal (ambient, non-binding) ──────────");
    eprintln!("   Active session: {sid} (Decision mode, OPEN)");
    eprintln!();
    eprintln!("   Signal Envelope:");
    eprintln!("     message_type:  \"Signal\"");
    eprintln!("     message_id:    \"{mid}\"");
    eprintln!("     session_id:    \"\" (empty — Signals live on ambient plane)");
    eprintln!("     mode:          \"\" (empty — Signals are not mode-scoped)");
    eprintln!("     sender:        \"agent://worker\"");
    eprintln!();
    eprintln!("   SignalPayload:");
    eprintln!("     signal_type:             \"progress\"");
    eprintln!("     data:                    \"analyzing proposal options\"");
    eprintln!("     correlation_session_id:  \"{sid}\"");
    eprintln!("     (correlates with session, but NOT session-scoped)");
    eprintln!();
    eprintln!("   RFC-MACP-0001 §5.1 rules:");
    eprintln!("     - Signals MUST carry empty session_id and empty mode");
    eprintln!("     - Signals MUST NOT mutate session state");
    eprintln!("     - Signals are non-binding (ambient plane)");
    eprintln!("     - Correlation via payload field, NOT via Envelope.session_id");
    eprintln!();

    let resp = client
        .send(with_sender(
            "agent://worker",
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let ack = resp.ack.expect("ack present");

    let err_code = ack
        .error
        .as_ref()
        .map(|e| e.code.as_str())
        .unwrap_or("(none)");
    eprintln!("   Runtime Response:");
    eprintln!("     ack.ok:            {}", ack.ok);
    eprintln!("     ack.duplicate:     {}", ack.duplicate);
    eprintln!(
        "     ack.session_state: {} (no session affected)",
        ack.session_state
    );
    eprintln!("     ack.error:         {err_code}");
    eprintln!();

    // Verify the session was NOT affected by the Signal
    let resp = get_session_as(&mut client, coord, &sid).await.unwrap();
    let meta = resp.metadata.expect("metadata present");
    eprintln!("   Session state AFTER Signal:");
    eprintln!("     state: {} (OPEN — unchanged by Signal)", meta.state);
    eprintln!("     mode:  {}", meta.mode);
    eprintln!();
    eprintln!("   Result: Signal ACCEPTED ✓");
    eprintln!("   Worker sent progress Signal on ambient plane.");
    eprintln!("   Session history was NOT modified. Session state unchanged.");
    eprintln!("─────────────────────────────────────────────────────────────");

    assert!(
        ack.ok,
        "Signal with empty session_id and mode should be accepted"
    );
    assert_eq!(
        meta.state, 1,
        "Session should remain OPEN — Signal must not mutate state"
    );
}

#[tokio::test]
async fn signal_with_non_empty_session_rejected() {
    // RFC-MACP-0001 §5.1: Signal with non-empty session_id violates envelope shape rules.
    let mut client = common::grpc_client().await;
    let mid = new_message_id();
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: String::new(),
        message_type: "Signal".into(),
        message_id: mid.clone(),
        session_id: "some-session-id".into(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: vec![],
    };

    eprintln!("── Signal Test: Invalid Signal (session_id set) ─────────────");
    eprintln!("   Envelope:");
    eprintln!("     message_type:  \"Signal\"");
    eprintln!("     session_id:    \"some-session-id\" ← VIOLATION");
    eprintln!("     mode:          \"\"");
    eprintln!();
    eprintln!("   RFC rule: Signals MUST carry empty session_id.");
    eprintln!("   A Signal with session_id is a structural envelope violation.");
    eprintln!();

    let resp = client
        .send(with_sender(
            "agent://signaler",
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let ack = resp.ack.expect("ack present");

    let err_code = ack
        .error
        .as_ref()
        .map(|e| e.code.as_str())
        .unwrap_or("(none)");
    eprintln!("   Response:");
    eprintln!("     ack.ok:    {}", ack.ok);
    eprintln!("     ack.error: {err_code}");
    eprintln!();
    eprintln!("   Result: Signal REJECTED ✓ — session_id must be empty for Signals");
    eprintln!("─────────────────────────────────────────────────────────────");

    assert!(!ack.ok, "Signal with non-empty session_id must be rejected");
}

#[tokio::test]
async fn signal_with_non_empty_mode_rejected() {
    // RFC-MACP-0001 §5.1: Signal with non-empty mode violates envelope shape rules.
    let mut client = common::grpc_client().await;
    let mid = new_message_id();
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: "macp.mode.decision.v1".into(),
        message_type: "Signal".into(),
        message_id: mid.clone(),
        session_id: String::new(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: vec![],
    };

    eprintln!("── Signal Test: Invalid Signal (mode set) ───────────────────");
    eprintln!("   Envelope:");
    eprintln!("     message_type:  \"Signal\"");
    eprintln!("     session_id:    \"\"");
    eprintln!("     mode:          \"macp.mode.decision.v1\" ← VIOLATION");
    eprintln!();
    eprintln!("   RFC rule: Signals MUST carry empty mode.");
    eprintln!("   Signals exist on the ambient plane — they are not scoped to any mode.");
    eprintln!();

    let resp = client
        .send(with_sender(
            "agent://signaler",
            SendRequest {
                envelope: Some(env),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let ack = resp.ack.expect("ack present");

    let err_code = ack
        .error
        .as_ref()
        .map(|e| e.code.as_str())
        .unwrap_or("(none)");
    eprintln!("   Response:");
    eprintln!("     ack.ok:    {}", ack.ok);
    eprintln!("     ack.error: {err_code}");
    eprintln!();
    eprintln!("   Result: Signal REJECTED ✓ — mode must be empty for Signals");
    eprintln!("─────────────────────────────────────────────────────────────");

    assert!(!ack.ok, "Signal with non-empty mode must be rejected");
}

#[tokio::test]
async fn watch_signals_receives_broadcast_signal() {
    // Demonstrates the full Signal flow:
    //   Agent A subscribes to WatchSignals stream
    //   Agent B sends a Signal via Send RPC
    //   Agent A receives the Signal on the stream
    let mut watcher = common::grpc_client().await;
    let mut sender_client = common::grpc_client().await;

    let sid = new_session_id(); // session to correlate with

    // Start a session so the signal has something to correlate with
    send_as(
        &mut sender_client,
        "agent://coordinator",
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            "agent://coordinator",
            session_start_payload(
                "signal demo",
                &["agent://coordinator", "agent://worker"],
                30_000,
            ),
        ),
    )
    .await
    .unwrap();

    // Agent A subscribes to WatchSignals
    eprintln!("── Signal Broadcast Test: WatchSignals RPC ──────────────────");
    eprintln!("   Agent A (watcher) subscribes to WatchSignals stream");
    let mut signal_stream = watcher
        .watch_signals(with_sender("agent://watcher", WatchSignalsRequest {}))
        .await
        .unwrap()
        .into_inner();

    // Agent B sends a Signal
    let signal_payload = SignalPayload {
        signal_type: "progress".into(),
        data: b"analyzing fraud patterns".to_vec(),
        confidence: 0.75,
        correlation_session_id: sid.clone(),
    };
    let signal_mid = new_message_id();
    let signal_env = Envelope {
        macp_version: "1.0".into(),
        mode: String::new(),
        message_type: "Signal".into(),
        message_id: signal_mid.clone(),
        session_id: String::new(),
        sender: String::new(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload: signal_payload.encode_to_vec(),
    };

    eprintln!("   Agent B (sender) sends Signal via Send RPC:");
    eprintln!("     signal_type:             \"progress\"");
    eprintln!("     data:                    \"analyzing fraud patterns\"");
    eprintln!("     confidence:              0.75");
    eprintln!("     correlation_session_id:  \"{sid}\"");
    eprintln!();

    let ack_resp = sender_client
        .send(with_sender(
            "agent://worker",
            SendRequest {
                envelope: Some(signal_env),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let ack = ack_resp.ack.expect("ack present");
    assert!(ack.ok, "Signal should be accepted");
    eprintln!("   Send RPC ack: ok={}", ack.ok);

    // Agent A receives the Signal on the WatchSignals stream
    let received = tokio::time::timeout(std::time::Duration::from_secs(5), signal_stream.message())
        .await
        .expect("should not timeout")
        .expect("stream should not error")
        .expect("should receive a message");

    let received_env = received.envelope.expect("envelope present");
    let received_payload =
        SignalPayload::decode(&*received_env.payload).expect("valid SignalPayload");

    eprintln!();
    eprintln!("   Agent A received Signal on WatchSignals stream:");
    eprintln!("     sender:                  \"{}\"", received_env.sender);
    eprintln!(
        "     message_type:            \"{}\"",
        received_env.message_type
    );
    eprintln!(
        "     message_id:              \"{}\"",
        received_env.message_id
    );
    eprintln!("     SignalPayload:");
    eprintln!(
        "       signal_type:           \"{}\"",
        received_payload.signal_type
    );
    eprintln!(
        "       data:                 \"{}\"",
        String::from_utf8_lossy(&received_payload.data)
    );
    eprintln!(
        "       confidence:            {}",
        received_payload.confidence
    );
    eprintln!(
        "       correlation_session:  \"{}\"",
        received_payload.correlation_session_id
    );
    eprintln!();

    assert_eq!(received_env.message_id, signal_mid);
    assert_eq!(received_env.message_type, "Signal");
    assert_eq!(received_env.sender, "agent://worker");
    assert_eq!(received_payload.signal_type, "progress");
    assert_eq!(received_payload.correlation_session_id, sid);

    eprintln!("   Result: Signal BROADCAST ✓");
    eprintln!("   Agent B sent Signal → Runtime broadcast → Agent A received it");
    eprintln!("   The Signal correlates with session {sid}");
    eprintln!("   but did NOT enter the session's accepted history.");
    eprintln!("─────────────────────────────────────────────────────────────");
}

// ── Version Binding (RFC-MACP-0003 §3) ─────────────────────────────────

#[tokio::test]
async fn commitment_with_wrong_mode_version_rejected() {
    // RFC: CommitmentPayload version fields MUST match session-bound versions.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";

    // Start session (binds mode_version="1.0.0", configuration_version="config.default")
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("version test", &[coord, voter], 30_000),
        ),
    )
    .await
    .unwrap();

    // Proposal (so commitment precondition is met)
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "option-A", "test"),
        ),
    )
    .await
    .unwrap();

    // Commitment with WRONG mode_version (session bound "1.0.0", we send "2.0.0")
    let bad_commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "option-A".into(),
        authority_scope: "team".into(),
        reason: "bad version".into(),
        mode_version: "2.0.0".into(), // WRONG — session bound "1.0.0"
        policy_version: POLICY_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &sid,
            coord,
            bad_commitment,
        ),
    )
    .await
    .unwrap();
    assert!(
        !ack.ok,
        "Commitment with mismatched mode_version must be rejected"
    );
}

#[tokio::test]
async fn commitment_with_wrong_config_version_rejected() {
    // RFC: configuration_version in Commitment must match session binding.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("config version test", &[coord, voter], 30_000),
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
            proposal_payload("p1", "option-A", "test"),
        ),
    )
    .await
    .unwrap();

    let bad_commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "option-A".into(),
        authority_scope: "team".into(),
        reason: "bad config".into(),
        mode_version: MODE_VERSION.into(),
        policy_version: POLICY_VERSION.into(),
        configuration_version: "wrong-config-version".into(), // WRONG
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();

    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &sid,
            coord,
            bad_commitment,
        ),
    )
    .await
    .unwrap();
    assert!(
        !ack.ok,
        "Commitment with mismatched configuration_version must be rejected"
    );
}

// ── Deduplication (RFC-MACP-0001 §8.2 & §8.3) ─────────────────────────

#[tokio::test]
async fn rejected_message_does_not_consume_dedup_slot() {
    // RFC §8.3: Rejected Envelopes MUST NOT consume message_id dedup slots.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("dedup test", &[coord, voter], 30_000),
        ),
    )
    .await
    .unwrap();

    // Send a message that will be REJECTED (Vote before any Proposal — wrong phase)
    let shared_mid = new_message_id();
    let ack = send_as(
        &mut client,
        voter,
        envelope(
            MODE_DECISION,
            "Vote",
            &shared_mid,
            &sid,
            voter,
            vote_payload("p1", "approve", "premature"),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok, "Vote before Proposal should be rejected");

    // Now send a VALID Proposal with the same message_id.
    // Per RFC, the rejected message did NOT consume the dedup slot,
    // so this should be accepted (not flagged as duplicate).
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "option-A", "test"),
        ),
    )
    .await
    .unwrap();

    let ack = send_as(
        &mut client,
        voter,
        envelope(
            MODE_DECISION,
            "Vote",
            &shared_mid,
            &sid,
            voter,
            vote_payload("p1", "approve", "now valid"),
        ),
    )
    .await
    .unwrap();
    assert!(
        ack.ok,
        "Reused message_id from rejected message should be accepted"
    );
    assert!(!ack.duplicate, "Should NOT be flagged as duplicate");
}

#[tokio::test]
async fn duplicate_session_start_same_session_id_rejected() {
    // RFC §8.2: Duplicate SessionStart with same session_id but different message_id must be rejected.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://test";

    // First SessionStart succeeds
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent,
            session_start_payload("first", &[agent], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Second SessionStart with same session_id, different message_id — rejected
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(), // different message_id
            &sid,              // same session_id
            agent,
            session_start_payload("second", &[agent], 30_000),
        ),
    )
    .await
    .unwrap();
    assert!(
        !ack.ok,
        "Duplicate SessionStart for same session_id must be rejected"
    );
}

// ── CancelSession Authorization (RFC-MACP-0001 §7.3) ───────────────────

#[tokio::test]
async fn cancel_from_non_initiator_rejected() {
    // RFC: Only the session initiator (SessionStart sender) can cancel.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let participant = "agent://participant";

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("cancel auth test", &[coord, participant], 30_000),
        ),
    )
    .await
    .unwrap();

    // Participant (not initiator) tries to cancel — should be rejected
    let result = cancel_session_as(&mut client, participant, &sid, "unauthorized cancel").await;
    // This should either return ok=false or a gRPC error
    match result {
        Ok(ack) => assert!(!ack.ok, "Non-initiator cancel must be rejected"),
        Err(status) => {
            assert!(
                status.code() == tonic::Code::PermissionDenied,
                "Expected PERMISSION_DENIED, got: {:?}",
                status.code()
            );
        }
    }
}

// ── Discovery / Manifests (RFC-MACP-0005) ───────────────────────────────

#[tokio::test]
async fn get_manifest_returns_all_modes_including_extensions() {
    // RFC: GetManifest MAY include both standards-track and extension modes.
    let mut client = common::grpc_client().await;
    let resp = client
        .get_manifest(GetManifestRequest {
            agent_id: String::new(),
        })
        .await
        .unwrap()
        .into_inner();

    let manifest = resp.manifest.expect("manifest present");
    assert!(!manifest.agent_id.is_empty(), "agent_id should be set");
    assert!(
        !manifest.description.is_empty(),
        "description should be set"
    );

    // Should include all 5 standard modes + extensions
    assert!(
        manifest.supported_modes.len() >= 6,
        "GetManifest should include standard + extension modes, got {}",
        manifest.supported_modes.len()
    );
    assert!(manifest
        .supported_modes
        .contains(&"macp.mode.decision.v1".to_string()));
    assert!(manifest
        .supported_modes
        .contains(&"ext.multi_round.v1".to_string()));
}

#[tokio::test]
async fn initialize_rejects_unsupported_protocol_version() {
    // RFC: If no mutually supported version, initialization MUST fail.
    let mut client = common::grpc_client().await;
    let result = client
        .initialize(InitializeRequest {
            supported_protocol_versions: vec!["99.0".into()],
            client_info: None,
            capabilities: None,
        })
        .await;

    assert!(
        result.is_err(),
        "Initialize with unsupported version must fail"
    );
    let status = result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::FailedPrecondition,
        "Expected FAILED_PRECONDITION for version mismatch"
    );
}

// ── TTL Edge Cases ──────────────────────────────────────────────────────

#[tokio::test]
async fn session_accepts_messages_within_ttl_window() {
    // Verify session is alive and accepts messages before TTL expires.
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let coord = "agent://coordinator";
    let voter = "agent://voter";

    // Start session with generous TTL
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            coord,
            session_start_payload("ttl test", &[coord, voter], 30_000),
        ),
    )
    .await
    .unwrap();

    // Send immediately — well within TTL
    let ack = send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            coord,
            proposal_payload("p1", "quick proposal", "within TTL"),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok, "Message within TTL window must be accepted");
    assert_eq!(ack.session_state, 1, "Session should still be OPEN");
}

#[tokio::test]
async fn watch_signals_requires_authentication() {
    // The ambient signal plane carries agent-generated payloads; an
    // unauthenticated caller must not be able to subscribe.
    let mut client = common::grpc_client().await;
    let err = client
        .watch_signals(WatchSignalsRequest {})
        .await
        .expect_err("unauthenticated WatchSignals must be rejected");
    assert_eq!(err.code(), tonic::Code::Unauthenticated);
}
