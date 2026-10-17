//! `accept_session_start` — the library SessionStart acceptance path (S12).
//! The same checks the server kernel runs, with no server.

use std::sync::Arc;

use macp_core::error::MacpError;
use macp_core::policy::PolicyDefinition;
use macp_core::session::{SessionState, CURRENT_SEMANTICS_REV, MAX_SUSPEND_MS};
use macp_modes::mode::decision::DecisionMode;
use macp_modes::step::{
    accept_session_start, step, validate_session_start, StartOptions, StepOutcome,
};
use macp_pb::decision_pb::ProposalPayload;
use macp_pb::pb::{Envelope, SessionStartPayload};
use prost::Message;

const SID: &str = "11111111-1111-4111-8111-111111111111";
const MODE: &str = "macp.mode.decision.v1";
const INITIATOR: &str = "agent://orchestrator";
const NOW: i64 = 1_700_000_000_000;

fn mode() -> DecisionMode {
    DecisionMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator))
}

fn payload() -> SessionStartPayload {
    SessionStartPayload {
        intent: "pick".into(),
        participants: vec![INITIATOR.into(), "agent://fraud".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: String::new(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: Default::default(),
        roots: vec![],
        max_suspend_ms: 0,
    }
}

fn start(sid: &str, p: &SessionStartPayload) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: MODE.into(),
        message_type: "SessionStart".into(),
        message_id: "start-1".into(),
        session_id: sid.into(),
        sender: INITIATOR.into(),
        timestamp_unix_ms: 0,
        payload: p.encode_to_vec(),
    }
}

fn policy(id: &str, mode: &str) -> PolicyDefinition {
    PolicyDefinition {
        policy_id: id.into(),
        mode: mode.into(),
        description: "test".into(),
        rules: serde_json::json!({}),
        schema_version: 1,
    }
}

fn accept(
    env: &Envelope,
    policy: Option<PolicyDefinition>,
) -> Result<macp_core::session::Session, MacpError> {
    accept_session_start(env, NOW, &mode(), policy, &StartOptions::default())
}

#[test]
fn accepts_a_canonical_start_with_every_binding() {
    let s = accept(&start(SID, &payload()), Some(policy("policy.default", "*"))).unwrap();
    assert_eq!(s.state, SessionState::Open);
    assert_eq!(s.semantics_rev, CURRENT_SEMANTICS_REV);
    assert_eq!(
        s.ttl_expiry,
        NOW + 60_000,
        "no envelope timestamp: TTL base is now"
    );
    assert_eq!(s.started_at_unix_ms, NOW);
    assert_eq!(s.max_suspend_ms, MAX_SUSPEND_MS);
    assert_eq!(s.policy_version, "policy.default");
    assert_eq!(s.initiator_sender, INITIATOR);
    assert!(s.seen_message_ids.contains("start-1"));
    assert!(
        !s.mode_state.is_empty(),
        "the mode's initial state is applied"
    );
}

#[test]
fn ttl_base_is_the_envelope_timestamp_when_present() {
    let mut env = start(SID, &payload());
    env.timestamp_unix_ms = 5_000;
    let s = accept(&env, None).unwrap();
    assert_eq!(s.ttl_expiry, 65_000);
    assert_eq!(s.policy_version, "");
}

/// RFC-MACP-0004 §11.2 requires refusing ids too weak to be unguessable and
/// documenting the accepted formats; it does not fix the format list. These
/// vectors pin *this runtime's* documented formats (`docs/protocol.md`):
/// lowercase-hyphenated UUID v4/v7, or a 22+ character base64url token. The
/// uppercase refusal is that local canonical-form rule (one UUID, one id), not
/// a spec requirement.
#[test]
fn refuses_weak_or_non_canonical_session_ids() {
    for sid in [
        "s1",
        "abcdefghijklmnopqrstu",                // 21 chars: too short
        "abcdefghijklmnopqrstuvw.",             // not base64url
        "AAAAAAAA-1111-4111-8111-11111111111A", // uppercase UUID v4
        "11111111-1111-1111-8111-111111111111", // UUID v1
    ] {
        assert!(
            matches!(
                accept(&start(sid, &payload()), None),
                Err(MacpError::InvalidSessionId)
            ),
            "{sid}"
        );
    }
}

#[test]
fn a_custom_validator_can_be_stricter() {
    let opts = StartOptions::default().with_session_id_validator(|sid| {
        if sid.starts_with("11111111") {
            Err(MacpError::InvalidSessionId)
        } else {
            Ok(())
        }
    });
    let err = accept_session_start(&start(SID, &payload()), NOW, &mode(), None, &opts).unwrap_err();
    assert!(matches!(err, MacpError::InvalidSessionId));
}

#[test]
fn refuses_wrong_protocol_version_and_missing_fields() {
    let mut env = start(SID, &payload());
    env.macp_version = "2.0".into();
    assert!(matches!(
        accept(&env, None),
        Err(MacpError::InvalidMacpVersion)
    ));
    for field in ["message_id", "sender", "message_type", "mode"] {
        let mut env = start(SID, &payload());
        match field {
            "message_id" => env.message_id.clear(),
            "sender" => env.sender.clear(),
            "message_type" => env.message_type = "Proposal".into(),
            _ => env.mode = " ".into(),
        }
        assert!(
            matches!(accept(&env, None), Err(MacpError::InvalidEnvelope)),
            "{field}"
        );
    }
}

#[test]
fn refuses_a_non_canonical_payload() {
    for mutate in [
        (|p: &mut SessionStartPayload| p.mode_version.clear()) as fn(&mut SessionStartPayload),
        |p| p.configuration_version.clear(),
        |p| p.ttl_ms = 0,
        |p| p.participants = vec![INITIATOR.into(), INITIATOR.into()],
    ] {
        let mut p = payload();
        mutate(&mut p);
        assert!(accept(&start(SID, &p), None).is_err(), "{p:?}");
    }
    let mut env = start(SID, &payload());
    env.payload = b"\xff\xff".to_vec();
    assert!(accept(&env, None).is_err());
}

#[test]
fn binds_the_policy_to_the_mode() {
    let mut p = payload();
    p.policy_version = "policy.task-only".into();
    let env = start(SID, &p);
    assert!(matches!(
        accept(&env, Some(policy("policy.task-only", "macp.mode.task.v1"))),
        Err(MacpError::InvalidPolicyDefinition)
    ));
    // "*" binds every mode.
    assert_eq!(
        accept(&env, Some(policy("policy.task-only", "*")))
            .unwrap()
            .policy_version,
        "policy.task-only"
    );
    // Named but unresolved, or resolved to a different definition.
    assert!(matches!(
        accept(&env, None),
        Err(MacpError::UnknownPolicyVersion)
    ));
    assert!(matches!(
        accept(&env, Some(policy("policy.other", "*"))),
        Err(MacpError::UnknownPolicyVersion)
    ));
}

#[test]
fn descriptor_version_is_enforced_and_bound() {
    let opts = StartOptions::default().with_descriptor_mode_version("2.0.0");
    let err = accept_session_start(&start(SID, &payload()), NOW, &mode(), None, &opts).unwrap_err();
    assert!(matches!(err, MacpError::InvalidEnvelope));
    // A non-strict (extension) start omitting mode_version binds the descriptor's.
    let mut p = payload();
    p.mode_version.clear();
    let opts = opts.with_strict(false);
    let v = validate_session_start(&start(SID, &p), NOW, &opts).unwrap();
    assert_eq!(v.bound_mode_version.as_deref(), Some("2.0.0"));
}

#[test]
fn suspension_cap_resolution() {
    let mut p = payload();
    p.max_suspend_ms = 1_000;
    assert_eq!(accept(&start(SID, &p), None).unwrap().max_suspend_ms, 1_000);
    let opts = StartOptions::default().with_default_max_suspend_ms(5);
    let s = accept_session_start(&start(SID, &payload()), NOW, &mode(), None, &opts).unwrap();
    assert_eq!(s.max_suspend_ms, 5);
}

fn proposal(id: &str) -> Envelope {
    let mut e = start(SID, &payload());
    e.message_type = "Proposal".into();
    e.message_id = id.into();
    e.payload = ProposalPayload {
        proposal_id: id.into(),
        option: "ship".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    e
}

#[test]
fn step_expires_past_the_ttl_and_past_the_suspension_cap() {
    let m = mode();
    let mut s = accept(&start(SID, &payload()), None).unwrap();
    let err = step(&mut s, &proposal("p1"), &m, NOW + 60_001).unwrap_err();
    assert!(matches!(err, MacpError::TtlExpired));
    assert_eq!(s.state, SessionState::Expired);

    // Suspended past its cap: the next message expires it, not SessionNotOpen.
    let mut p = payload();
    p.max_suspend_ms = 100;
    let mut s = accept(&start(SID, &p), None).unwrap();
    s.suspend(NOW + 10).unwrap();
    let err = step(&mut s, &proposal("p2"), &m, NOW + 50).unwrap_err();
    assert!(matches!(err, MacpError::SessionNotOpen), "within the cap");
    let err = step(&mut s, &proposal("p2"), &m, NOW + 200).unwrap_err();
    assert!(matches!(err, MacpError::TtlExpired));
    assert_eq!(s.state, SessionState::Expired);
    assert_eq!(s.suspended_at_ms, None);
}

#[test]
fn step_refuses_malformed_envelopes_without_consuming_dedup() {
    let m = mode();
    let mut s = accept(&start(SID, &payload()), None).unwrap();
    let mut e = proposal("p1");
    e.sender.clear();
    assert!(matches!(
        step(&mut s, &e, &m, NOW + 1),
        Err(MacpError::InvalidEnvelope)
    ));
    let mut e = proposal("p1");
    e.macp_version = "0.9".into();
    assert!(matches!(
        step(&mut s, &e, &m, NOW + 1),
        Err(MacpError::InvalidMacpVersion)
    ));
    let mut e = proposal("p1");
    e.session_id = "22222222-2222-4222-8222-222222222222".into();
    assert!(matches!(
        step(&mut s, &e, &m, NOW + 1),
        Err(MacpError::InvalidEnvelope)
    ));
    assert!(!s.seen_message_ids.contains("p1"));
    assert!(matches!(
        step(&mut s, &proposal("p1"), &m, NOW + 1).unwrap(),
        StepOutcome::Accepted {
            state: SessionState::Open,
            ..
        }
    ));
}
