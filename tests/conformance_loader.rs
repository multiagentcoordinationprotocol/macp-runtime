//! The conformance fixtures, run through the `macp-conform` library against
//! two targets per fixture:
//!
//! - [`RuntimeTarget`]: this crate's full `Runtime` kernel (in-memory storage,
//!   the server's own `process` path, its `LogStore` history and its
//!   `replay_session`) — the Server profile minus the network;
//! - [`InProcessTarget`]: `macp-conform`'s Embedded target over
//!   `macp_modes::step` and `macp_modes::replay` alone.
//!
//! Each fixture must pass on both, with the same outcome — so the library the
//! `macp-conform` CLI ships is held to exactly the verdicts this runtime's own
//! suite has always enforced, and the two acceptance paths cannot drift.

use async_trait::async_trait;
use macp_conform::fixture::NamedFixture;
use macp_conform::runner::run_fixture;
use macp_conform::{
    state_name, Capabilities, ConformanceTarget, HistoryEntry, InProcessTarget, Observation,
    Outcome, Profile, Submission, TargetError,
};
use macp_runtime::log_store::{EntryKind, LogStore};
use macp_runtime::macp_core::{ManualClock, PolicyDefinition};
use macp_runtime::pb::Envelope;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::storage::MemoryBackend;
use std::path::Path;
use std::sync::Arc;

/// The reference runtime as a conformance target. A fresh `Runtime` per
/// fixture; a clocked fixture runs it on a `ManualClock`.
struct RuntimeTarget {
    rt: Runtime,
    clock: Option<Arc<ManualClock>>,
}

fn make_runtime(clock: Option<Arc<ManualClock>>) -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let rt = Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    );
    match clock {
        Some(clock) => rt.with_clock(clock),
        None => rt,
    }
}

impl RuntimeTarget {
    fn new() -> Self {
        Self {
            rt: make_runtime(None),
            clock: None,
        }
    }
}

#[async_trait]
impl ConformanceTarget for RuntimeTarget {
    fn profile(&self) -> Profile {
        Profile::Embedded
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
            .with_clock_control(true)
            .with_replay(true)
    }

    fn describe(&self) -> String {
        "macp-runtime kernel (in-memory)".into()
    }

    async fn begin_fixture(&mut self, clock_start: Option<i64>) -> Result<(), TargetError> {
        self.clock = clock_start.map(|at| Arc::new(ManualClock::new(at)));
        self.rt = make_runtime(self.clock.clone());
        Ok(())
    }

    async fn register_policy(&mut self, definition: PolicyDefinition) -> Result<(), TargetError> {
        self.rt.register_policy(definition).map_err(TargetError)
    }

    async fn set_clock(&mut self, now_ms: i64) -> Result<(), TargetError> {
        let clock = self
            .clock
            .as_ref()
            .ok_or_else(|| TargetError("set_clock on an unclocked fixture".into()))?;
        clock.set(now_ms);
        Ok(())
    }

    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError> {
        Ok(match self.rt.process(&envelope, None).await {
            Ok(r) => Submission::Accepted {
                duplicate: r.duplicate,
            },
            Err(e) => Submission::Rejected {
                code: e.error_code().to_string(),
            },
        })
    }

    async fn observe(&mut self, sid: &str, _initiator: &str) -> Result<Observation, TargetError> {
        let session = self
            .rt
            .get_session_checked(sid)
            .await
            .ok_or_else(|| TargetError(format!("no session {sid}")))?;
        let log = self.rt.log_store.get_log(sid).await.unwrap_or_default();
        let history = log
            .iter()
            .filter(|e| e.entry_kind == EntryKind::Incoming)
            .map(|e| {
                HistoryEntry::new(
                    e.message_type.clone(),
                    e.sender.clone(),
                    e.message_id.clone(),
                    e.raw_payload.clone(),
                )
            })
            .collect();
        pin_resolution_and_receipt(&self.rt, &session, &log).await;
        let mode_state = if session.mode_state.is_empty() {
            None
        } else {
            serde_json::from_slice(&session.mode_state).ok()
        };
        Ok(Observation::new(state_name(&session.state))
            .with_resolution(session.resolution.clone())
            .with_mode_state(mode_state)
            .with_history(history))
    }

    async fn check_replay(&mut self, sid: &str) -> Result<Result<(), String>, TargetError> {
        let live = self
            .rt
            .get_session_checked(sid)
            .await
            .ok_or_else(|| TargetError(format!("no session {sid}")))?;
        let log = self.rt.log_store.get_log(sid).await.unwrap_or_default();
        // Pass the runtime's policy registry so replay re-resolves any bound
        // policy and re-evaluates commitments through it: policy-bound
        // replay, not just log-trusting replay.
        let replayed = match replay_session(
            sid,
            &log,
            self.rt.mode_registry().as_ref(),
            Some(self.rt.policy_registry().as_ref()),
        ) {
            Ok(s) => s,
            Err(e) => return Ok(Err(format!("replay failed: {e}"))),
        };
        if replayed.state != live.state {
            return Ok(Err(format!(
                "state: replayed {:?}, live {:?}",
                replayed.state, live.state
            )));
        }
        if replayed.resolution != live.resolution {
            return Ok(Err("resolution differs".into()));
        }
        if replayed.mode_state != live.mode_state {
            return Ok(Err("mode_state differs".into()));
        }
        if replayed.seen_message_ids != live.seen_message_ids {
            return Ok(Err("dedup state (seen_message_ids) differs".into()));
        }
        Ok(Ok(()))
    }
}

/// Seam S-d: a resolved standards-track session's `resolution` is its
/// accepted Commitment's payload bytes, verbatim, and its decision receipt
/// (RFC-MACP-0013 §13) issues with integrity `match`. A resolved extension
/// session gets no receipt.
async fn pin_resolution_and_receipt(
    rt: &Runtime,
    session: &macp_runtime::session::Session,
    log: &[macp_runtime::log_store::LogEntry],
) {
    use macp_runtime::macp_core::receipt::{verify, Integrity, TrustedKeys};
    use macp_runtime::session::SessionState;
    if session.state != SessionState::Resolved {
        return;
    }
    let sid = session.session_id.as_str();
    let receipt = rt.decision_receipt(sid).await;
    if !macp_runtime::receipts::RECEIPT_MODES.contains(&session.mode.as_str()) {
        assert!(
            matches!(
                receipt,
                Err(macp_runtime::error::MacpError::ReceiptUnavailable)
            ),
            "{sid}: an extension-mode session must get no receipt"
        );
        return;
    }
    let commitment = log
        .iter()
        .rev()
        .find(|e| e.entry_kind == EntryKind::Incoming && e.message_type == "Commitment")
        .unwrap_or_else(|| panic!("{sid}: resolved without an accepted Commitment"));
    assert_eq!(
        session.resolution.as_deref(),
        Some(&commitment.raw_payload[..]),
        "{sid}: resolution must be the Commitment payload bytes, verbatim"
    );
    let receipt = receipt.unwrap_or_else(|e| panic!("{sid}: no receipt: {e}"));
    let receipt: serde_json::Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(
        verify(&receipt, &TrustedKeys::new()).integrity,
        Some(Integrity::Match),
        "{sid}: receipt integrity"
    );
}

async fn run_conformance_fixture(path: &Path) {
    let fixture = NamedFixture::from_file(path).unwrap_or_else(|e| panic!("{e}"));
    let on_runtime = run_fixture(&mut RuntimeTarget::new(), &fixture).await;
    let in_process = run_fixture(&mut InProcessTarget::new(), &fixture).await;
    for (who, r) in [("runtime", &on_runtime), ("in-process", &in_process)] {
        assert_eq!(
            r.outcome,
            Outcome::Pass,
            "{} on the {who} target: {:?} {:?}",
            path.display(),
            r.reason,
            r.failure
        );
        assert!(
            r.unverified.is_empty(),
            "{} on the {who} target left {:?} unverified",
            path.display(),
            r.unverified
        );
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

// Spec W2.7 policy packs (examples/policies/ in the spec repo, docs/policy-packs.md).
// Each fixture inlines one ready-made PolicyDescriptor verbatim and exercises the
// one rule it relies on: designated_role commitment authority, a weighted
// electorate of one, and a quorum n_of_m override of required_approvals.
conformance_test!(
    conformance_decision_policy_pack_human_signoff,
    "decision_policy_pack_human_signoff.json"
);
conformance_test!(
    conformance_decision_policy_pack_compliance_vote,
    "decision_policy_pack_compliance_vote.json"
);
conformance_test!(
    conformance_quorum_policy_pack_two_agent_rule,
    "quorum_policy_pack_two_agent_rule.json"
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
    // SessionStart obligations), and to 40 by the W2.7 policy-pack fixtures
    // (spec #209).
    assert!(
        checked >= 40,
        "expected all fixtures checked, got {checked}"
    );
}
