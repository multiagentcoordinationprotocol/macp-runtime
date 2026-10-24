//! The machine-readable run report.
//!
//! Its JSON shape is defined by `schema/macp-conform-report.schema.json`
//! (shipped in this crate, and exported as [`REPORT_SCHEMA`]).
//! `report_format` is bumped on any incompatible change; fields may be added
//! without a bump.

use crate::fixture::FixtureSource;
use crate::level::{Group, Level};
use crate::target::{Capabilities, Profile};
use serde::Serialize;

/// The current `report_format`.
pub const REPORT_FORMAT: u32 = 1;

/// The JSON Schema (draft 2020-12) the report validates against.
pub const REPORT_SCHEMA: &str = include_str!("../schema/macp-conform-report.schema.json");

/// One fixture's or vector's verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every assertion the target could observe held.
    Pass,
    /// An assertion failed.
    Fail,
    /// Not run, because the target lacks a capability the fixture needs
    /// (`reason` says which: `needs-clock-control`, `needs-hash-api`).
    Skipped,
    /// The harness or the target broke (malformed fixture, transport error).
    /// Not a conformance verdict.
    Error,
}

/// `session` for a session fixture, `cmt_hash` for a commitment-hash vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureKind {
    Session,
    CmtHash,
}

/// Where a fixture failed.
#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    /// The fixture step: 0 is the harness's `SessionStart`, `n` is
    /// `messages[n-1]`. Absent for final-state assertions.
    pub step: Option<usize>,
    /// Which assertion (`accept`, `reject`, `error_code`, `final_state`,
    /// `resolution_present`, `resolution`, `mode_state`, `history`, `replay`,
    /// `hash`, `verify`, `must_differ_from`, `session_start`).
    pub assertion: String,
    pub message: String,
}

/// One fixture's or vector's result.
#[derive(Debug, Clone, Serialize)]
pub struct FixtureResult {
    pub name: String,
    pub kind: FixtureKind,
    pub group: Group,
    pub outcome: Outcome,
    /// Why it was skipped or errored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
    /// Assertions the fixture makes that the target could not observe. A
    /// passing fixture with unverified assertions passed on everything else.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unverified: Vec<String>,
}

impl FixtureResult {
    pub fn passed(&self) -> bool {
        self.outcome == Outcome::Pass
    }
}

/// `pass`: every fixture in the group passed. `fail`: at least one failed or
/// errored. `incomplete`: none failed, but some were skipped. `not_run`:
/// the group was not selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupStatus {
    Pass,
    Fail,
    Incomplete,
    NotRun,
}

/// Counts and status for one level (its own fixtures only, not the levels
/// it includes) or for the multi_round group.
#[derive(Debug, Clone, Serialize)]
pub struct GroupSummary {
    pub group: Group,
    pub status: GroupStatus,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: usize,
    /// Unverified assertions across the group's passing fixtures.
    pub unverified: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TargetInfo {
    pub description: String,
    pub profile: Profile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantics_rev: Option<u32>,
    pub capabilities: Capabilities,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Requested {
    pub level: Level,
    pub multi_round: bool,
    pub strict: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub unverified: usize,
}

/// A whole run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub report_format: u32,
    pub runner: RunnerInfo,
    pub target: TargetInfo,
    pub fixtures_source: FixtureSource,
    pub requested: Requested,
    /// Core, Decision, Standard, in that order.
    pub levels: Vec<GroupSummary>,
    pub multi_round: GroupSummary,
    /// The highest level whose own group and every group below it are
    /// `pass` (with `strict`, `incomplete` never counts as `pass`; without
    /// it, an `incomplete` group also blocks the level, since some of its
    /// fixtures did not run). Absent when not even Core passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub achieved_level: Option<Level>,
    /// The claim the run supports, e.g. `Standard modes, Server profile`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    pub totals: Totals,
    pub fixtures: Vec<FixtureResult>,
}

impl Report {
    /// Did any selected fixture fail or error? (`incomplete` counts too with
    /// `strict`.) This is the CLI's exit status.
    pub fn is_failure(&self) -> bool {
        let selected = self
            .levels
            .iter()
            .chain(std::iter::once(&self.multi_round))
            .filter(|g| g.status != GroupStatus::NotRun);
        let mut any = false;
        for g in selected {
            any |= g.status == GroupStatus::Fail
                || (self.requested.strict && g.status == GroupStatus::Incomplete);
        }
        any
    }

    /// A short human-readable summary.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "target: {} ({:?} profile)\n",
            self.target.description, self.target.profile
        ));
        for r in &self.fixtures {
            let mark = match r.outcome {
                Outcome::Pass => "PASS",
                Outcome::Fail => "FAIL",
                Outcome::Skipped => "SKIP",
                Outcome::Error => "ERR ",
            };
            out.push_str(&format!("  {mark} {}", r.name));
            if let Some(reason) = &r.reason {
                out.push_str(&format!(" ({reason})"));
            }
            if let Some(f) = &r.failure {
                match f.step {
                    Some(step) => {
                        out.push_str(&format!(" - step {step} {}: {}", f.assertion, f.message))
                    }
                    None => out.push_str(&format!(" - {}: {}", f.assertion, f.message)),
                }
            }
            if !r.unverified.is_empty() {
                out.push_str(&format!(" [unverified: {}]", r.unverified.join(", ")));
            }
            out.push('\n');
        }
        for g in self.levels.iter().chain(std::iter::once(&self.multi_round)) {
            out.push_str(&format!(
                "{:<12} {:?}: {} passed, {} failed, {} skipped, {} errors, {} unverified\n",
                serde_json::to_value(g.group)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                g.status,
                g.passed,
                g.failed,
                g.skipped,
                g.errors,
                g.unverified
            ));
        }
        match &self.claim {
            Some(claim) => out.push_str(&format!("claim: {claim}\n")),
            None => out.push_str("claim: none (no level fully passed)\n"),
        }
        out
    }
}
