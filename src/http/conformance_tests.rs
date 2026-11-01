//! The conformance run over HTTP catches a broken HTTP mapping (plan
//! `plans/http-listener.md` P10).
//!
//! `tests/conformance_http.rs` shows the fixture pack passing over the
//! binding; this shows the same run failing when the listener's `Send` Ack
//! mapping is broken on purpose (`HttpListener::with_broken_ack_mapping`, a
//! `#[cfg(test)]`-only hook that inverts `ack.ok`), so that pass is not
//! vacuous. A control run against an unbroken listener with the same
//! configuration passes, so the failure is the hook's.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use macp_conform::report::Outcome;
use macp_conform::{
    run_with_hash_target, FixtureSet, HttpTarget, InProcessTarget, Level, RunOptions,
};

use super::{HttpConfig, HttpListener};
use crate::server::MacpServer;

async fn listener(broken: bool) -> String {
    let runtime = Arc::new(crate::runtime::Runtime::new(
        Arc::new(crate::storage::MemoryBackend),
        Arc::new(crate::registry::SessionRegistry::new()),
        Arc::new(crate::log_store::LogStore::new()),
    ));
    let server = MacpServer::new(runtime, crate::security::SecurityLayer::dev_mode());
    let env: HashMap<&str, String> = HashMap::from([
        ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string()),
        ("MACP_ALLOW_INSECURE", "1".into()),
    ]);
    let config = HttpConfig::from_env(&move |k| env.get(k).cloned())
        .unwrap()
        .unwrap();
    let mut listener = HttpListener::bind(&config, server).await.unwrap();
    if broken {
        listener = listener.with_broken_ack_mapping();
    }
    let addr = listener.local_addr().unwrap();
    tokio::spawn(listener.serve(std::future::pending()));
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broken_ack_mapping_fails_the_conformance_run() {
    let set =
        FixtureSet::from_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance"))
            .unwrap();
    let opts = RunOptions::default()
        .with_level(Level::Standard)
        .with_multi_round(true)
        .with_strict(true);
    let hashes = InProcessTarget::new();

    let mut good = HttpTarget::connect(&listener(false).await)
        .await
        .unwrap()
        .with_stream_idle(Duration::from_millis(250));
    let control = run_with_hash_target(&mut good, &hashes, &set, &opts).await;
    assert!(!control.is_failure(), "control:\n{}", control.render_text());

    let mut bad = HttpTarget::connect(&listener(true).await).await.unwrap();
    let broken = run_with_hash_target(&mut bad, &hashes, &set, &opts).await;
    assert!(broken.is_failure(), "the broken mapping went unnoticed");
    assert_eq!(broken.claim, None, "{}", broken.render_text());
    // Every session fixture fails (its SessionStart now reads as rejected);
    // only the hash vectors, which run in-process, still pass.
    for r in &broken.fixtures {
        let expected = if r.name.starts_with("cmt-hash/") {
            Outcome::Pass
        } else if r.reason.as_deref() == Some(macp_conform::runner::NEEDS_CLOCK_CONTROL) {
            Outcome::Skipped
        } else {
            Outcome::Fail
        };
        assert_eq!(r.outcome, expected, "{}", r.name);
    }
}
