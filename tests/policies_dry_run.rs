//! `MACP_POLICIES_DRY_RUN=1` — the validate-only pass over `MACP_POLICIES_DIR`.
//!
//! `load_from_dir` is fatal at startup and stops at the first rejection, so an
//! operator upgrading into new registration constraints has no way to find out
//! what a policies directory would do until the runtime refuses to boot. These
//! tests drive the real binary: the exit code and the per-file report are the
//! contract, and the process must never reach the server.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("macp-dry-run-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_policy(dir: &Path, file: &str, policy_id: &str, mode: &str, rules: serde_json::Value) {
    let document = serde_json::json!({
        "policy_id": policy_id,
        "mode": mode,
        "description": "dry-run fixture",
        "rules": rules,
        "schema_version": 1,
    });
    std::fs::write(
        dir.join(file),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
}

/// Run the runtime binary in dry-run mode and return `(exit code, stdout+stderr)`.
///
/// The bind address is a scratch port that is never contacted: if dry-run ever
/// stopped short-circuiting, the process would go on to serve and this helper
/// would time out rather than hang the suite.
fn dry_run(dir: &Path) -> (i32, String) {
    run(Some(dir))
}

fn run(dir: Option<&Path>) -> (i32, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macp-runtime"));
    match dir {
        Some(dir) => {
            command.env("MACP_POLICIES_DIR", dir);
        }
        None => {
            command.env_remove("MACP_POLICIES_DIR");
        }
    }
    let mut child = command
        .env("MACP_POLICIES_DRY_RUN", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:50187")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn macp-runtime");

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("try_wait failed") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("dry run did not exit: it must never start the server");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };

    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .ok();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .ok();
    output.push_str(&stderr);
    (status.code().unwrap_or(-1), output)
}

#[test]
fn dry_run_reports_every_rejection_by_filename_and_exits_nonzero() {
    let dir = temp_dir("mixed");
    write_policy(
        &dir,
        "good.json",
        "policy.ops.good",
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "majority", "threshold": 0.5 } }),
    );
    // Out-of-schema `voting.algorithm`.
    write_policy(
        &dir,
        "typo.json",
        "policy.ops.typo",
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "majorty" } }),
    );
    // Out-of-schema quorum `threshold.value`: the schema types it `integer`.
    write_policy(
        &dir,
        "fractional.json",
        "policy.ops.fractional",
        "macp.mode.quorum.v1",
        serde_json::json!({ "threshold": { "type": "n_of_m", "value": 0.5 } }),
    );

    let (code, output) = dry_run(&dir);
    assert_eq!(code, 1, "output: {output}");
    // Every rejection is reported, not just the first one `load_from_dir` hits.
    assert!(output.contains("typo.json"), "output: {output}");
    assert!(output.contains("fractional.json"), "output: {output}");
    assert!(
        output.contains("INVALID_POLICY_DEFINITION"),
        "output: {output}"
    );
    assert!(
        output.contains("3 policy file(s) checked"),
        "output: {output}"
    );
    assert!(output.contains("2 rejected"), "output: {output}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dry_run_exits_zero_for_a_directory_that_would_load() {
    let dir = temp_dir("clean");
    write_policy(
        &dir,
        "good.json",
        "policy.ops.good",
        "macp.mode.decision.v1",
        serde_json::json!({ "voting": { "algorithm": "unanimous" } }),
    );
    write_policy(
        &dir,
        "quorum.json",
        "policy.ops.quorum",
        "macp.mode.quorum.v1",
        // Boundary values the canonical schemas permit.
        serde_json::json!({ "threshold": { "type": "percentage", "value": 100 } }),
    );

    let (code, output) = dry_run(&dir);
    assert_eq!(code, 0, "output: {output}");
    assert!(output.contains("0 rejected"), "output: {output}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dry_run_warns_but_succeeds_when_the_directory_holds_no_policy_files() {
    // A readable directory with nothing in it is the shape of a mis-pointed
    // MACP_POLICIES_DIR. `load_from_dir` would start happily, so dry-run must
    // exit 0 — but it has to say so out loud rather than leaving a bare `0` in
    // the summary line as the only signal.
    let dir = temp_dir("empty");
    std::fs::write(dir.join("notes.txt"), "not a policy").unwrap();

    let (code, output) = dry_run(&dir);
    assert_eq!(code, 0, "output: {output}");
    assert!(
        output.contains("WARNING: no *.json files found"),
        "output: {output}"
    );
    assert!(
        output.contains("0 policy file(s) checked"),
        "output: {output}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dry_run_over_an_unreadable_directory_is_an_error() {
    let missing = std::env::temp_dir().join("macp-dry-run-absent-directory");
    std::fs::remove_dir_all(&missing).ok();
    let (code, output) = dry_run(&missing);
    assert_eq!(code, 1, "output: {output}");
    assert!(output.contains("MACP_POLICIES_DIR"), "output: {output}");
}

#[test]
fn dry_run_without_a_policies_dir_configured_is_an_error() {
    let (code, output) = run(None);
    assert_eq!(code, 1, "output: {output}");
    assert!(
        output.contains("requires MACP_POLICIES_DIR"),
        "output: {output}"
    );
}
