//! The spec's two Server-profile rules (`docs/conformance.md`, Profiles),
//! against a target that behaves like a gRPC server: the in-process kernel
//! with no clock control, no replay, no hash API and no visible mode state.
//!
//! 1. A Server claim at Core or above counts the `cmt-hash` vectors when
//!    they pass in-process against the same implementation
//!    ([`run_with_hash_target`]); the claim says `(hash vectors in-process)`.
//! 2. A clock-dependent fixture is skipped (`needs-clock-control`) and does
//!    not count against a level, on a Server target only.

use async_trait::async_trait;
use macp_conform::report::{GroupStatus, Outcome};
use macp_conform::runner::{NEEDS_CLOCK_CONTROL, NEEDS_HASH_API};
use macp_conform::{
    run, run_with_hash_target, Capabilities, ConformanceTarget, FixtureSet, InProcessTarget, Level,
    Observable, Observation, Profile, RunOptions, Submission, TargetError,
};
use macp_core::PolicyDefinition;
use macp_pb::pb::Envelope;
use std::path::PathBuf;

fn vendored() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

/// The in-process kernel seen through a wire: only submit and observe.
struct WireLike {
    inner: InProcessTarget,
    profile: Profile,
}

impl WireLike {
    fn new(profile: Profile) -> Self {
        Self {
            inner: InProcessTarget::new(),
            profile,
        }
    }
}

#[async_trait]
impl ConformanceTarget for WireLike {
    fn profile(&self) -> Profile {
        self.profile
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    fn describe(&self) -> String {
        "wire-like".into()
    }
    async fn begin_fixture(&mut self, clock_start: Option<i64>) -> Result<(), TargetError> {
        self.inner.begin_fixture(clock_start).await
    }
    async fn register_policy(&mut self, definition: PolicyDefinition) -> Result<(), TargetError> {
        self.inner.register_policy(definition).await
    }
    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError> {
        self.inner.submit(envelope).await
    }
    async fn observe(
        &mut self,
        session_id: &str,
        initiator: &str,
    ) -> Result<Observation, TargetError> {
        let mut obs = self.inner.observe(session_id, initiator).await?;
        obs.mode_state = Observable::Unobservable;
        Ok(obs)
    }
}

fn opts() -> RunOptions {
    RunOptions::default()
        .with_level(Level::Standard)
        .with_multi_round(true)
        .with_strict(true)
}

#[tokio::test]
async fn server_with_in_process_hashes_reaches_standard() {
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let mut server = WireLike::new(Profile::Server);
    let hashes = InProcessTarget::new();
    let report = run_with_hash_target(&mut server, &hashes, &set, &opts()).await;

    let skipped: Vec<_> = report
        .fixtures
        .iter()
        .filter(|r| r.outcome == Outcome::Skipped)
        .collect();
    assert!(
        !skipped.is_empty(),
        "the vendored set has a clocked fixture"
    );
    assert!(skipped
        .iter()
        .all(|r| r.reason.as_deref() == Some(NEEDS_CLOCK_CONTROL)));
    assert!(report
        .fixtures
        .iter()
        .filter(|r| r.name.starts_with("cmt-hash/"))
        .all(|r| r.outcome == Outcome::Pass));
    assert_eq!(report.totals.excused, skipped.len());
    assert_eq!(report.totals.skipped, skipped.len());

    assert!(report.levels.iter().all(|g| g.status == GroupStatus::Pass));
    assert_eq!(report.achieved_level, Some(Level::Standard));
    // --strict: excused skips are not an incomplete level.
    assert!(!report.is_failure(), "{}", report.render_text());
    assert_eq!(
        report.claim.as_deref(),
        Some("Standard modes + multi_round, Server profile (hash vectors in-process)")
    );
    assert_eq!(
        report.hash_vectors_target.as_deref(),
        Some(hashes.describe().as_str())
    );
}

#[tokio::test]
async fn server_without_a_hash_target_stays_below_core() {
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let report = run(&mut WireLike::new(Profile::Server), &set, &opts()).await;
    let core = &report.levels[0];
    assert_eq!(core.status, GroupStatus::Incomplete);
    assert_eq!(core.skipped, 10);
    assert_eq!(core.excused, 0);
    assert!(report
        .fixtures
        .iter()
        .filter(|r| r.name.starts_with("cmt-hash/"))
        .all(|r| r.reason.as_deref() == Some(NEEDS_HASH_API)));
    assert_eq!(report.achieved_level, None);
    assert_eq!(report.claim, None);
    assert_eq!(report.hash_vectors_target, None);
    // The clock skip is still excused where it sits: Standard passes on
    // its own, it just cannot be claimed without Core below it.
    assert_eq!(report.levels[2].status, GroupStatus::Pass);
    assert!(report.render_text().contains("--hash-vectors in-process"));
}

/// The clock rule is a Server-profile rule: an Embedded run is expected to
/// drive clocked fixtures with a controlled clock, so a skip there blocks.
#[tokio::test]
async fn embedded_without_clock_control_is_incomplete() {
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let mut target = WireLike::new(Profile::Embedded);
    let report = run_with_hash_target(&mut target, &InProcessTarget::new(), &set, &opts()).await;
    assert_eq!(report.totals.excused, 0);
    assert!(report
        .levels
        .iter()
        .any(|g| g.status == GroupStatus::Incomplete));
    assert_ne!(report.achieved_level, Some(Level::Standard));
    assert!(report.is_failure(), "--strict fails an incomplete level");
}

/// Every key a report serializes is declared in the shipped schema.
#[tokio::test]
async fn report_keys_are_in_the_schema() {
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let report = run_with_hash_target(
        &mut WireLike::new(Profile::Server),
        &InProcessTarget::new(),
        &set,
        &opts(),
    )
    .await;
    let schema: serde_json::Value = serde_json::from_str(macp_conform::REPORT_SCHEMA).unwrap();
    let json = serde_json::to_value(&report).unwrap();
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    let declared = |s: &serde_json::Value, k: &str| s["properties"].get(k).is_some();
    for k in keys(&json) {
        assert!(declared(&schema, &k), "report.{k}");
    }
    for k in keys(&json["totals"]) {
        assert!(declared(&schema["properties"]["totals"], &k), "totals.{k}");
    }
    for k in keys(&json["multi_round"]) {
        assert!(
            declared(&schema["$defs"]["group_summary"], &k),
            "group_summary.{k}"
        );
    }
}
