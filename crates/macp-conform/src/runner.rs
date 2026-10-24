//! Running fixtures against a [`ConformanceTarget`].
//!
//! The harness rules are the spec's (`schemas/conformance/README.md`):
//!
//! - each fixture runs in its own session, with a fresh UUID v4 `session_id`;
//!   the harness opens it with a `SessionStart` (`message_id` `m0`, intent
//!   `conformance`) from the fixture's initiator, built from the fixture's
//!   own fields, after registering its inline `policy` if it has one;
//! - `messages[i]` is sent as `message_id` `m{i+1}`, with `macp_version`
//!   `1.0`, the fixture's `mode` and session, unless its `envelope` block
//!   overrides them;
//! - a clocked fixture (`accepted_at_ms` on every message) opens the session
//!   at acceptance time 0 with Envelope timestamp 0 and sets the acceptance
//!   clock before each message; an unclocked one uses wall-clock timestamps;
//! - after the last message the final state, resolution, mode state,
//!   history and replay equivalence are checked, in that order.
//!
//! A fixture stops at its first failing assertion.

use crate::encode::{commitment_to_json, encode_payload, history_payload_to_json, json_contains};
use crate::fixture::{EnvelopeOverride, FixtureSet, NamedFixture, NamedVector};
use crate::level::{group_of_fixture, group_of_vector, Group, Level};
use crate::report::{
    Failure, FixtureKind, FixtureResult, GroupStatus, GroupSummary, Outcome, Report, Requested,
    RunnerInfo, TargetInfo, Totals, REPORT_FORMAT,
};
use crate::target::{ConformanceTarget, Observable, Submission};
use macp_core::PolicyDefinition;
use macp_pb::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use prost::Message;
use std::collections::{HashMap, HashSet};

/// Skip reason for a clocked fixture on a target without clock control.
pub const NEEDS_CLOCK_CONTROL: &str = "needs-clock-control";
/// Skip reason for a commitment-hash vector on a target with no hash API.
pub const NEEDS_HASH_API: &str = "needs-hash-api";

/// What to run.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunOptions {
    /// Run this level and every level below it.
    pub level: Level,
    /// Also run the optional multi_round fixtures.
    pub multi_round: bool,
    /// Count `incomplete` as failure in [`Report::is_failure`].
    pub strict: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            level: Level::Standard,
            multi_round: false,
            strict: false,
        }
    }
}

impl RunOptions {
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }
    pub fn with_multi_round(mut self, on: bool) -> Self {
        self.multi_round = on;
        self
    }
    pub fn with_strict(mut self, on: bool) -> Self {
        self.strict = on;
        self
    }

    fn selects(&self, group: Group) -> bool {
        match group {
            Group::Level(l) => l <= self.level,
            Group::MultiRound => self.multi_round,
            Group::Unclassified => false,
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Run the selected fixtures and vectors of `set` against `target`.
pub async fn run(
    target: &mut dyn ConformanceTarget,
    set: &FixtureSet,
    opts: &RunOptions,
) -> Report {
    let mut results = Vec::new();
    for f in &set.fixtures {
        if opts.selects(group_of_fixture(&f.name)) {
            results.push(run_fixture(target, f).await);
        }
    }
    let hashes: HashMap<String, String> = set
        .vectors
        .iter()
        .filter_map(|v| Some((v.vector.name.clone(), v.vector.hash.clone()?)))
        .collect();
    for v in &set.vectors {
        if opts.selects(group_of_vector(&v.name)) {
            results.push(run_vector(&*target, v, &hashes));
        }
    }
    build_report(target, set, opts, results)
}

fn summarize(group: Group, selected: bool, results: &[FixtureResult]) -> GroupSummary {
    let mine: Vec<_> = results.iter().filter(|r| r.group == group).collect();
    let count = |o: Outcome| mine.iter().filter(|r| r.outcome == o).count();
    let (passed, failed, skipped, errors) = (
        count(Outcome::Pass),
        count(Outcome::Fail),
        count(Outcome::Skipped),
        count(Outcome::Error),
    );
    let status = if !selected {
        GroupStatus::NotRun
    } else if failed + errors > 0 {
        GroupStatus::Fail
    } else if skipped > 0 || passed == 0 {
        GroupStatus::Incomplete
    } else {
        GroupStatus::Pass
    };
    GroupSummary {
        group,
        status,
        passed,
        failed,
        skipped,
        errors,
        unverified: mine
            .iter()
            .filter(|r| r.outcome == Outcome::Pass)
            .map(|r| r.unverified.len())
            .sum(),
    }
}

fn build_report(
    target: &dyn ConformanceTarget,
    set: &FixtureSet,
    opts: &RunOptions,
    results: Vec<FixtureResult>,
) -> Report {
    let levels: Vec<GroupSummary> = Level::ALL
        .iter()
        .map(|&l| summarize(Group::Level(l), opts.selects(Group::Level(l)), &results))
        .collect();
    let multi_round = summarize(Group::MultiRound, opts.multi_round, &results);
    let mut achieved_level = None;
    for (l, g) in Level::ALL.iter().zip(&levels) {
        if g.status == GroupStatus::Pass {
            achieved_level = Some(*l);
        } else {
            break;
        }
    }
    let profile = target.profile();
    let claim = achieved_level.map(|l| {
        let mut c = l.claim_name().to_string();
        if multi_round.status == GroupStatus::Pass {
            c.push_str(" + multi_round");
        }
        let p = match profile {
            crate::target::Profile::Server => "Server",
            crate::target::Profile::Embedded => "Embedded",
        };
        c.push_str(&format!(", {p} profile"));
        if let Some(rev) = target.semantics_rev() {
            c.push_str(&format!(", semantics_rev {rev}"));
        }
        c
    });
    let mut totals = Totals::default();
    for r in &results {
        match r.outcome {
            Outcome::Pass => {
                totals.passed += 1;
                totals.unverified += r.unverified.len();
            }
            Outcome::Fail => totals.failed += 1,
            Outcome::Skipped => totals.skipped += 1,
            Outcome::Error => totals.errors += 1,
        }
    }
    Report {
        report_format: REPORT_FORMAT,
        runner: RunnerInfo {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        target: TargetInfo {
            description: target.describe(),
            profile,
            semantics_rev: target.semantics_rev(),
            capabilities: target.capabilities(),
        },
        fixtures_source: set.source.clone(),
        requested: Requested {
            level: opts.level,
            multi_round: opts.multi_round,
            strict: opts.strict,
        },
        levels,
        multi_round,
        achieved_level,
        claim,
        totals,
        fixtures: results,
    }
}

/// How a fixture run ended early.
enum Stop {
    Fail(Failure),
    Error(String),
}

fn fail(step: Option<usize>, assertion: &str, message: impl Into<String>) -> Stop {
    Stop::Fail(Failure {
        step,
        assertion: assertion.into(),
        message: message.into(),
    })
}

/// Run one session fixture.
pub async fn run_fixture(target: &mut dyn ConformanceTarget, f: &NamedFixture) -> FixtureResult {
    let mut result = FixtureResult {
        name: f.name.clone(),
        kind: FixtureKind::Session,
        group: group_of_fixture(&f.name),
        outcome: Outcome::Pass,
        reason: None,
        failure: None,
        unverified: Vec::new(),
    };
    if let Err(e) = f.fixture.check_clock_shape() {
        result.outcome = Outcome::Error;
        result.reason = Some(format!("malformed fixture: {e}"));
        return result;
    }
    if f.fixture.is_clocked() && !target.capabilities().clock_control {
        result.outcome = Outcome::Skipped;
        result.reason = Some(NEEDS_CLOCK_CONTROL.into());
        return result;
    }
    match drive(target, f, &mut result.unverified).await {
        Ok(()) => {}
        Err(Stop::Fail(failure)) => {
            result.outcome = Outcome::Fail;
            result.failure = Some(failure);
            result.unverified.clear();
        }
        Err(Stop::Error(e)) => {
            result.outcome = Outcome::Error;
            result.reason = Some(e);
            result.unverified.clear();
        }
    }
    result
}

async fn drive(
    target: &mut dyn ConformanceTarget,
    nf: &NamedFixture,
    unverified: &mut Vec<String>,
) -> Result<(), Stop> {
    let fx = &nf.fixture;
    let clocked = fx.is_clocked();
    let terr = |e: crate::target::TargetError| Stop::Error(format!("target error: {e}"));
    target
        .begin_fixture(clocked.then_some(0))
        .await
        .map_err(terr)?;
    let sid = uuid::Uuid::new_v4().as_hyphenated().to_string();
    let stamp = || if clocked { 0 } else { now_ms() };

    if let Some(policy) = &fx.policy {
        let def: PolicyDefinition = serde_json::from_value(policy.clone())
            .map_err(|e| Stop::Error(format!("malformed fixture: bad inline policy: {e}")))?;
        target.register_policy(def).await.map_err(terr)?;
    }

    let start = SessionStartPayload {
        intent: "conformance".into(),
        participants: fx.participants.clone(),
        mode_version: fx.mode_version.clone(),
        configuration_version: fx.configuration_version.clone(),
        policy_version: fx.policy_version.clone(),
        ttl_ms: fx.ttl_ms,
        context_id: String::new(),
        extensions: HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    };
    let env = Envelope {
        macp_version: "1.0".into(),
        mode: fx.mode.clone(),
        message_type: "SessionStart".into(),
        message_id: "m0".into(),
        session_id: sid.clone(),
        sender: fx.initiator.clone(),
        timestamp_unix_ms: stamp(),
        payload: start.encode_to_vec(),
    };
    match target.submit(env).await.map_err(terr)? {
        Submission::Accepted { .. } => {}
        Submission::Rejected { code } => {
            return Err(fail(
                Some(0),
                "session_start",
                format!("the fixture's SessionStart was rejected with {code}"),
            ))
        }
    }

    for (i, msg) in fx.messages.iter().enumerate() {
        let step = i + 1;
        let payload = encode_payload(&msg.payload_type, &msg.message_type, &msg.payload)
            .map_err(|e| Stop::Error(format!("malformed fixture: message {step}: {e}")))?;
        if let (true, Some(at)) = (clocked, msg.accepted_at_ms) {
            target.set_clock(at).await.map_err(terr)?;
        }
        let no_override = EnvelopeOverride::default();
        let ov = msg.envelope.as_ref().unwrap_or(&no_override);
        let env = Envelope {
            macp_version: ov.macp_version.clone().unwrap_or_else(|| "1.0".into()),
            mode: ov.mode.clone().unwrap_or_else(|| fx.mode.clone()),
            message_type: msg.message_type.clone(),
            message_id: format!("m{step}"),
            session_id: ov.session_id.clone().unwrap_or_else(|| sid.clone()),
            sender: msg.sender.clone(),
            timestamp_unix_ms: ov.timestamp_unix_ms.unwrap_or_else(stamp),
            payload,
        };
        let got = target.submit(env).await.map_err(terr)?;
        match (msg.expect.as_str(), got) {
            ("accept", Submission::Accepted { .. }) => {}
            ("accept", Submission::Rejected { code }) => {
                return Err(fail(
                    Some(step),
                    "accept",
                    format!("{} expected accept, rejected with {code}", msg.message_type),
                ))
            }
            ("reject", Submission::Accepted { .. }) => {
                return Err(fail(
                    Some(step),
                    "reject",
                    format!("{} expected reject, was accepted", msg.message_type),
                ))
            }
            ("reject", Submission::Rejected { code }) => {
                if let Some(expected) = &msg.expected_error_code {
                    if &code != expected {
                        return Err(fail(
                            Some(step),
                            "error_code",
                            format!("{} expected {expected}, got {code}", msg.message_type),
                        ));
                    }
                }
            }
            (other, _) => {
                return Err(Stop::Error(format!(
                    "malformed fixture: message {step}: unknown expect {other:?}"
                )))
            }
        }
    }

    let obs = target.observe(&sid, &fx.initiator).await.map_err(terr)?;
    if obs.state != fx.expected_final_state {
        return Err(fail(
            None,
            "final_state",
            format!("expected {}, got {}", fx.expected_final_state, obs.state),
        ));
    }

    if let Some(present) = fx.expect_resolution_present {
        match &obs.resolution {
            Observable::Observed(r) if r.is_some() != present => {
                return Err(fail(
                    None,
                    "resolution_present",
                    format!("expected present={present}, got present={}", r.is_some()),
                ))
            }
            Observable::Observed(_) => {}
            Observable::Unobservable => unverified.push("resolution_present".into()),
        }
    }

    if let Some(expected) = &fx.expected_resolution {
        match &obs.resolution {
            Observable::Observed(r) => {
                let actual = r.as_deref().and_then(commitment_to_json).ok_or_else(|| {
                    fail(None, "resolution", "resolution missing or not decodable")
                })?;
                json_contains(&actual, expected).map_err(|m| fail(None, "resolution", m))?;
            }
            Observable::Unobservable => unverified.push("resolution".into()),
        }
    }

    if let Some(expected) = &fx.expected_mode_state {
        match &obs.mode_state {
            Observable::Observed(m) => {
                let actual = m
                    .as_ref()
                    .ok_or_else(|| fail(None, "mode_state", "mode state missing or not JSON"))?;
                json_contains(actual, expected).map_err(|m| fail(None, "mode_state", m))?;
            }
            Observable::Unobservable => unverified.push("mode_state".into()),
        }
    }

    if let Some(expected) = &fx.expected_history {
        match &obs.history {
            Observable::Observed(history) => {
                let submitted: HashSet<String> =
                    (1..=fx.messages.len()).map(|i| format!("m{i}")).collect();
                let accepted: Vec<_> = history.iter().filter(|e| e.message_id != "m0").collect();
                if accepted.len() != expected.len() {
                    let shape: Vec<String> = accepted
                        .iter()
                        .map(|e| format!("{} from {}", e.message_type, e.sender))
                        .collect();
                    return Err(fail(
                        None,
                        "history",
                        format!(
                            "expected {} entries, got {}: {shape:?}",
                            expected.len(),
                            accepted.len()
                        ),
                    ));
                }
                for (idx, (a, e)) in accepted.iter().zip(expected).enumerate() {
                    if a.message_type != e.message_type || a.sender != e.sender {
                        return Err(fail(
                            None,
                            "history",
                            format!(
                                "entry {idx}: expected {} from {}, got {} from {}",
                                e.message_type, e.sender, a.message_type, a.sender
                            ),
                        ));
                    }
                    let synthetic = !submitted.contains(&a.message_id);
                    if synthetic != e.synthetic {
                        return Err(fail(
                            None,
                            "history",
                            format!(
                                "entry {idx}: synthetic expected {}, got {synthetic} (message_id {})",
                                e.synthetic, a.message_id
                            ),
                        ));
                    }
                    if let Some(ep) = &e.payload {
                        let ap = history_payload_to_json(&a.message_type, &a.payload)
                            .map_err(|m| fail(None, "history", format!("entry {idx}: {m}")))?;
                        json_contains(&ap, ep)
                            .map_err(|m| fail(None, "history", format!("entry {idx}: {m}")))?;
                    }
                }
            }
            Observable::Unobservable => unverified.push("history".into()),
        }
    }

    if fx.verify_replay_equivalence {
        if target.capabilities().replay {
            target
                .check_replay(&sid)
                .await
                .map_err(terr)?
                .map_err(|m| fail(None, "replay", m))?;
        } else {
            unverified.push("replay".into());
        }
    }
    Ok(())
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Run one commitment-hash vector. `hashes` maps every compute vector's
/// `name` to its pinned hash, for `must_differ_from`.
pub fn run_vector(
    target: &dyn ConformanceTarget,
    nv: &NamedVector,
    hashes: &HashMap<String, String>,
) -> FixtureResult {
    let mut result = FixtureResult {
        name: format!("cmt-hash/{}", nv.name),
        kind: FixtureKind::CmtHash,
        group: group_of_vector(&nv.name),
        outcome: Outcome::Pass,
        reason: None,
        failure: None,
        unverified: Vec::new(),
    };
    let v = &nv.vector;
    let failed = |result: &mut FixtureResult, assertion: &str, message: String| {
        result.outcome = Outcome::Fail;
        result.failure = Some(Failure {
            step: None,
            assertion: assertion.into(),
            message,
        });
    };
    if !target.capabilities().commitment_hash {
        result.outcome = Outcome::Skipped;
        result.reason = Some(NEEDS_HASH_API.into());
        return result;
    }
    match (
        &v.hash,
        &v.payload,
        &v.wire_hex,
        &v.claimed_hash,
        &v.expected_outcome,
    ) {
        (Some(hash), Some(payload), None, None, None) => {
            let payload: CommitmentPayload = payload.into();
            let computed = target.commitment_hash(&payload).unwrap_or_default();
            if &computed != hash {
                failed(
                    &mut result,
                    "hash",
                    format!("computed {computed}, expected {hash}"),
                );
                return result;
            }
            let own = target
                .verify_commitment_hash(&payload.encode_to_vec(), hash)
                .unwrap_or_default();
            if own != "match" {
                failed(
                    &mut result,
                    "verify",
                    format!("its own hash must verify as match, got {own}"),
                );
                return result;
            }
            if let Some(other) = &v.must_differ_from {
                match hashes.get(other) {
                    Some(h) if h == hash => {
                        failed(
                            &mut result,
                            "must_differ_from",
                            format!("hash equals that of {other}"),
                        );
                    }
                    Some(_) => {}
                    None => {
                        result.outcome = Outcome::Error;
                        result.reason = Some(format!(
                            "malformed vector: must_differ_from {other} not found"
                        ));
                    }
                }
            }
        }
        (None, None, Some(wire), Some(claimed), Some(expected)) => {
            let Some(wire) = decode_hex(wire) else {
                result.outcome = Outcome::Error;
                result.reason = Some("malformed vector: wire_hex is not hex".into());
                return result;
            };
            let got = target
                .verify_commitment_hash(&wire, claimed)
                .unwrap_or_default();
            if &got != expected {
                failed(
                    &mut result,
                    "verify",
                    format!("expected {expected}, got {got}"),
                );
            }
        }
        _ => {
            result.outcome = Outcome::Error;
            result.reason = Some("malformed vector: neither a compute nor a verify vector".into());
        }
    }
    result
}
