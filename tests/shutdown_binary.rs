//! Real-binary graceful-shutdown checks (http-listener P1b): one shared
//! shutdown signal, fed by SIGINT **or** SIGTERM, stops the gRPC server, the
//! Prometheus listener and arms the `MACP_SHUTDOWN_DRAIN_SECS` deadline.
//!
//! The in-flight work is a real unary `Send` held open deterministically: it
//! is issued through `tonic::client::Grpc::client_streaming` against the unary
//! path, so the HTTP/2 stream (headers) reaches the server at once but the one
//! request message is only written when the test opens a gate. The server's
//! unary handler is awaiting that message, so the request is genuinely in
//! flight for as long as the test wants. A second RPC on the same connection
//! completing proves the server has read the held stream's HEADERS frame
//! (frames on one HTTP/2 connection are processed in order).
//!
//! "The signal has been handled" is observed, not slept for: the metrics port
//! (always enabled here) closes when the shared signal fires.
//!
//! Signals are delivered with `kill`, so the file is Unix-only.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use macp_runtime::pb::{Envelope, SendRequest, SendResponse, SessionStartPayload};
use prost::Message;
use tokio::sync::oneshot;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::Channel;

const SID: &str = "6f1c2d3e-4b5a-4c6d-8e7f-9a0b1c2d3e4f";

/// Kills the child if a test panics before it exits.
struct Child(std::process::Child);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Child {
    fn signal(&self, sig: &str) {
        let status = Command::new("kill")
            .args([&format!("-{sig}"), &self.0.id().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{sig} failed");
    }

    fn alive(&mut self) -> bool {
        self.0.try_wait().unwrap().is_none()
    }

    /// Wait (bounded) for exit; `None` if still running after `limit`.
    fn wait_exit(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Some(status) = self.0.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Combined stdout + stderr. Call after the process exited.
    fn output(&mut self) -> String {
        let mut out = String::new();
        if let Some(mut s) = self.0.stdout.take() {
            s.read_to_string(&mut out).ok();
        }
        if let Some(mut s) = self.0.stderr.take() {
            s.read_to_string(&mut out).ok();
        }
        out
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Runtime {
    child: Child,
    grpc: String,
    metrics: String,
    _scratch: tempfile::TempDir,
}

/// Spawn the binary in dev mode with a scrubbed environment, the metrics
/// endpoint on, and the given drain deadline; return once both gRPC (health
/// SERVING) and the metrics port answer.
async fn start(drain_secs: u64) -> Runtime {
    let scratch = tempfile::tempdir().unwrap();
    let grpc = format!("127.0.0.1:{}", free_port());
    let metrics = format!("127.0.0.1:{}", free_port());
    let mut command = Command::new(env!("CARGO_BIN_EXE_macp-runtime"));
    command.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    command
        .current_dir(scratch.path())
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", &grpc)
        .env("MACP_METRICS_ADDR", &metrics)
        .env("MACP_SHUTDOWN_DRAIN_SECS", drain_secs.to_string())
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut rt = Runtime {
        child: Child(command.spawn().expect("spawn macp-runtime")),
        grpc,
        metrics,
        _scratch: scratch,
    };
    wait_ready(&mut rt).await;
    rt
}

/// One `GET /metrics`; true on a 200 response.
fn metrics_answers(addr: &str) -> bool {
    let Ok(mut sock) =
        std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500))
    else {
        return false;
    };
    sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
    if sock
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 12];
    sock.read_exact(&mut head).is_ok() && head.starts_with(b"HTTP/1.1 200")
}

/// True when a TCP connect to `addr` is refused.
fn refuses(addr: &str) -> bool {
    std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500))
        .is_err()
}

async fn wait_ready(rt: &mut Runtime) {
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut grpc_ok = false;
    while Instant::now() < deadline {
        if !rt.child.alive() {
            panic!(
                "runtime exited before ready; output:\n{}",
                rt.child.output()
            );
        }
        if !grpc_ok {
            if let Ok(channel) = connect(&rt.grpc).await {
                grpc_ok = matches!(
                    HealthClient::new(channel)
                        .check(HealthCheckRequest { service: String::new() })
                        .await,
                    Ok(r) if r.get_ref().status == ServingStatus::Serving as i32
                );
            }
        }
        if grpc_ok && metrics_answers(&rt.metrics) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = rt.child.0.kill();
    let _ = rt.child.0.wait();
    panic!("runtime never became ready; output:\n{}", rt.child.output());
}

async fn connect(addr: &str) -> Result<Channel, tonic::transport::Error> {
    tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(60))
        .connect()
        .await
}

fn session_start() -> SendRequest {
    SendRequest {
        envelope: Some(Envelope {
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
        }),
    }
}

/// A unary `Send` held in flight until `release` fires.
struct HeldSend {
    release: Option<oneshot::Sender<()>>,
    response: tokio::task::JoinHandle<Result<SendResponse, tonic::Status>>,
}

/// Start a held unary `Send` and return once the server has read its HEADERS.
async fn hold_send(addr: &str) -> HeldSend {
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;
    let channel = connect(addr).await.expect("connect");
    let (polled_tx, polled_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let body = async_stream::stream! {
        let _ = polled_tx.send(());
        let _ = release_rx.await;
        yield session_start();
    };
    let mut req = tonic::Request::new(body);
    req.metadata_mut()
        .insert("authorization", "Bearer agent://a".parse().unwrap());
    let mut grpc = tonic::client::Grpc::new(channel.clone());
    let response = tokio::spawn(async move {
        grpc.ready()
            .await
            .map_err(|e| tonic::Status::unknown(e.to_string()))?;
        grpc.client_streaming(
            req,
            PathAndQuery::from_static("/macp.v1.MACPRuntimeService/Send"),
            tonic_prost::ProstCodec::<SendRequest, SendResponse>::default(),
        )
        .await
        .map(|r| r.into_inner())
    });
    tokio::time::timeout(Duration::from_secs(10), polled_rx)
        .await
        .expect("request body never polled")
        .expect("request task died");
    // Same connection, later frame: once this answers, the held stream's
    // HEADERS frame has been read by the server.
    HealthClient::new(channel)
        .check(HealthCheckRequest {
            service: String::new(),
        })
        .await
        .expect("health check behind the held request");
    HeldSend {
        release: Some(release_tx),
        response,
    }
}

/// Poll until the metrics port refuses connections (the shared signal fired),
/// while the process is still alive.
async fn wait_metrics_closed(rt: &mut Runtime) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if refuses(&rt.metrics) {
            return;
        }
        assert!(rt.child.alive(), "runtime exited before metrics closed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("metrics port still accepting 10 s after the signal");
}

/// Signal with a held unary request: the signal is handled (metrics closes)
/// while the request is still open, the request then completes normally, and
/// the process exits 0 well within the drain deadline.
async fn drains_inflight_unary(sig: &str) {
    const DRAIN: u64 = 5;
    let mut rt = start(DRAIN).await;
    let mut held = hold_send(&rt.grpc).await;

    let signalled = Instant::now();
    rt.child.signal(sig);
    wait_metrics_closed(&mut rt).await;
    assert!(rt.child.alive(), "exited with a request in flight");

    held.release.take().unwrap().send(()).unwrap();
    let resp = tokio::time::timeout(Duration::from_secs(5), held.response)
        .await
        .expect("held request never answered")
        .unwrap()
        .expect("held request failed during drain");
    let ack = resp.ack.expect("ack");
    assert!(ack.ok, "{ack:?}");

    let status = rt
        .child
        .wait_exit(Duration::from_secs(DRAIN))
        .unwrap_or_else(|| panic!("did not exit within {DRAIN} s of {sig}"));
    let out = rt.child.output();
    assert!(status.success(), "{sig}: exit {status}; output:\n{out}");
    assert!(signalled.elapsed() < Duration::from_secs(DRAIN), "{out}");
    // The log line carries the signal name (ANSI-coloured, so match the value).
    assert!(
        out.contains(&format!("\"SIG{sig}\"")),
        "signal not logged:\n{out}"
    );
    assert!(out.contains("all connections drained"), "{out}");
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_drains_inflight_unary_request() {
    drains_inflight_unary("TERM").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_drains_inflight_unary_request() {
    drains_inflight_unary("INT").await;
}

/// A request still open at the deadline: the metrics port stops accepting
/// right after the signal while the process keeps draining, then the process
/// exits at the deadline (not before, not long after).
#[tokio::test(flavor = "multi_thread")]
async fn request_open_at_deadline_exits_at_deadline_and_metrics_stops() {
    const DRAIN: u64 = 2;
    let mut rt = start(DRAIN).await;
    let _held = hold_send(&rt.grpc).await; // never released

    let signalled = Instant::now();
    rt.child.signal("TERM");
    wait_metrics_closed(&mut rt).await;
    assert!(rt.child.alive());

    let status = rt
        .child
        .wait_exit(Duration::from_secs(DRAIN + 5))
        .expect("did not exit after the drain deadline");
    let elapsed = signalled.elapsed();
    let out = rt.child.output();
    assert!(status.success(), "exit {status}; output:\n{out}");
    assert!(
        elapsed >= Duration::from_millis(DRAIN * 1000 - 100),
        "exited before the deadline ({elapsed:?}):\n{out}"
    );
    assert!(out.contains("drain deadline reached"), "{out}");
}

/// A second signal during the drain exits immediately (128 + SIGTERM),
/// without waiting for the deadline.
#[tokio::test(flavor = "multi_thread")]
async fn second_signal_during_drain_exits_immediately() {
    const DRAIN: u64 = 60;
    let mut rt = start(DRAIN).await;
    let _held = hold_send(&rt.grpc).await; // never released

    rt.child.signal("TERM");
    wait_metrics_closed(&mut rt).await;
    assert!(rt.child.alive());

    let second = Instant::now();
    rt.child.signal("TERM");
    let status = rt
        .child
        .wait_exit(Duration::from_secs(5))
        .expect("second signal did not exit the process");
    assert!(second.elapsed() < Duration::from_secs(5));
    let out = rt.child.output();
    assert_eq!(status.code(), Some(143), "output:\n{out}");
    assert!(out.contains("second shutdown signal"), "{out}");
}
