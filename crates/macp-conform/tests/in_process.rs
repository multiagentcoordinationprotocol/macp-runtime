//! The in-process target against the runtime's vendored copy of the spec
//! fixtures (`tests/conformance/`, byte-identical to the spec's
//! `schemas/conformance/` at the pinned SPEC_REV, which CI checks).

use macp_conform::report::{GroupStatus, Outcome};
use macp_conform::{run, FixtureSet, InProcessTarget, Level, RunOptions};
use std::path::PathBuf;

fn vendored() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance")
}

#[tokio::test]
async fn in_process_target_passes_every_vendored_fixture() {
    let set = FixtureSet::from_dir(vendored()).expect("load vendored fixtures");
    assert!(
        set.fixtures.len() >= 40,
        "only {} fixtures",
        set.fixtures.len()
    );
    assert_eq!(set.vectors.len(), 10);
    let mut target = InProcessTarget::new();
    let opts = RunOptions::default()
        .with_level(Level::Standard)
        .with_multi_round(true)
        .with_strict(true);
    let report = run(&mut target, &set, &opts).await;
    let bad: Vec<_> = report
        .fixtures
        .iter()
        .filter(|r| r.outcome != Outcome::Pass)
        .map(|r| format!("{} {:?} {:?} {:?}", r.name, r.outcome, r.reason, r.failure))
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    assert!(report.fixtures.iter().all(|r| r.unverified.is_empty()));
    assert!(report.levels.iter().all(|g| g.status == GroupStatus::Pass));
    assert_eq!(report.multi_round.status, GroupStatus::Pass);
    assert_eq!(report.achieved_level, Some(Level::Standard));
    assert!(!report.is_failure());
    assert_eq!(
        report.claim.as_deref(),
        Some("Standard modes + multi_round, Embedded profile, semantics_rev 4")
    );
}

#[tokio::test]
async fn a_lower_level_runs_only_its_fixtures() {
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let mut target = InProcessTarget::new();
    let report = run(
        &mut target,
        &set,
        &RunOptions::default().with_level(Level::Core),
    )
    .await;
    assert!(report
        .fixtures
        .iter()
        .all(|r| r.name.starts_with("session_start_") || r.name.starts_with("cmt-hash/")));
    assert_eq!(report.levels[0].status, GroupStatus::Pass);
    assert_eq!(report.levels[1].status, GroupStatus::NotRun);
    assert_eq!(report.multi_round.status, GroupStatus::NotRun);
    assert_eq!(report.achieved_level, Some(Level::Core));
}

#[test]
fn report_schema_is_json() {
    let v: serde_json::Value = serde_json::from_str(macp_conform::REPORT_SCHEMA).unwrap();
    assert_eq!(v["properties"]["report_format"]["const"], 1);
}

/// The runner must fail a fixture whose expectations the target does not
/// meet: each mutation below breaks one assertion of a passing fixture.
#[tokio::test]
async fn mutated_expectations_fail() {
    use macp_conform::runner::run_fixture;
    let set = FixtureSet::from_dir(vendored()).unwrap();
    let base = set.fixture("decision_happy_path.json").unwrap().clone();
    let mut target = InProcessTarget::new();
    assert_eq!(run_fixture(&mut target, &base).await.outcome, Outcome::Pass);

    type Mutation = fn(&mut macp_conform::fixture::Fixture);
    let cases: [(&str, Mutation); 5] = [
        ("final_state", |f| f.expected_final_state = "Expired".into()),
        ("reject", |f| f.messages[0].expect = "reject".into()),
        ("resolution_present", |f| {
            f.expect_resolution_present = Some(false)
        }),
        ("resolution", |f| {
            f.expected_resolution = Some(serde_json::json!({"action": "no.such.action"}))
        }),
        ("history", |f| f.expected_history = Some(vec![])),
    ];
    for (assertion, mutate) in cases {
        let mut nf = base.clone();
        mutate(&mut nf.fixture);
        let r = run_fixture(&mut target, &nf).await;
        assert_eq!(r.outcome, Outcome::Fail, "{assertion}: {r:?}");
        assert_eq!(r.failure.unwrap().assertion, assertion);
    }

    let mut wrong_code = set
        .fixture("session_start_reject_paths.json")
        .unwrap()
        .clone();
    let step = wrong_code
        .fixture
        .messages
        .iter()
        .position(|m| m.expect == "reject" && m.expected_error_code.is_some());
    if let Some(i) = step {
        wrong_code.fixture.messages[i].expected_error_code = Some("NOT_A_CODE".into());
        let r = run_fixture(&mut target, &wrong_code).await;
        assert_eq!(r.failure.unwrap().assertion, "error_code");
    }
}
