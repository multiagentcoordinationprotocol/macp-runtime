//! Telemetry bootstrap (R2 P1): telemetry is inert unless configured, the
//! binary survives a misconfiguration, log output does not change while
//! telemetry is active, and an `https://` collector is reached through the
//! native roots plus `OTEL_EXPORTER_OTLP_CERTIFICATE`.
//!
//! The dry-run golden test runs in every build, so the default and `otel`
//! binaries are pinned to the same file. Everything else needs `otel`, except
//! the feature-off warning test, which needs its absence.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Remove ANSI escapes and a leading RFC 3339 timestamp from each line.
fn normalize(output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    for line in output.split_inclusive('\n') {
        let mut clean = String::with_capacity(line.len());
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' && chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                clean.push(c);
            }
        }
        // `2026-10-31T12:00:00.000000Z  INFO ...` -> `INFO ...`
        let bytes = clean.as_bytes();
        let stripped = if bytes.len() > 20
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[..4].iter().all(u8::is_ascii_digit)
        {
            match clean.find(' ') {
                Some(i) => clean[i..].trim_start().to_string(),
                None => clean,
            }
        } else {
            clean
        };
        out.push_str(&stripped);
    }
    out
}

/// A runtime command with a scrubbed environment: no `RUST_LOG`, no `OTEL_*`
/// inherited from the developer's shell or CI.
fn runtime_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macp-runtime"));
    command.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    command
}

fn read_all(child: &mut std::process::Child) -> (String, String) {
    let mut stdout = String::new();
    if let Some(mut s) = child.stdout.take() {
        s.read_to_string(&mut stdout).ok();
    }
    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        s.read_to_string(&mut stderr).ok();
    }
    (stdout, stderr)
}

/// `MACP_POLICIES_DRY_RUN=1` with no OTEL variables prints exactly the
/// checked-in golden, in the default build and in the `otel` build alike.
#[test]
fn dry_run_output_matches_the_golden_in_every_build() {
    let scratch = tempfile::tempdir().unwrap();
    let policies = scratch.path().join("policies");
    std::fs::create_dir(&policies).unwrap();
    std::fs::write(
        policies.join("majority.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "policy_id": "policy.golden.majority",
            "mode": "macp.mode.decision.v1",
            "description": "golden fixture",
            "rules": { "voting": { "algorithm": "majority", "threshold": 0.5 } },
            "schema_version": 1,
        }))
        .unwrap(),
    )
    .unwrap();

    let mut child = runtime_command()
        .current_dir(scratch.path())
        .env("MACP_POLICIES_DRY_RUN", "1")
        .env("MACP_POLICIES_DIR", "policies")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_ALLOW_INSECURE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn macp-runtime");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("dry run did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (stdout, stderr) = read_all(&mut child);
    assert_eq!(status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");

    let actual = format!(
        "--- stdout\n{}--- stderr\n{}",
        normalize(&stdout),
        normalize(&stderr)
    );
    let golden_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/policies_dry_run.txt");
    let golden = std::fs::read_to_string(&golden_path).expect("read the golden");
    assert_eq!(
        actual,
        golden,
        "dry-run output drifted from {}",
        golden_path.display()
    );
}

/// Feature off, an OTLP endpoint variable earns exactly one warning, and the
/// variable is otherwise ignored.
#[cfg(not(feature = "otel"))]
#[test]
fn feature_off_binary_warns_once_when_an_endpoint_is_set() {
    let scratch = tempfile::tempdir().unwrap();
    let mut child = runtime_command()
        .current_dir(scratch.path())
        .env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
        .env("MACP_POLICIES_DRY_RUN", "1")
        .env("MACP_POLICIES_DIR", ".")
        .env("MACP_ALLOW_INSECURE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn macp-runtime");
    let status = child.wait().unwrap();
    let (stdout, stderr) = read_all(&mut child);
    assert_eq!(status.code(), Some(0), "stdout: {stdout}\nstderr: {stderr}");
    let all = format!("{stdout}{stderr}");
    let warnings: Vec<&str> = all
        .lines()
        .filter(|l| l.contains("built without the `otel` feature"))
        .collect();
    assert_eq!(warnings.len(), 1, "output: {all}");
    assert!(warnings[0].contains("WARN"), "{}", warnings[0]);
}

#[cfg(feature = "otel")]
#[path = "support/fake_collector.rs"]
mod fake_collector;

#[cfg(feature = "otel")]
mod otel {
    use super::*;
    use crate::fake_collector::FakeCollector;

    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use macp_runtime::log_store::LogStore;
    use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeService;
    use macp_runtime::pb::{Envelope, SendRequest, SessionStartPayload};
    use macp_runtime::registry::SessionRegistry;
    use macp_runtime::runtime::Runtime;
    use macp_runtime::security::SecurityLayer;
    use macp_runtime::server::MacpServer;
    use macp_runtime::telemetry::init::{self, SignalExport, TRACER_NAME};
    use macp_runtime::telemetry::{Telemetry, TARGET};
    use opentelemetry::trace::{Tracer as _, TracerProvider as _};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use prost::Message;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::{Layer, SubscriberExt};
    use tracing_subscriber::EnvFilter;

    // ---- binary: a misconfiguration never stops the runtime ----

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Poll `grpc.health.v1` until SERVING, for at most 30 s. Fails fast,
    /// with the reason, if the child exits first; every connect and call is
    /// individually bounded, so a hung attempt cannot eat the deadline.
    async fn wait_serving(child: &mut std::process::Child, addr: &str) -> Result<(), String> {
        use tonic_health::pb::health_check_response::ServingStatus;
        use tonic_health::pb::health_client::HealthClient;
        use tonic_health::pb::HealthCheckRequest;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                return Err(format!("runtime exited with {status} before SERVING"));
            }
            if let Ok(channel) = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
                .unwrap()
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(2))
                .connect()
                .await
            {
                let mut client = HealthClient::new(channel);
                if let Ok(resp) = client
                    .check(HealthCheckRequest {
                        service: String::new(),
                    })
                    .await
                {
                    if resp.into_inner().status == ServingStatus::Serving as i32 {
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err("runtime never reported SERVING within 30 s".to_string())
    }

    /// Start the binary with `extra` env, wait for health SERVING, stop it,
    /// and return its combined, normalized output.
    async fn run_until_serving(extra: &[(&str, String)]) -> String {
        run_serving_then(extra, |_| async {}).await
    }

    /// One accepted SessionStart through `Send` on the binary at `addr`, so
    /// the runtime creates real SERVER and `process` spans (R2 P3).
    async fn drive_one_send(addr: String) {
        use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let mut req = tonic::Request::new(SendRequest {
            envelope: Some(session_start_envelope()),
        });
        req.metadata_mut()
            .insert("authorization", "Bearer agent://a".parse().unwrap());
        let ack = MacpRuntimeServiceClient::new(channel)
            .send(req)
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(ack.ok, "{ack:?}");
    }

    /// [`run_until_serving`], running `after(addr)` once the binary serves and
    /// before stopping it. The batch span processor's delay is 100 ms, so
    /// any span the traffic creates is exported while the binary still runs.
    async fn run_serving_then<F, Fut>(extra: &[(&str, String)], after: F) -> String
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        // The probed gRPC port can be taken by another test before the child
        // binds it (a TOCTOU race): a child that exits with `AddrInUse` is
        // respawned on a fresh port, at most three times.
        let mut after = Some(after);
        for attempt in 1..=3 {
            let scratch = tempfile::tempdir().unwrap();
            let addr = format!("127.0.0.1:{}", free_port());
            let mut command = runtime_command();
            command
                .current_dir(scratch.path())
                .env("MACP_ALLOW_INSECURE", "1")
                .env("MACP_MEMORY_ONLY", "1")
                .env("MACP_BIND_ADDR", &addr)
                .env("RUST_LOG", "info")
                .env("OTEL_BSP_SCHEDULE_DELAY", "100")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in extra {
                command.env(k, v);
            }
            let mut child = Child(command.spawn().expect("spawn macp-runtime"));
            let serving = wait_serving(&mut child.0, &addr).await;
            if serving.is_ok() {
                (after.take().expect("runs once"))(addr.clone()).await;
            }
            let _ = child.0.kill();
            let _ = child.0.wait();
            let (stdout, stderr) = read_all(&mut child.0);
            let output = normalize(&format!("{stdout}{stderr}"));
            if let Err(reason) = serving {
                if attempt < 3
                    && (output.contains("AddrInUse") || output.contains("Address already in use"))
                {
                    continue;
                }
                panic!("{reason}; output:\n{output}");
            }
            return output;
        }
        unreachable!("the last attempt returns or panics")
    }

    /// `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf` with an endpoint: the
    /// runtime starts, logs one error naming the variable, stays inactive, and
    /// sends the collector nothing.
    ///
    /// The plan says "stderr"; `tracing`'s fmt writer is stdout, so the line
    /// is looked for in the combined output (as `tests/policies_dry_run.rs`
    /// does).
    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_protocol_logs_one_error_and_the_runtime_still_serves() {
        let collector = FakeCollector::start().await;
        let output = run_serving_then(
            &[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint()),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf".to_string()),
            ],
            |addr| async move {
                drive_one_send(addr).await;
                // Several batch delays: an active runtime would have exported
                // by now (the control below proves it does).
                tokio::time::sleep(Duration::from_millis(500)).await;
            },
        )
        .await;
        let errors: Vec<&str> = output.lines().filter(|l| l.contains("ERROR")).collect();
        assert_eq!(errors.len(), 1, "output:\n{output}");
        assert!(
            errors[0].contains("OTEL_EXPORTER_OTLP_PROTOCOL"),
            "{}",
            errors[0]
        );
        assert!(
            !output.contains("OpenTelemetry enabled"),
            "telemetry must stay inactive; output:\n{output}"
        );
        // Real traffic created real spans (R2 P3); none reached the collector.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(collector.requests().is_empty());
    }

    /// Assert the runtime served, logged exactly one ERROR naming `var`, and
    /// stayed inactive.
    fn assert_one_error_naming(output: &str, var: &str) {
        let errors: Vec<&str> = output.lines().filter(|l| l.contains("ERROR")).collect();
        assert_eq!(errors.len(), 1, "output:\n{output}");
        assert!(errors[0].contains(var), "{}", errors[0]);
        assert!(
            !output.contains("OpenTelemetry enabled"),
            "telemetry must stay inactive; output:\n{output}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_certificate_logs_one_error_and_the_runtime_still_serves() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-ca.pem");
        let output = run_until_serving(&[
            (
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "https://127.0.0.1:4317".to_string(),
            ),
            (
                "OTEL_EXPORTER_OTLP_CERTIFICATE",
                missing.to_str().unwrap().to_string(),
            ),
        ])
        .await;
        assert_one_error_naming(&output, "OTEL_EXPORTER_OTLP_CERTIFICATE");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn non_uri_endpoint_logs_one_error_and_the_runtime_still_serves() {
        let output = run_until_serving(&[(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "not a uri".to_string(),
        )])
        .await;
        assert_one_error_naming(&output, "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
    }

    // ---- bounded shutdown ----

    /// A listener that accepts and never answers: an export to it hangs
    /// until the exporter's own timeout (10 s by default).
    fn blackhole() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for conn in listener.incoming().flatten() {
                held.push(conn);
            }
        });
        format!("http://{addr}")
    }

    /// An active guard with one span pending against a dead collector shuts
    /// down, and its tokio runtime drops, within the 5 s bound (plus slack).
    /// Dropping the runtime is the point: tokio joins its blocking pool
    /// without limit, so the bound must hold through the drop, not just
    /// through `shutdown().await`.
    #[test]
    fn shutdown_against_a_dead_collector_is_bounded() {
        let closed_port = format!("http://127.0.0.1:{}", free_port());
        for endpoint in [closed_port, blackhole()] {
            let started = Instant::now();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let provider = init::build_tracer_provider(
                    &SignalExport {
                        endpoint: endpoint.clone(),
                        ca_pem: None,
                    },
                    init::resource(&|_| None),
                )
                .unwrap();
                provider.tracer(TRACER_NAME).in_span("pending", |_| {});
                init::Guard::from_tracer_provider(provider).shutdown().await;
            });
            drop(rt);
            let elapsed = started.elapsed();
            assert!(
                elapsed < Duration::from_secs(7),
                "{endpoint}: shutdown + runtime drop took {elapsed:?}"
            );
        }
    }

    // ---- the test-only active constructor ----

    /// `active_for_tests` gives an active `Telemetry` and a dispatcher bound
    /// to the caller's provider, beside an inactive runtime in one process.
    #[test]
    fn active_for_tests_routes_telemetry_spans_to_the_callers_provider() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let (dispatch, telemetry) = init::active_for_tests(&provider, None);
        assert!(telemetry.is_enabled());
        let runtime = Runtime::new(
            Arc::new(macp_runtime::storage::MemoryBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
        )
        .with_telemetry(telemetry);
        drop(runtime);
        assert!(!macp_runtime::telemetry::Telemetry::disabled().is_enabled());
        tracing::dispatcher::with_default(&dispatch, || {
            let _s = tracing::info_span!(target: TARGET, "probe", otel.name = "probe").entered();
            tracing::info_span!(target: "elsewhere", "ignored").in_scope(|| {});
        });
        provider.force_flush().unwrap();
        let names: Vec<_> = exporter
            .get_finished_spans()
            .unwrap()
            .into_iter()
            .map(|s| s.name.to_string())
            .collect();
        assert_eq!(names, vec!["probe"]);
    }

    /// The control: with `grpc`, the same binary activates.
    #[tokio::test(flavor = "multi_thread")]
    async fn grpc_protocol_activates_and_the_runtime_serves() {
        let collector = FakeCollector::start().await;
        let output = run_serving_then(
            &[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint()),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc".to_string()),
            ],
            |addr| async {
                drive_one_send(addr).await;
                let spans = collector.wait_for_spans(2, Duration::from_secs(10)).await;
                let names: Vec<&str> = spans.iter().map(|s| s.name.as_str()).collect();
                assert!(
                    names.contains(&"process macp.mode.decision.v1"),
                    "{names:?}"
                );
                assert!(
                    names.contains(&"macp.v1.MACPRuntimeService/Send"),
                    "{names:?}"
                );
            },
        )
        .await;
        assert!(
            output.contains("OpenTelemetry enabled"),
            "output:\n{output}"
        );
        assert!(!output.contains("ERROR"), "output:\n{output}");
    }

    // ---- in-process: log lines are unchanged while telemetry is active ----

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }
    impl Buf {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    const SID: &str = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f60";

    fn session_start_envelope() -> Envelope {
        let payload = SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://a".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "SessionStart".into(),
            message_id: "m1".into(),
            session_id: SID.into(),
            sender: String::new(),
            timestamp_unix_ms: 1_800_000_000_000,
            payload,
        }
    }

    /// One SessionStart through `MacpServer::send`, on a runtime carrying
    /// `telemetry`. Active, the server creates the real `process` span (R2
    /// P3) and the runtime logs inside it; inactive telemetry creates no span
    /// at all, which is what the fmt-only baseline models.
    async fn scenario(telemetry: Telemetry) {
        let runtime = Runtime::new(
            Arc::new(macp_runtime::storage::MemoryBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
        )
        .with_clock(Arc::new(macp_runtime::macp_core::ManualClock::new(
            1_800_000_000_000,
        )))
        .with_telemetry(telemetry);
        let server = MacpServer::new(Arc::new(runtime), SecurityLayer::dev_mode());
        let mut req = tonic::Request::new(SendRequest {
            envelope: Some(session_start_envelope()),
        });
        req.metadata_mut()
            .insert("authorization", "Bearer agent://a".parse().unwrap());
        let ack = server.send(req).await.unwrap().into_inner().ack.unwrap();
        assert!(ack.ok, "{ack:?}");
    }

    async fn fmt_only_output(directive: &str) -> String {
        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new(directive))
            .with_writer(buf.clone())
            .without_time()
            .with_ansi(false)
            .finish();
        let _g = tracing::subscriber::set_default(subscriber);
        scenario(Telemetry::disabled()).await;
        buf.contents()
    }

    /// The log output and the exported spans (read before the provider drops:
    /// its shutdown clears the in-memory exporter).
    async fn active_output(directive: &str) -> (String, Vec<SpanData>) {
        let buf = Buf::default();
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(buf.clone())
                    .without_time()
                    .with_ansi(false)
                    .with_filter(init::fmt_filter(EnvFilter::new(directive))),
            )
            .with(init::otel_layer(provider.tracer(TRACER_NAME)));
        {
            let _g = tracing::subscriber::set_default(subscriber);
            // The active `Telemetry` only; this test's own subscriber (with
            // the fmt layer) is the dispatcher.
            let (_, telemetry) = init::active_for_tests(&provider, None);
            scenario(telemetry).await;
        }
        provider.force_flush().unwrap();
        (buf.contents(), exporter.get_finished_spans().unwrap())
    }

    async fn assert_log_unchanged(directive: &str) {
        let baseline = fmt_only_output(directive).await;
        let (active, spans) = active_output(directive).await;

        // Falsifiable: the runtime logs inside the `process` span, and the
        // otel layer really is live (it exported that real span).
        assert!(
            baseline.contains("session started"),
            "baseline:\n{baseline}"
        );
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].name, "process macp.mode.decision.v1");
        // No `tracing` event (and so no `reason`/error text) is exported.
        assert!(spans[0].events.is_empty(), "{:?}", spans[0].events);

        assert_eq!(active, baseline, "directive {directive}");
        assert!(!active.contains("process{"), "{active}");
        assert!(!active.contains("grpc{"), "{active}");
    }

    #[tokio::test]
    async fn log_lines_are_unchanged_while_telemetry_is_active() {
        assert_log_unchanged("debug").await;
    }

    /// An operator `RUST_LOG` naming the telemetry target cannot pull its
    /// spans into the log: the `filter_fn` exclusion wins.
    #[tokio::test]
    async fn log_lines_are_unchanged_even_when_rust_log_names_the_telemetry_target() {
        assert_log_unchanged("debug,macp_runtime::telemetry=info").await;
        assert_log_unchanged("macp_runtime::telemetry=trace,macp_runtime=debug").await;
    }

    // ---- TLS: native roots plus OTEL_EXPORTER_OTLP_CERTIFICATE ----

    struct TestPki {
        ca_pem: String,
        leaf_pem: String,
        leaf_key_pem: String,
    }

    fn test_pki() -> TestPki {
        use rcgen::{
            BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, KeyUsagePurpose,
        };
        let mut ca = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca.distinguished_name
            .push(DnType::CommonName, "macp otel test CA");
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca_key = KeyPair::generate().unwrap();
        let ca_cert = ca.self_signed(&ca_key).unwrap();

        let mut leaf =
            CertificateParams::new(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
        leaf.distinguished_name
            .push(DnType::CommonName, "fake collector");
        let leaf_key = KeyPair::generate().unwrap();
        let ca_issuer = Issuer::from_params(&ca, &ca_key);
        let leaf_cert = leaf.signed_by(&leaf_key, &ca_issuer).unwrap();
        TestPki {
            ca_pem: ca_cert.pem(),
            leaf_pem: leaf_cert.pem(),
            leaf_key_pem: leaf_key.serialize_pem(),
        }
    }

    /// Build the runtime's tracer provider for `traces`, export one span, and
    /// return what the flush reported.
    async fn export_one_span(traces: SignalExport) -> Result<(), String> {
        let provider = init::build_tracer_provider(&traces, init::resource(&|_| None))?;
        let tracer = provider.tracer(TRACER_NAME);
        tracer.in_span("tls probe", |_| {});
        let result = tokio::task::spawn_blocking(move || {
            let flushed = provider.force_flush();
            let _ = provider.shutdown_with_timeout(Duration::from_secs(5));
            flushed
        })
        .await
        .unwrap();
        result.map_err(|e| e.to_string())
    }

    /// The `https://` path enables the native roots (so a public-CA collector
    /// verifies) and trusts `OTEL_EXPORTER_OTLP_CERTIFICATE` on top: with the
    /// private CA the span arrives, without it the handshake fails and nothing
    /// arrives. `export_config` -> `build_tracer_provider` is exactly the
    /// path `init` takes; only the span source is the test's own, because P1
    /// creates no runtime spans.
    #[tokio::test(flavor = "multi_thread")]
    async fn https_collector_is_reached_only_with_the_configured_ca() {
        let pki = test_pki();
        let collector = FakeCollector::start_tls(&pki.leaf_pem, &pki.leaf_key_pem).await;
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, &pki.ca_pem).unwrap();
        let ca_path = ca_path.to_str().unwrap().to_string();
        let endpoint = collector.endpoint();
        assert!(endpoint.starts_with("https://"));

        // Without the certificate variable: native roots only, handshake fails.
        let env_without = {
            let endpoint = endpoint.clone();
            move |k: &str| (k == "OTEL_EXPORTER_OTLP_ENDPOINT").then(|| endpoint.clone())
        };
        let config = init::export_config(&env_without, init::activation(&env_without)).unwrap();
        let without = export_one_span(config.traces.unwrap()).await;
        assert!(without.is_err(), "export must fail the TLS handshake");
        assert!(collector.spans().is_empty());

        // With it: the private CA is trusted and the span arrives.
        let env_with = {
            let endpoint = endpoint.clone();
            let ca_path = ca_path.clone();
            move |k: &str| match k {
                "OTEL_EXPORTER_OTLP_ENDPOINT" => Some(endpoint.clone()),
                "OTEL_EXPORTER_OTLP_CERTIFICATE" => Some(ca_path.clone()),
                _ => None,
            }
        };
        let config = init::export_config(&env_with, init::activation(&env_with)).unwrap();
        export_one_span(config.traces.unwrap())
            .await
            .expect("export through the private CA");
        let spans = collector.wait_for_spans(1, Duration::from_secs(10)).await;
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].name, "tls probe");
    }
}
