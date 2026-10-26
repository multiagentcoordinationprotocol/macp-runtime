//! `macp replay` over a recorded decision session (`tests/fixtures/data`): a
//! runtime's file-backend data directory, written with
//! `MACP_COMPACT_TERMINAL_LOGS=0` by `cargo run --bin client`.

use std::path::{Path, PathBuf};

use macp_cli::replay::{self, ReplayError, Source};

const SID: &str = "4c7542ed-6b5e-4290-bbd1-9ed372ed914c";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn data_dir() -> Source {
    Source::DataDir {
        dir: fixtures().join("data"),
        session_id: SID.into(),
    }
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn the_recorded_policy_replays_to_the_stored_outcome() {
    let r = block_on(replay::run(&data_dir(), None)).unwrap();
    assert_eq!(r.recorded_policy_id, "policy.default");
    assert!(!replay::outcome_changed(&r));
    assert_eq!(r.first_divergence, None);
    let text = replay::render_text(&r, false);
    assert!(text.contains("Resolved by Commitment m5 at #5"), "{text}");
    assert!(text.contains("integrity  ok"), "{text}");
}

#[test]
fn a_stricter_candidate_changes_the_outcome() {
    let candidate = replay::read_policy(&fixtures().join("unanimous.json")).unwrap();
    let r = block_on(replay::run(&data_dir(), Some(&candidate))).unwrap();
    assert!(replay::outcome_changed(&r));
    // history_index 4 is the Commitment, the 5th accepted envelope.
    assert_eq!(r.first_divergence, Some(4));
    let text = replay::render_text(&r, true);
    assert!(
        text.contains("denied     #5 m5: threshold_not_met"),
        "{text}"
    );
    assert!(text.contains("outcome    changed"), "{text}");
    let json = replay::render_json(&r);
    assert_eq!(json["outcome_changed"], true);
    assert_eq!(json["candidate"]["state"], "Open");
    assert_eq!(json["recorded"]["resolution"]["commitment_id"], "c1");
}

#[test]
fn a_log_file_replays_without_a_stored_session() {
    let log = fixtures().join("data/sessions").join(SID).join("log.jsonl");
    let r = block_on(replay::run(&Source::File(log), None)).unwrap();
    assert!(replay::render_text(&r, false).contains("Resolved"));
}

#[test]
fn a_log_that_no_longer_reaches_the_stored_state_is_an_integrity_failure() {
    let src = fixtures().join("data/sessions").join(SID);
    let tmp = std::env::temp_dir().join(format!("macp-cli-replay-{}", std::process::id()));
    let dst = tmp.join("sessions").join(SID);
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::copy(src.join("session.json"), dst.join("session.json")).unwrap();
    // Drop the Commitment: the log now ends Open, the session says Resolved.
    let log = std::fs::read_to_string(src.join("log.jsonl")).unwrap();
    let kept: Vec<&str> = log.lines().take(4).collect();
    std::fs::write(dst.join("log.jsonl"), kept.join("\n")).unwrap();
    let out = block_on(replay::run(
        &Source::DataDir {
            dir: tmp.clone(),
            session_id: SID.into(),
        },
        None,
    ));
    std::fs::remove_dir_all(&tmp).ok();
    assert!(matches!(out, Err(ReplayError::Integrity(_))), "{out:?}");
}

#[test]
fn an_unknown_session_cannot_run() {
    let out = block_on(replay::run(
        &Source::DataDir {
            dir: fixtures().join("data"),
            session_id: "nope".into(),
        },
        None,
    ));
    assert!(matches!(out, Err(ReplayError::Unavailable(_))), "{out:?}");
}
