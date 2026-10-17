//! W0-4: the RFC-MACP-0010 §5.1(2) synthetic implicit accept must fire for an
//! embedder that drives a handoff session through `macp_modes::step` alone —
//! no `macp-runtime`, no server sweep, only an injected clock.

use std::sync::Arc;

use macp_core::policy::PolicyDefinition;
use macp_core::session::{Session, SessionState};
use macp_modes::mode::handoff::HandoffMode;
use macp_modes::mode::Mode;
use macp_modes::step::{step, StepOutcome};
use macp_pb::handoff_pb::HandoffOfferPayload;
use macp_pb::pb::{CommitmentPayload, Envelope};
use prost::Message;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const MODE: &str = "macp.mode.handoff.v1";
const OWNER: &str = "agent://owner";
const TARGET: &str = "agent://target";
const TIMEOUT_MS: i64 = 60;
const OFFERED_AT: i64 = 1_000;

fn mode() -> HandoffMode {
    HandoffMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator))
}

fn session() -> Session {
    Session::builder(SESSION_ID, MODE, OWNER)
        .ttl_ms(60_000)
        .ttl_expiry(OFFERED_AT + 60_000)
        .participants(vec![OWNER.into(), TARGET.into()])
        .mode_version("1.0.0")
        .configuration_version("cfg-1")
        .policy_version("handoff-auto")
        .policy_definition(Some(PolicyDefinition {
            policy_id: "handoff-auto".into(),
            mode: MODE.into(),
            description: "implicit accept".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": TIMEOUT_MS },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        }))
        .build()
}

/// Envelope timestamps are deliberately `0`: at `semantics_rev >= 1` the
/// handoff clock is the acceptance clock the embedder injects, never the
/// client-supplied timestamp.
fn env(sender: &str, message_type: &str, message_id: &str, payload: Vec<u8>) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: MODE.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: SESSION_ID.into(),
        sender: sender.into(),
        timestamp_unix_ms: 0,
        payload,
    }
}

fn offer() -> Envelope {
    let payload = HandoffOfferPayload {
        handoff_id: "h1".into(),
        target_participant: TARGET.into(),
        scope: "support".into(),
        reason: "escalate".into(),
    }
    .encode_to_vec();
    env(OWNER, "HandoffOffer", "offer-1", payload)
}

fn commitment(s: &Session) -> Envelope {
    let payload = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "handoff.accepted".into(),
        authority_scope: "support".into(),
        reason: "bound".into(),
        mode_version: s.mode_version.clone(),
        policy_version: s.policy_version.clone(),
        configuration_version: s.configuration_version.clone(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();
    env(OWNER, "Commitment", "commit-1", payload)
}

fn started() -> (HandoffMode, Session) {
    let mode = mode();
    let mut s = session();
    let resp = mode
        .on_session_start(&s, &env(OWNER, "SessionStart", "start", vec![]))
        .expect("session start");
    s.apply_mode_response(resp);
    s.seen_message_ids.insert("start".into());
    step(&mut s, &offer(), &mode, OFFERED_AT).expect("offer accepted");
    (mode, s)
}

#[test]
fn step_only_embedder_gets_the_implicit_accept() {
    let (mode, mut s) = started();
    let now = OFFERED_AT + TIMEOUT_MS + 500;
    let commit = commitment(&s);
    let out = step(&mut s, &commit, &mode, now).expect("commitment accepted");
    let StepOutcome::Accepted {
        state, synthesized, ..
    } = out
    else {
        panic!("expected Accepted, got {out:?}");
    };
    assert_eq!(state, SessionState::Resolved);
    assert_eq!(synthesized.len(), 1, "exactly one synthetic accept");
    let syn = &synthesized[0];
    assert_eq!(syn.message_id, "implicit-accept:h1");
    assert_eq!(syn.message_type, "HandoffAccept");
    assert_eq!(syn.sender, TARGET);
    // §5.1(3): stamped with the computed deadline, not the observation time.
    assert_eq!(syn.timestamp_unix_ms, OFFERED_AT + TIMEOUT_MS);
    assert!(s.seen_message_ids.contains("implicit-accept:h1"));
    // No participant activity is credited for the synthetic (replay parity).
    assert_eq!(s.participant_message_counts.get(TARGET), None);
}

#[test]
fn a_completed_suspension_is_excluded_from_the_window() {
    let (mode, mut s) = started();
    // 40ms paused inside the window pushes the deadline out by 40ms.
    s.suspend(OFFERED_AT + 10).unwrap();
    s.resume(OFFERED_AT + 50).unwrap();
    let commit = commitment(&s);
    let out = step(&mut s, &commit, &mode, OFFERED_AT + 500).expect("commitment accepted");
    let StepOutcome::Accepted { synthesized, .. } = out else {
        panic!("expected Accepted, got {out:?}");
    };
    assert_eq!(synthesized.len(), 1);
    assert_eq!(
        synthesized[0].timestamp_unix_ms,
        OFFERED_AT + TIMEOUT_MS + 40
    );
}

#[test]
fn nothing_is_synthesized_before_the_deadline() {
    let (mode, mut s) = started();
    let commit = commitment(&s);
    let err = step(&mut s, &commit, &mode, OFFERED_AT + TIMEOUT_MS - 1).unwrap_err();
    // Offer still outstanding: the commitment is refused, and the refusal
    // consumed nothing.
    assert!(!s.seen_message_ids.contains("commit-1"));
    assert!(!s.seen_message_ids.contains("implicit-accept:h1"));
    let _ = err;
}

#[test]
fn a_rejected_trigger_still_records_the_due_accept() {
    // Freeze-profile carve-out (CLAUDE.md): the synthetic was due
    // independently of the trigger, so it stays in history even when the
    // trigger is refused; the trigger's own id stays free.
    let (mode, mut s) = started();
    let stranger = env("agent://stranger", "HandoffContext", "ctx-1", vec![]);
    // Unauthorized triggers synthesize nothing (authorization precedes
    // synthesis, as in the server kernel).
    assert!(step(&mut s, &stranger, &mode, OFFERED_AT + 500).is_err());
    assert!(!s.seen_message_ids.contains("implicit-accept:h1"));
    // An authorized but invalid trigger: the accept is recorded first.
    let bad = env(OWNER, "HandoffContext", "ctx-2", b"not a payload".to_vec());
    assert!(step(&mut s, &bad, &mode, OFFERED_AT + 500).is_err());
    assert!(s.seen_message_ids.contains("implicit-accept:h1"));
    assert!(!s.seen_message_ids.contains("ctx-2"));
}
