//! `macp_modes::replay` rebuilds exactly the session `macp_modes::step` built
//! live — the durable-embedder contract (RFC-MACP-0003 §1): record the start
//! and every event, replay them, get the same session back.

use std::sync::Arc;

use macp_core::policy::PolicyDefinition;
use macp_core::session::{Session, SessionState};
use macp_modes::mode::handoff::HandoffMode;
use macp_modes::replay::{
    apply_event, replay, replay_start, RecordedStart, ReplayEvent, ReplayOptions,
};
use macp_modes::step::{accept_session_start, step, StartOptions, StepOutcome};
use macp_pb::handoff_pb::HandoffOfferPayload;
use macp_pb::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use prost::Message;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const MODE: &str = "macp.mode.handoff.v1";
const OWNER: &str = "agent://owner";
const TARGET: &str = "agent://target";
const POLICY_ID: &str = "handoff-auto";
const TIMEOUT_MS: i64 = 60;
/// The client's SessionStart clock — deliberately behind the acceptance clock,
/// so a replay that based the TTL on the wrong one shows up.
const CLIENT_START_TS: i64 = 900;
const STARTED_AT: i64 = 950;
const OFFERED_AT: i64 = 1_000;

fn mode() -> HandoffMode {
    HandoffMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator))
}

fn policy() -> PolicyDefinition {
    PolicyDefinition {
        policy_id: POLICY_ID.into(),
        mode: MODE.into(),
        description: "implicit accept".into(),
        rules: serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": TIMEOUT_MS },
            "commitment": { "authority": "initiator_only" }
        }),
        schema_version: 1,
    }
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

fn start_env() -> Envelope {
    let payload = SessionStartPayload {
        intent: "escalate".into(),
        participants: vec![OWNER.into(), TARGET.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: POLICY_ID.into(),
        ttl_ms: 60_000,
        max_suspend_ms: 5_000,
        ..Default::default()
    };
    Envelope {
        timestamp_unix_ms: CLIENT_START_TS,
        ..env(OWNER, "SessionStart", "start", payload.encode_to_vec())
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

/// A durable embedder's step: run [`step`] and record what it accepted — the
/// synthesized envelopes first, then the message (`StepOutcome` order).
fn step_and_record(
    s: &mut Session,
    e: &Envelope,
    m: &HandoffMode,
    now: i64,
    log: &mut Vec<ReplayEvent>,
) {
    match step(s, e, m, now).expect("accepted") {
        StepOutcome::Accepted { synthesized, .. } => {
            log.extend(
                synthesized
                    .into_iter()
                    .map(|envelope| ReplayEvent::Synthetic { envelope }),
            );
            log.push(ReplayEvent::Message {
                envelope: e.clone(),
                accepted_at_ms: now,
            });
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
}

/// The live run: start, offer, a suspension inside the window, then a
/// commitment past the (suspension-corrected) deadline that triggers the
/// synthetic implicit accept.
fn live_run() -> (HandoffMode, RecordedStart, Vec<ReplayEvent>, Session) {
    let m = mode();
    let start = start_env();
    let mut s = accept_session_start(
        &start,
        STARTED_AT,
        &m,
        Some(policy()),
        &StartOptions::default(),
    )
    .expect("session starts");
    let recorded = RecordedStart::from_session(start, &s, None);
    let mut log = Vec::new();

    step_and_record(&mut s, &offer(), &m, OFFERED_AT, &mut log);
    s.suspend(OFFERED_AT + 10).unwrap();
    log.push(ReplayEvent::Suspended {
        at_ms: OFFERED_AT + 10,
    });
    s.resume(OFFERED_AT + 50).unwrap();
    log.push(ReplayEvent::Resumed {
        at_ms: OFFERED_AT + 50,
    });
    let commit = commitment(&s);
    step_and_record(&mut s, &commit, &m, OFFERED_AT + 500, &mut log);
    assert_eq!(s.state, SessionState::Resolved);
    (m, recorded, log, s)
}

fn assert_same_session(replayed: &Session, live: &Session) {
    assert_eq!(replayed.session_id, live.session_id);
    assert_eq!(replayed.state, live.state);
    assert_eq!(replayed.resolution, live.resolution);
    assert_eq!(replayed.mode_state, live.mode_state, "mode_state");
    assert_eq!(replayed.seen_message_ids, live.seen_message_ids);
    assert_eq!(replayed.ttl_expiry, live.ttl_expiry, "ttl_expiry");
    assert_eq!(replayed.ttl_ms, live.ttl_ms);
    assert_eq!(replayed.started_at_unix_ms, live.started_at_unix_ms);
    assert_eq!(replayed.participants, live.participants);
    assert_eq!(replayed.mode_version, live.mode_version);
    assert_eq!(replayed.configuration_version, live.configuration_version);
    assert_eq!(replayed.policy_version, live.policy_version);
    let policy_of = |s: &Session| {
        s.policy_definition
            .as_ref()
            .map(|p| (p.policy_id.clone(), p.rules.clone(), p.schema_version))
    };
    assert_eq!(policy_of(replayed), policy_of(live));
    assert_eq!(replayed.semantics_rev, live.semantics_rev);
    assert_eq!(replayed.max_suspend_ms, live.max_suspend_ms);
    assert_eq!(replayed.suspended_at_ms, live.suspended_at_ms);
    assert_eq!(
        replayed.accumulated_suspended_ms,
        live.accumulated_suspended_ms
    );
    assert_eq!(replayed.suspension_intervals, live.suspension_intervals);
    assert_eq!(
        replayed.participant_message_counts,
        live.participant_message_counts
    );
    assert_eq!(replayed.participant_last_seen, live.participant_last_seen);
}

#[test]
fn replay_rebuilds_the_live_session() {
    let (m, recorded, log, live) = live_run();
    // The history holds the synthetic accept, stamped with the deadline.
    let syn = log
        .iter()
        .find_map(|e| match e {
            ReplayEvent::Synthetic { envelope } => Some(envelope),
            _ => None,
        })
        .expect("a synthetic accept was recorded");
    assert_eq!(syn.timestamp_unix_ms, OFFERED_AT + TIMEOUT_MS + 40);
    // RFC-MACP-0003 §2: the deadline is the client's SessionStart timestamp
    // plus ttl_ms (not the acceptance clock), plus the 40ms the resume banked.
    assert_eq!(live.ttl_expiry, CLIENT_START_TS + 60_000 + 40);
    assert_eq!(live.max_suspend_ms, 5_000);

    let replayed = replay(&recorded, log, &m, &ReplayOptions::default()).expect("replays");
    assert_same_session(&replayed, &live);
}

#[test]
fn resuming_from_a_checkpoint_matches_a_full_replay() {
    let (m, recorded, log, live) = live_run();
    let opts = ReplayOptions::default();
    let (head, tail) = log.split_at(2);
    // A recorder's checkpoint is any session it rebuilt; persist, reload, and
    // apply the rest with `apply_event`.
    let mut checkpoint = replay(&recorded, head.to_vec(), &m, &opts).unwrap();
    for event in tail.iter().cloned() {
        apply_event(&mut checkpoint, event, &m, &opts).unwrap();
    }
    assert_same_session(&checkpoint, &live);
}

#[test]
fn a_recorder_that_cannot_tell_synthetics_apart_credits_no_activity() {
    // The server log records participant and synthetic envelopes alike; with
    // crediting off, replay leaves activity empty rather than credit the
    // synthetic accept to the target.
    let (m, recorded, log, _) = live_run();
    let flattened = log.into_iter().map(|e| match e {
        ReplayEvent::Synthetic { envelope } => ReplayEvent::Message {
            accepted_at_ms: envelope.timestamp_unix_ms,
            envelope,
        },
        other => other,
    });
    let opts = ReplayOptions::default().with_credit_participant_activity(false);
    let replayed = replay(&recorded, flattened, &m, &opts).unwrap();
    assert_eq!(replayed.state, SessionState::Resolved);
    assert!(replayed.participant_message_counts.is_empty());
    assert!(replayed.seen_message_ids.contains("implicit-accept:h1"));
}

#[test]
fn a_history_the_mode_refuses_fails_loudly() {
    // A commitment before the offer was accepted — not a history `step` could
    // have produced. Replay refuses it rather than build a different session.
    let (m, recorded, _, live) = live_run();
    let events = vec![ReplayEvent::Message {
        envelope: commitment(&live),
        accepted_at_ms: OFFERED_AT,
    }];
    assert!(replay(&recorded, events, &m, &ReplayOptions::default()).is_err());
}

#[test]
fn an_event_after_the_terminal_transition_only_takes_its_dedup_slot() {
    let (m, recorded, mut log, live) = live_run();
    log.push(ReplayEvent::Message {
        envelope: env(TARGET, "HandoffContext", "late-1", b"garbage".to_vec()),
        accepted_at_ms: OFFERED_AT + 600,
    });
    let replayed = replay(&recorded, log, &m, &ReplayOptions::default()).unwrap();
    assert_eq!(replayed.state, SessionState::Resolved);
    assert_eq!(replayed.mode_state, live.mode_state);
    assert!(replayed.seen_message_ids.contains("late-1"));
}

#[test]
fn an_empty_requested_policy_binds_the_resolved_id() {
    // `ValidatedStart::accept` binds the resolved definition's id when the
    // payload named none; replay must agree.
    let m = mode();
    let mut payload = SessionStartPayload::decode(start_env().payload.as_slice()).unwrap();
    payload.policy_version.clear();
    let start = Envelope {
        payload: payload.encode_to_vec(),
        ..start_env()
    };
    let live = accept_session_start(
        &start,
        STARTED_AT,
        &m,
        Some(policy()),
        &StartOptions::default(),
    )
    .unwrap();
    assert_eq!(live.policy_version, POLICY_ID);
    let replayed = replay_start(&RecordedStart::from_session(start, &live, None), &m).unwrap();
    assert_same_session(&replayed, &live);
}
