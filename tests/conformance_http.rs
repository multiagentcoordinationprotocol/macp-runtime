//! The conformance fixtures over the HTTP binding (RFC-MACP-0006 §4), plan
//! `plans/http-listener.md` P10, `http-listener` feature.
//!
//! One `MacpServer` (dev-mode auth, as the binary with
//! `MACP_ALLOW_INSECURE=1` and no tokens) is served over gRPC and over HTTP
//! on loopback `:0` ports in this process. The vendored fixture set
//! (`tests/conformance/`, byte-identical to the spec at `SPEC_REV`) runs
//! through `macp_conform::GrpcTarget` and `macp_conform::HttpTarget`, both
//! with the commitment-hash vectors in-process, exactly as the CI
//! `conform-cli` job runs the CLI. The HTTP run must claim what the gRPC run
//! claims, and every fixture that passes over gRPC must pass over HTTP.
//!
//! The negative counterpart (a deliberately broken HTTP Ack mapping makes
//! this run fail) is a unit test of the listener, `src/http/`, because its
//! hook is `#[cfg(test)]`-only and so exists in no integration-test build.
#![cfg(feature = "http-listener")]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use macp_conform::report::Outcome;
use macp_conform::{
    run_with_hash_target, FixtureSet, GrpcTarget, HttpTarget, InProcessTarget, Level, Report,
    RunOptions,
};
use macp_runtime::http::{HttpConfig, HttpListener};
use macp_runtime::log_store::LogStore;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use tonic::transport::server::TcpIncoming;
use tonic::transport::Server;

const CLAIM: &str = "Standard modes + multi_round, Server profile (hash vectors in-process)";

fn fixtures() -> FixtureSet {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    FixtureSet::from_dir(dir).expect("the vendored fixture set loads")
}

fn opts() -> RunOptions {
    RunOptions::default()
        .with_level(Level::Standard)
        .with_multi_round(true)
        .with_strict(true)
}

/// `(grpc base URL, http base URL)` of one server on loopback.
async fn serve_both() -> (String, String) {
    let runtime = Arc::new(Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));
    let server = MacpServer::new(runtime, SecurityLayer::dev_mode());

    let env: HashMap<&str, String> = HashMap::from([
        ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string()),
        ("MACP_ALLOW_INSECURE", "1".into()),
    ]);
    let config = HttpConfig::from_env(&move |k| env.get(k).cloned())
        .unwrap()
        .unwrap();
    let listener = HttpListener::bind(&config, server.clone()).await.unwrap();
    let http = listener.local_addr().unwrap();
    tokio::spawn(listener.serve(std::future::pending()));

    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let grpc = incoming.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(MacpRuntimeServiceServer::new(server))
            .serve_with_incoming(incoming),
    );
    (format!("http://{grpc}"), format!("http://{http}"))
}

fn outcomes(report: &Report) -> BTreeMap<String, Outcome> {
    report
        .fixtures
        .iter()
        .map(|r| (r.name.clone(), r.outcome))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_fixture_that_passes_over_grpc_passes_over_http() {
    let set = fixtures();
    let (grpc_url, http_url) = serve_both().await;
    let hashes = InProcessTarget::new();
    let idle = Duration::from_millis(250);

    let mut grpc = GrpcTarget::connect(&grpc_url)
        .await
        .unwrap()
        .with_stream_idle(idle);
    let grpc_report = run_with_hash_target(&mut grpc, &hashes, &set, &opts()).await;

    let mut http = HttpTarget::connect(&http_url)
        .await
        .unwrap()
        .with_stream_idle(idle);
    let (name, version) = http.runtime_info().await.unwrap();
    assert_eq!(
        (name.as_str(), version.as_str()),
        ("macp-runtime", env!("CARGO_PKG_VERSION")),
        "the --hash-vectors in-process release check reads Initialize over HTTP"
    );
    let http_report = run_with_hash_target(&mut http, &hashes, &set, &opts()).await;

    assert!(
        !grpc_report.is_failure(),
        "gRPC baseline:\n{}",
        grpc_report.render_text()
    );
    assert!(
        !http_report.is_failure(),
        "HTTP:\n{}",
        http_report.render_text()
    );
    assert_eq!(grpc_report.claim.as_deref(), Some(CLAIM));
    assert_eq!(http_report.claim.as_deref(), Some(CLAIM));

    let (g, h) = (outcomes(&grpc_report), outcomes(&http_report));
    assert_eq!(
        g.keys().collect::<Vec<_>>(),
        h.keys().collect::<Vec<_>>(),
        "both runs select the same fixtures"
    );
    let passed_over_grpc: Vec<_> = g
        .iter()
        .filter(|(_, o)| **o == Outcome::Pass)
        .map(|(n, _)| n)
        .collect();
    assert!(
        passed_over_grpc.len() > 40,
        "the vendored set is the full pack, got {}",
        passed_over_grpc.len()
    );
    for name in &passed_over_grpc {
        assert_eq!(
            h[*name],
            Outcome::Pass,
            "{name} passes over gRPC, not over HTTP"
        );
    }
    // Same outcome everywhere: the one clocked fixture is skipped
    // (`needs-clock-control`) on both bindings, and nothing else differs.
    assert_eq!(g, h);
    // History is observed over HTTP (not reported unverified): only the
    // assertions a Server cannot show are, as over gRPC.
    for r in &http_report.fixtures {
        assert!(
            !r.unverified
                .iter()
                .any(|u| u == "history" || u == "resolution"),
            "{}: {:?}",
            r.name,
            r.unverified
        );
    }
}
