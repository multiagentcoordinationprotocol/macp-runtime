//! `macp_modes::what_if::replay_under_policy` — re-drive a recorded Decision
//! session under a candidate policy and diff the outcome against the record.

use std::sync::Arc;

use macp_core::policy::PolicyDefinition;
use macp_core::session::{SessionState, CURRENT_SEMANTICS_REV};
use macp_modes::mode::decision::DecisionMode;
use macp_modes::replay::{replay, RecordedStart, ReplayEvent, ReplayOptions};
use macp_modes::what_if::{replay_under_policy, EntryOutcome, HistoryPoint};
use macp_pb::decision_pb::{ObjectionPayload, ProposalPayload, VotePayload};
use macp_pb::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use prost::Message;

const SESSION_ID: &str = "22222222-2222-4222-8222-222222222222";
const MODE: &str = "macp.mode.decision.v1";
const LEAD: &str = "agent://lead";
const FRAUD: &str = "agent://fraud";
const RISK: &str = "agent://risk";
const RECORDED_POLICY: &str = "policy.default";
const STARTED_AT: i64 = 1_000;

fn mode() -> DecisionMode {
    DecisionMode::new(Arc::new(macp_policy::DefaultPolicyEvaluator))
}

fn policy(id: &str, rules: serde_json::Value) -> PolicyDefinition {
    PolicyDefinition {
        policy_id: id.into(),
        mode: MODE.into(),
        description: id.into(),
        rules,
        schema_version: 1,
    }
}

/// A single critical objection vetoes every commitment.
fn veto_policy() -> PolicyDefinition {
    policy(
        "policy.veto-on-critical",
        serde_json::json!({
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }),
    )
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

fn start() -> RecordedStart {
    let payload = SessionStartPayload {
        intent: "approve a payment".into(),
        participants: vec![LEAD.into(), FRAUD.into(), RISK.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: RECORDED_POLICY.into(),
        ttl_ms: 60_000,
        ..Default::default()
    };
    RecordedStart::new(
        env(LEAD, "SessionStart", "m-start", payload.encode_to_vec()),
        STARTED_AT,
    )
    .with_semantics_rev(CURRENT_SEMANTICS_REV)
    .with_policy(Some(policy(RECORDED_POLICY, serde_json::json!({}))))
}

fn message(at: i64, envelope: Envelope) -> ReplayEvent {
    ReplayEvent::Message {
        envelope,
        accepted_at_ms: STARTED_AT + at,
    }
}

/// Propose, a critical objection, two approvals, then the lead commits.
fn events() -> Vec<ReplayEvent> {
    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "approve".into(),
        rationale: "low risk".into(),
        supporting_data: vec![],
    };
    let objection = ObjectionPayload {
        proposal_id: "p1".into(),
        reason: "new device".into(),
        severity: "critical".into(),
    };
    let vote = |reason: &str| VotePayload {
        proposal_id: "p1".into(),
        vote: "APPROVE".into(),
        reason: reason.into(),
    };
    let commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "payments".into(),
        reason: "approved".into(),
        mode_version: "1.0.0".into(),
        policy_version: RECORDED_POLICY.into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    };
    vec![
        message(
            10,
            env(LEAD, "Proposal", "m-proposal", proposal.encode_to_vec()),
        ),
        message(
            20,
            env(RISK, "Objection", "m-objection", objection.encode_to_vec()),
        ),
        message(
            30,
            env(FRAUD, "Vote", "m-vote-fraud", vote("ok").encode_to_vec()),
        ),
        message(
            40,
            env(RISK, "Vote", "m-vote-risk", vote("fine").encode_to_vec()),
        ),
        message(
            50,
            env(LEAD, "Commitment", "m-commit", commitment.encode_to_vec()),
        ),
    ]
}

#[test]
fn a_vetoing_candidate_denies_the_recorded_commitment() {
    let report = replay_under_policy(
        &start(),
        events(),
        &mode(),
        &ReplayOptions::default(),
        &veto_policy(),
    )
    .unwrap();

    assert_eq!(report.recorded.state, Some(SessionState::Resolved));
    let by = report.recorded.resolved_by.as_ref().unwrap();
    assert_eq!(by.message_id, "m-commit");
    assert_eq!(by.at, HistoryPoint::Event(4));

    assert_eq!(report.candidate.state, Some(SessionState::Open));
    assert_eq!(report.candidate.resolved_by, None);
    assert!(report.outcome_changed());
    assert_eq!(report.first_divergence, Some(HistoryPoint::Event(4)));

    assert_eq!(report.denials.len(), 1);
    assert_eq!(report.denials[0].message_id.as_deref(), Some("m-commit"));
    assert!(!report.denials[0].reasons.is_empty());
    match &report.entries.last().unwrap().candidate {
        EntryOutcome::Refused { code, reasons } => {
            assert_eq!(*code, "POLICY_DENIED");
            assert_eq!(reasons, &report.denials[0].reasons);
        }
        other => panic!("expected the commitment refused, got {other:?}"),
    }

    // The refused commitment took no dedup slot, as live rejection would not.
    let cand = report.candidate_session.as_ref().unwrap();
    assert!(!cand.seen_message_ids.contains("m-commit"));
    // Only the rules were substituted: the binding is still the recorded one.
    assert_eq!(cand.policy_version, RECORDED_POLICY);
    assert_eq!(report.candidate_policy_id, "policy.veto-on-critical");
    assert_eq!(report.recorded_policy_id.as_deref(), Some(RECORDED_POLICY));
}

#[test]
fn the_recorded_policy_as_candidate_changes_nothing() {
    let recorded = start().policy.unwrap();
    let report = replay_under_policy(
        &start(),
        events(),
        &mode(),
        &ReplayOptions::default(),
        &recorded,
    )
    .unwrap();
    assert!(!report.outcome_changed());
    assert_eq!(report.first_divergence, None);
    assert!(report.denials.is_empty());
    assert_eq!(report.entries.len(), 1 + events().len());
    assert!(report.entries.iter().all(|e| !e.diverges()));
}

#[test]
fn the_recorded_half_matches_plain_replay() {
    let plain = replay(&start(), events(), &mode(), &ReplayOptions::default()).unwrap();
    let report = replay_under_policy(
        &start(),
        events(),
        &mode(),
        &ReplayOptions::default(),
        &veto_policy(),
    )
    .unwrap();
    assert_eq!(report.recorded.state, Some(plain.state));
    assert_eq!(report.recorded.resolution, plain.resolution);
}

#[test]
fn a_history_the_mode_refuses_is_still_an_integrity_error() {
    let mut events = events();
    // The commitment twice: the recorded replay refuses the second, so the
    // history is not one this mode produced.
    let ReplayEvent::Message { envelope, .. } = events[4].clone() else {
        unreachable!()
    };
    events.insert(
        4,
        message(
            45,
            Envelope {
                message_id: "m-commit-early".into(),
                sender: FRAUD.into(),
                ..envelope
            },
        ),
    );
    let err = replay_under_policy(
        &start(),
        events,
        &mode(),
        &ReplayOptions::default(),
        &veto_policy(),
    )
    .unwrap_err();
    assert_eq!(err.error_code(), "FORBIDDEN");
}

#[test]
fn the_report_serializes_for_tools() {
    let report = replay_under_policy(
        &start(),
        events(),
        &mode(),
        &ReplayOptions::default(),
        &veto_policy(),
    )
    .unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["first_divergence"],
        serde_json::json!({"at": "event", "index": 4})
    );
    assert_eq!(json["candidate"]["state"], "Open");
    assert_eq!(json["entries"][5]["candidate"]["outcome"], "refused");
    assert_eq!(json["entries"][5]["candidate"]["code"], "POLICY_DENIED");
    assert!(json.get("candidate_session").is_none());
}
