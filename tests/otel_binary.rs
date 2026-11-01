//! Real-binary OpenTelemetry checks (R2 P6): the `otel` build of the
//! `macp-runtime` binary, a real OTLP/gRPC exporter, and a fake collector.
//!
//! What this adds beyond `tests/otel_bootstrap.rs` (which already proves the
//! binary activates and that the SERVER and `process` spans reach a
//! collector):
//!
//! 1. the exported **resource** (`service.name` defaulted to `macp-runtime`
//!    with `OTEL_SERVICE_NAME` unset, `service.version` = the crate version)
//!    and the four semconv **metrics** through the real `PeriodicReader`;
//! 2. with no `OTEL_*` variable at all, nothing dials the OTLP defaults
//!    (`127.0.0.1:4317`, `[::1]:4317`, `127.0.0.1:4318`) during a session
//!    and a graceful shutdown;
//! 3. on SIGINT the batch span processor's pending spans are flushed within
//!    the 5 s shutdown bound.
//!
//! Every wait is bounded. SIGINT is delivered with `kill -INT`, so the file is
//! Unix-only.
#![cfg(all(feature = "otel", unix))]

#[path = "support/binary_child.rs"]
mod binary_child;
#[path = "support/fake_collector.rs"]
mod fake_collector;

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fake_collector::FakeCollector;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::{CancelSessionRequest, Envelope, SendRequest, SessionStartPayload};
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::trace::v1::span::SpanKind;
use prost::Message;

const SID: &str = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f61";
const PROCESS_SPAN: &str = "process macp.mode.decision.v1";
const SEND_SPAN: &str = "macp.v1.MACPRuntimeService/Send";
const METRICS: [&str; 4] = [
    "macp.envelope.processed",
    "macp.envelope.process.duration",
    "macp.session.transitions",
    "macp.session.duration",
];

use binary_child::{addr_in_use, free_port, Child, SPAWN_ATTEMPTS};

/// Send SIGINT and wait (bounded) for the process to exit. Returns the exit
/// status, or `None` if it did not exit within `limit`.
fn interrupt_and_wait(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    child.signal("INT");
    child.wait_exit(limit)
}

/// Spawn the binary in dev mode with a scrubbed environment (no `OTEL_*` or
/// `RUST_LOG` inherited from the developer's shell or CI) plus `extra`.
fn spawn_runtime(scratch: &std::path::Path, addr: &str, extra: &[(&str, String)]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macp-runtime"));
    command.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    command
        .current_dir(scratch)
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", addr)
        .env("MACP_SHUTDOWN_DRAIN_SECS", "1")
        .env("RUST_LOG", "info");
    for (k, v) in extra {
        command.env(k, v);
    }
    Child::spawn(&mut command)
}

/// Spawn on a probed gRPC port and wait for SERVING: a probe that lost the
/// race to another test (`AddrInUse`) is respawned with a fresh port, at most
/// `SPAWN_ATTEMPTS` times. Returns the child and its gRPC address.
async fn start_runtime(scratch: &std::path::Path, extra: &[(&str, String)]) -> (Child, String) {
    for attempt in 1..=SPAWN_ATTEMPTS {
        let addr = format!("127.0.0.1:{}", free_port());
        let mut child = spawn_runtime(scratch, &addr, extra);
        match wait_serving(&mut child, &addr).await {
            Ok(()) => return (child, addr),
            Err(out) => assert!(
                addr_in_use(&out) && attempt < SPAWN_ATTEMPTS,
                "runtime exited before SERVING; output:\n{out}"
            ),
        }
    }
    unreachable!("the last attempt panics")
}

/// Poll `grpc.health.v1` every 10 ms until SERVING, for at most 30 s. Every
/// connect and call is individually bounded. If the child exits first, or the
/// deadline passes, panic at once with the reason and the child's output,
/// except that an exit returns `Err(output)` for `start_runtime` to judge.
async fn wait_serving(child: &mut Child, addr: &str) -> Result<(), String> {
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !child.alive() {
            return Err(child.output());
        }
        if let Ok(channel) = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
            .unwrap()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(2))
            .connect()
            .await
        {
            if let Ok(resp) = HealthClient::new(channel)
                .check(HealthCheckRequest {
                    service: String::new(),
                })
                .await
            {
                if resp.into_inner().status == ServingStatus::Serving as i32 && child.alive() {
                    return Ok(());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.kill();
    panic!(
        "runtime never reported SERVING within 30 s; output:\n{}",
        child.output()
    );
}

fn session_start_envelope() -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: "macp.mode.decision.v1".into(),
        message_type: "SessionStart".into(),
        message_id: "m1".into(),
        session_id: SID.into(),
        sender: String::new(),
        timestamp_unix_ms: 1_800_000_000_000,
        payload: SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://a".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec(),
    }
}

fn as_agent<T>(inner: T) -> tonic::Request<T> {
    let mut req = tonic::Request::new(inner);
    req.metadata_mut()
        .insert("authorization", "Bearer agent://a".parse().unwrap());
    req
}

/// A scripted session through the real gRPC boundary: an accepted
/// SessionStart, then (when `cancel`) an initiator CancelSession, which ends
/// the session and so feeds `macp.session.duration`. The channel is dropped on
/// return, so a later graceful shutdown has no connection to drain.
async fn drive_session(addr: &str, cancel: bool) {
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut client = MacpRuntimeServiceClient::new(channel);
    let ack = client
        .send(as_agent(SendRequest {
            envelope: Some(session_start_envelope()),
        }))
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert!(ack.ok, "{ack:?}");
    if cancel {
        client
            .cancel_session(as_agent(CancelSessionRequest {
                session_id: SID.into(),
                reason: "done".into(),
            }))
            .await
            .unwrap();
    }
}

fn string_attr(attrs: &[KeyValue], key: &str) -> Option<String> {
    attrs.iter().find(|kv| kv.key == key).and_then(|kv| {
        match kv.value.as_ref().and_then(|v| v.value.as_ref()) {
            Some(Value::StringValue(s)) => Some(s.clone()),
            _ => None,
        }
    })
}

/// Poll `ready` every 25 ms until it holds or `timeout` passes.
async fn eventually(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Case 1: resource, `process` span, SERVER span and the four metrics reach
/// the collector, with `OTEL_SERVICE_NAME` unset.
#[tokio::test(flavor = "multi_thread")]
async fn resource_spans_and_metrics_reach_the_collector() {
    let collector = FakeCollector::start().await;
    let scratch = tempfile::tempdir().unwrap();
    let (mut child, addr) = start_runtime(
        scratch.path(),
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint()),
            ("OTEL_METRIC_EXPORT_INTERVAL", "500".to_string()),
            ("OTEL_BSP_SCHEDULE_DELAY", "100".to_string()),
        ],
    )
    .await;
    drive_session(&addr, true).await;

    let spans_arrived = eventually(Duration::from_secs(10), || {
        let spans = collector.spans();
        spans.iter().any(|s| s.name == PROCESS_SPAN)
            && spans
                .iter()
                .any(|s| s.name == SEND_SPAN && s.kind == SpanKind::Server as i32)
    })
    .await;
    let names: Vec<String> = collector.spans().into_iter().map(|s| s.name).collect();
    assert!(spans_arrived, "spans received: {names:?}");

    // Every exported batch carries the default resource.
    let requests = collector.requests();
    let resources: Vec<_> = requests
        .iter()
        .flat_map(|r| &r.resource_spans)
        .map(|rs| rs.resource.clone().unwrap_or_default().attributes)
        .collect();
    assert!(!resources.is_empty());
    for attrs in &resources {
        assert_eq!(
            string_attr(attrs, "service.name").as_deref(),
            Some("macp-runtime")
        );
        assert_eq!(
            string_attr(attrs, "service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    let metrics_arrived = eventually(Duration::from_secs(10), || {
        let got: Vec<String> = collector.metrics().into_iter().map(|m| m.name).collect();
        METRICS.iter().all(|m| got.iter().any(|g| g == m))
    })
    .await;
    let got: Vec<String> = collector.metrics().into_iter().map(|m| m.name).collect();
    assert!(metrics_arrived, "metrics received: {got:?}");
    for rm in collector
        .metric_requests()
        .iter()
        .flat_map(|r| &r.resource_metrics)
    {
        let attrs = rm.resource.clone().unwrap_or_default().attributes;
        assert_eq!(
            string_attr(&attrs, "service.name").as_deref(),
            Some("macp-runtime")
        );
    }

    let status = interrupt_and_wait(&mut child, Duration::from_secs(10));
    assert!(status.is_some(), "runtime did not exit on SIGINT");
}

/// EAFNOSUPPORT: the host has no IPv6 (Linux 97, macOS/BSD 47).
#[cfg(target_os = "linux")]
const EAFNOSUPPORT: i32 = 97;
#[cfg(not(target_os = "linux"))]
const EAFNOSUPPORT: i32 = 47;

/// Whether a bind error means "this address cannot be watched on this host"
/// (taken, or no such loopback) rather than a test failure.
fn unbindable(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::AddrInUse | ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
    ) || e.raw_os_error() == Some(EAFNOSUPPORT)
}

/// A listener on one OTLP default address, counting every connection it
/// accepts until stopped.
struct PortWatch {
    addr: &'static str,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl PortWatch {
    /// Bind `addr`, or print a skip notice and return `None` if the address
    /// is taken or unavailable on this host (e.g. no IPv6 loopback).
    fn bind(addr: &'static str) -> Option<Self> {
        let bound = {
            let _guard = binary_child::socket_guard();
            TcpListener::bind(addr)
        };
        let listener = match bound {
            Ok(l) => l,
            Err(e) if unbindable(&e) => {
                eprintln!(
                    "SKIPPED {addr} in no_otel_vars_dials_nothing_on_the_default_otlp_ports: \
                     cannot bind it on this host ({e}); this address proved nothing"
                );
                return None;
            }
            Err(e) => panic!("bind {addr}: {e}"),
        };
        listener.set_nonblocking(true).unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let accepted = accepted.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok(_) => {
                            accepted.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => {}
                    }
                }
                // One last drain, so a connection that raced `stop` still counts.
                while listener.accept().is_ok() {
                    accepted.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        Some(Self {
            addr,
            accepted,
            stop,
            thread,
        })
    }

    /// Stop the listener and return how many connections it accepted.
    fn finish(self) -> usize {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap();
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Case 2: with no `OTEL_*` variable, listeners on the literal OTLP default
/// addresses (`127.0.0.1:4317`, `[::1]:4317`, and the OTLP/HTTP port
/// `127.0.0.1:4318`) see zero connections across a scripted session and a
/// graceful shutdown. A listener on a random port would prove nothing (no
/// code would dial it), so an address that cannot be bound is skipped, loudly,
/// and the case is skipped if none can.
#[tokio::test(flavor = "multi_thread")]
async fn no_otel_vars_dials_nothing_on_the_default_otlp_ports() {
    let watches: Vec<PortWatch> = ["127.0.0.1:4317", "[::1]:4317", "127.0.0.1:4318"]
        .into_iter()
        .filter_map(PortWatch::bind)
        .collect();
    if watches.is_empty() {
        eprintln!(
            "SKIPPED no_otel_vars_dials_nothing_on_the_default_otlp_ports: \
             no OTLP default address could be bound; this case proved nothing"
        );
        return;
    }

    let scratch = tempfile::tempdir().unwrap();
    let (mut child, addr) = start_runtime(scratch.path(), &[]).await;
    drive_session(&addr, true).await;
    let status = interrupt_and_wait(&mut child, Duration::from_secs(10));
    assert!(status.is_some(), "runtime did not exit on SIGINT");
    let output = child.output();

    for watch in watches {
        let addr = watch.addr;
        let accepted = watch.finish();
        assert_eq!(
            accepted, 0,
            "something dialed {addr} with no OTEL vars; output:\n{output}"
        );
    }
    assert!(
        !output.contains("OpenTelemetry enabled"),
        "telemetry must stay inactive; output:\n{output}"
    );
}

/// Case 3: spans pending in the batch processor are flushed on SIGINT within
/// the 5 s shutdown bound. The schedule delay is a minute, so nothing is
/// exported before the signal: whatever the collector holds afterwards came
/// from the shutdown flush.
#[tokio::test(flavor = "multi_thread")]
async fn sigint_flushes_pending_spans_within_the_bound() {
    let collector = FakeCollector::start().await;
    let scratch = tempfile::tempdir().unwrap();
    let (mut child, addr) = start_runtime(
        scratch.path(),
        &[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint()),
            ("OTEL_BSP_SCHEDULE_DELAY", "60000".to_string()),
            ("OTEL_METRIC_EXPORT_INTERVAL", "60000".to_string()),
        ],
    )
    .await;
    drive_session(&addr, false).await;
    // Give an (incorrectly) eager exporter a chance to show itself.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        collector.spans().is_empty(),
        "spans exported before SIGINT; the flush below would prove nothing"
    );

    let signalled = Instant::now();
    let status = interrupt_and_wait(&mut child, Duration::from_secs(10));
    let exited_after = signalled.elapsed();
    let output = child.output();
    assert!(
        status.is_some(),
        "runtime did not exit on SIGINT:\n{output}"
    );
    let flushed = eventually(
        Duration::from_secs(5).saturating_sub(signalled.elapsed()),
        || {
            let spans = collector.spans();
            spans.iter().any(|s| s.name == PROCESS_SPAN)
                && spans.iter().any(|s| s.name == SEND_SPAN)
        },
    )
    .await;
    let names: Vec<String> = collector.spans().into_iter().map(|s| s.name).collect();
    assert!(
        flushed,
        "final spans not at the collector within 5 s of SIGINT (exit after \
         {exited_after:?}); received {names:?}; output:\n{output}"
    );
    assert!(
        exited_after <= Duration::from_secs(5),
        "shutdown took {exited_after:?}; output:\n{output}"
    );
}
