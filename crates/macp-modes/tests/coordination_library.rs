//! Library-consumability acceptance test (build-coordination-library.md, Part D).
//!
//! Drives a full decision session — start → proposal → commitment → resolved —
//! using ONLY `macp-core` + `macp-modes` + `macp-policy`. There is no
//! `macp-runtime`, gRPC server, storage backend, or tokio anywhere in this test:
//! it constructs a mode with an injected evaluator and steps messages through
//! `macp_modes::step`, exactly as an embedding library consumer would — the
//! session itself comes from `accept_session_start`, the same acceptance path
//! the server kernel runs.

use std::sync::Arc;

use macp_core::session::{Session, SessionState};
use macp_modes::mode::decision::DecisionMode;
use macp_modes::step::{accept_session_start, step, StartOptions, StepOutcome};
use macp_pb::decision_pb::ProposalPayload;
use macp_pb::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use prost::Message;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const MODE: &str = "macp.mode.decision.v1";
const INITIATOR: &str = "agent://orchestrator";

fn session(mode: &DecisionMode) -> Session {
    let payload = SessionStartPayload {
        intent: "release".into(),
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
    .encode_to_vec();
    accept_session_start(
        &env(INITIATOR, "SessionStart", "start", payload),
        0,
        mode,
        None,
        &StartOptions::default(),
    )
    .expect("session start accepted")
}

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

#[test]
fn drives_a_full_decision_session_without_the_runtime() {
    // A consumer supplies its own evaluator; here we reuse the default one.
    let mode = DecisionMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator));
    let mut s = session(&mode);
    assert_eq!(s.state, SessionState::Open);

    // A proposal from the initiator (a declared participant).
    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "ship-it".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    let out = step(
        &mut s,
        &env(INITIATOR, "Proposal", "m-prop", proposal),
        &mode,
        1,
    )
    .expect("proposal accepted");
    assert!(matches!(
        out,
        StepOutcome::Accepted {
            state: SessionState::Open,
            ..
        }
    ));

    // The initiator commits — versions must match the session bindings.
    let commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "release".into(),
        reason: "bound".into(),
        mode_version: s.mode_version.clone(),
        policy_version: s.policy_version.clone(),
        configuration_version: s.configuration_version.clone(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();
    let out = step(
        &mut s,
        &env(INITIATOR, "Commitment", "m-commit", commitment),
        &mode,
        2,
    )
    .expect("commitment accepted");

    // The session resolved — entirely through the library surface.
    assert!(matches!(
        out,
        StepOutcome::Accepted {
            state: SessionState::Resolved,
            ..
        }
    ));
    assert_eq!(s.state, SessionState::Resolved);
    assert!(s.resolution.is_some());
}

#[test]
fn rejected_message_does_not_consume_a_dedup_slot() {
    // The dedup invariant holds for a library consumer too: a non-participant's
    // message is rejected and its id stays available for a later valid message.
    let mode = DecisionMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator));
    let mut s = session(&mode);

    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "ship-it".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();

    // Stranger is not a participant → rejected, dedup slot NOT consumed.
    assert!(step(
        &mut s,
        &env("agent://stranger", "Proposal", "m-prop", proposal.clone()),
        &mode,
        1,
    )
    .is_err());
    assert!(!s.seen_message_ids.contains("m-prop"));

    // Same message id from a real participant is accepted.
    let out = step(
        &mut s,
        &env(INITIATOR, "Proposal", "m-prop", proposal),
        &mode,
        1,
    )
    .expect("re-used id accepted from a participant");
    assert!(matches!(
        out,
        StepOutcome::Accepted {
            state: SessionState::Open,
            ..
        }
    ));
}
