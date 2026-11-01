//! The HTTP SERVER span and `http.server.request.duration` (plan
//! `plans/http-listener.md` P9), features `otel` and `http-listener`.
//!
//! Every test drives a real `HttpListener` on `127.0.0.1:0` over real TCP,
//! with spans and metrics going to in-memory exporters through
//! `telemetry::init::active_for_tests`, installed as the thread's default
//! dispatcher. Each test runs on tokio's current-thread runtime, so the
//! listener and connection tasks it spawns are polled on that thread and see
//! that dispatcher (the pattern of `tests/otel_grpc.rs`).
//!
//! The global text-map propagator is the W3C `TraceContextPropagator`, set
//! once per process, the value `telemetry::init::init` installs.
#![cfg(all(feature = "otel", feature = "http-listener"))]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use macp_runtime::http::{HttpConfig, HttpListener};
use macp_runtime::log_store::LogStore;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::telemetry::instruments::HTTP_REQUEST_DURATION_BOUNDARIES;
use macp_runtime::telemetry::{init, Telemetry};
use opentelemetry::trace::{SpanId, SpanKind, Status, TraceId};
use opentelemetry::Value as OtelValue;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const DECISION: &str = "macp.mode.decision.v1";
const ORCH: &str = "agent://orchestrator";
const PEER: &str = "agent://peer";
const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT_ID: &str = "00f067aa0ba902b7";
/// The bearer token: it must never appear in any span or metric.
const TOKEN: &str = "tok-SECRET-5e1f9c";
const TOKENS: &str = r#"[
  {"token": "tok-SECRET-5e1f9c", "sender": "agent://orchestrator"},
  {"token": "tok-peer", "sender": "agent://peer"}
]"#;
/// The attribute keys of every HTTP SERVER span, no more.
const SPAN_KEYS: [&str; 5] = [
    "http.request.method",
    "http.response.status_code",
    "http.route",
    "network.protocol.version",
    "url.scheme",
];

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// The in-memory span and metric pipeline of one test.
struct Pipeline {
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
    _guard: tracing::dispatcher::DefaultGuard,
}

impl Pipeline {
    fn install() -> (Self, Telemetry) {
        install_propagator();
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let metrics = InMemoryMetricExporter::default();
        let meter = SdkMeterProvider::builder()
            .with_periodic_exporter(metrics.clone())
            .build();
        let (dispatch, telemetry) = init::active_for_tests(&tracer, Some(&meter));
        let guard = tracing::dispatcher::set_default(&dispatch);
        (
            Self {
                spans,
                metrics,
                tracer,
                meter,
                _guard: guard,
            },
            telemetry,
        )
    }

    fn all(&self) -> Vec<SpanData> {
        self.tracer.force_flush().unwrap();
        self.spans.get_finished_spans().unwrap()
    }

    fn server(&self) -> Vec<SpanData> {
        self.all()
            .into_iter()
            .filter(|s| s.span_kind == SpanKind::Server)
            .collect()
    }

    /// Wait (yielding to the listener tasks) until a SERVER span named
    /// `name` has ended. A span ends when its response body does, a moment
    /// after the client has the response.
    async fn wait_for(&self, name: &str) -> SpanData {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(s) = self.server().into_iter().find(|s| s.name == name) {
                return s;
            }
            assert!(Instant::now() < deadline, "no SERVER span {name:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

struct Rig {
    addr: SocketAddr,
}

async fn rig(telemetry: Telemetry, keepalive: Duration) -> Rig {
    let runtime = Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_telemetry(telemetry);
    let security = SecurityLayer::dev_mode()
        .with_static_tokens(TOKENS)
        .unwrap();
    let server = MacpServer::new(Arc::new(runtime), security);
    let env: BTreeMap<&str, &str> = BTreeMap::from([
        ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0"),
        ("MACP_ALLOW_INSECURE", "1"),
        ("MACP_REQUEST_TIMEOUT_SECS", "1"),
    ]);
    let config = HttpConfig::from_env(&move |k| env.get(k).map(|v| v.to_string()))
        .unwrap()
        .unwrap();
    let listener = HttpListener::bind(&config, server)
        .await
        .unwrap()
        .with_sse_keepalive(keepalive);
    let addr = listener.local_addr().unwrap();
    tokio::spawn(listener.serve(std::future::pending()));
    Rig { addr }
}

impl Rig {
    /// One request on a fresh HTTP/1.1 connection; the connection task's
    /// handle comes back so a test can cut the connection.
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (http::Response<Incoming>, tokio::task::JoinHandle<()>) {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        let (mut send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
        let conn = tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut b = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b.body(Full::new(Bytes::copy_from_slice(body))).unwrap();
        (send.send_request(req).await.unwrap(), conn)
    }

    async fn rpc(&self, method: &str, headers: &[(&str, &str)], body: &Value) -> (u16, Value) {
        let mut all = vec![("content-type", "application/json")];
        all.extend_from_slice(headers);
        let (resp, _conn) = self
            .request(
                "POST",
                &format!("/macp/v1/{method}"),
                &all,
                &serde_json::to_vec(body).unwrap(),
            )
            .await;
        let status = resp.status().as_u16();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

fn start(sid: &str, intent: &str) -> Value {
    json!({"envelope": {
        "macp_version": "1.0",
        "mode": DECISION,
        "message_type": "SessionStart",
        "message_id": format!("{sid}-start"),
        "session_id": sid,
        "sender": "",
        "payload": {
            "intent": intent,
            "participants": [ORCH, PEER],
            "mode_version": "1.0.0",
            "configuration_version": "config.default",
            "policy_version": "policy.default",
            "ttl_ms": 600000,
        },
    }})
}

fn attrs(span: &SpanData) -> BTreeMap<String, OtelValue> {
    span.attributes
        .iter()
        .map(|kv| (kv.key.to_string(), kv.value.clone()))
        .collect()
}

const BEARER: &str = "Bearer tok-SECRET-5e1f9c";

// A client `traceparent` parents the HTTP SERVER span, whose name and
// attributes follow the HTTP semantic conventions; `process` is its child.
#[tokio::test]
async fn client_traceparent_parents_the_span_and_process_is_its_child() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    let traceparent = format!("00-{TRACE_ID}-{PARENT_ID}-01");
    let sid = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f60";
    let (status, ack) = rig
        .rpc(
            "Send",
            &[("authorization", BEARER), ("traceparent", &traceparent)],
            &start(sid, "parenting"),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(ack["ack"]["ok"], true, "{ack}");

    let server = pipeline.wait_for("POST /macp/v1/Send").await;
    assert_eq!(server.span_kind, SpanKind::Server);
    assert_eq!(
        server.span_context.trace_id(),
        TraceId::from_hex(TRACE_ID).unwrap()
    );
    assert_eq!(server.parent_span_id, SpanId::from_hex(PARENT_ID).unwrap());
    assert_eq!(server.status, Status::Unset);
    let a = attrs(&server);
    assert_eq!(
        a.keys().map(String::as_str).collect::<Vec<_>>(),
        SPAN_KEYS.to_vec()
    );
    assert_eq!(a["http.request.method"], OtelValue::from("POST"));
    assert_eq!(a["http.route"], OtelValue::from("/macp/v1/Send"));
    assert_eq!(a["url.scheme"], OtelValue::from("http"));
    assert_eq!(a["network.protocol.version"], OtelValue::from("1.1"));
    assert_eq!(a["http.response.status_code"], OtelValue::I64(200));

    let process: Vec<SpanData> = pipeline
        .all()
        .into_iter()
        .filter(|s| s.name.starts_with("process"))
        .collect();
    assert_eq!(process.len(), 1, "{process:?}");
    assert_eq!(process[0].span_kind, SpanKind::Internal);
    assert_eq!(process[0].parent_span_id, server.span_context.span_id());
    assert_eq!(
        process[0].span_context.trace_id(),
        server.span_context.trace_id()
    );
}

// Unknown routes are named by the method alone and carry no `http.route`;
// so do preflights; a non-RFC 9110 method is `_OTHER`, named `HTTP`; the
// well-known path is a route. None of these is an ERROR.
#[tokio::test]
async fn names_stay_bounded_and_only_matched_routes_carry_a_route() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    let ct = ("content-type", "application/json");

    let (resp, _c) = rig
        .request("POST", "/macp/v1/NoSuchMethod-a1b2", &[ct], b"{}")
        .await;
    assert_eq!(resp.status(), 404);
    let (resp, _c) = rig
        .request("POST", "/some/other/path-c3d4", &[ct], b"{}")
        .await;
    assert_eq!(resp.status(), 404);
    let (resp, _c) = rig
        .request("OPTIONS", "/macp/v1/Send", &[("origin", "https://x")], b"")
        .await;
    assert_eq!(resp.status(), 204);
    let (resp, _c) = rig
        .request("FROBNICATE", "/macp/v1/Send", &[ct], b"{}")
        .await;
    assert_eq!(resp.status(), 405);
    let (resp, _c) = rig.request("GET", "/.well-known/macp.json", &[], b"").await;
    assert_eq!(resp.status(), 200);
    drop(resp.into_body().collect().await);
    // Unauthenticated: a 4xx, not an ERROR.
    let (status, _) = rig.rpc("ListSessions", &[], &json!({})).await;
    assert_eq!(status, 401);

    let deadline = Instant::now() + Duration::from_secs(5);
    let spans = loop {
        let spans = pipeline.server();
        if spans.len() >= 6 || Instant::now() > deadline {
            break spans;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let mut names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "GET /.well-known/macp.json",
            "HTTP /macp/v1/Send",
            "OPTIONS",
            "POST",
            "POST",
            "POST /macp/v1/ListSessions",
        ]
    );
    for s in &spans {
        assert_eq!(s.status, Status::Unset, "{}", s.name);
        assert_eq!(s.parent_span_id, SpanId::INVALID, "{}", s.name);
        let a = attrs(s);
        let has_route = !matches!(s.name.as_ref(), "POST" | "OPTIONS");
        assert_eq!(a.contains_key("http.route"), has_route, "{}", s.name);
        for (k, v) in &a {
            // `error.type` is added only on a 5xx.
            assert!(
                SPAN_KEYS.contains(&k.as_str()) || k == "error.type",
                "{k} on {}",
                s.name
            );
            let v = v.as_str();
            assert!(!v.contains("NoSuchMethod"), "{k}={v}");
            assert!(!v.contains("other/path"), "{k}={v}");
            assert!(!v.contains("FROBNICATE"), "{k}={v}");
        }
        let expected_status = match s.name.as_ref() {
            "POST" => 404,
            "OPTIONS" => 204,
            "HTTP /macp/v1/Send" => 405,
            "GET /.well-known/macp.json" => 200,
            _ => 401,
        };
        assert_eq!(
            a["http.response.status_code"],
            OtelValue::I64(expected_status),
            "{}",
            s.name
        );
    }
    let other = spans
        .iter()
        .find(|s| s.name == "HTTP /macp/v1/Send")
        .unwrap();
    assert_eq!(
        attrs(other)["http.request.method"],
        OtelValue::from("_OTHER")
    );
}

// A `5xx` (here the request timeout's `500`) sets ERROR, with no
// description.
#[tokio::test]
async fn only_a_5xx_is_an_error() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    // A body that never arrives: only the 1 s request timeout answers.
    let mut tcp = TcpStream::connect(rig.addr).await.unwrap();
    tcp.write_all(
        b"POST /macp/v1/ListModes HTTP/1.1\r\nhost: x\r\ncontent-type: application/json\r\n\
          content-length: 64\r\n\r\n{",
    )
    .await
    .unwrap();
    let mut buf = vec![0u8; 1024];
    let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert!(
        buf[..n].starts_with(b"HTTP/1.1 500"),
        "{}",
        String::from_utf8_lossy(&buf[..n])
    );
    let span = pipeline.wait_for("POST /macp/v1/ListModes").await;
    assert_eq!(span.status, Status::error(""));
    assert_eq!(
        attrs(&span)["http.response.status_code"],
        OtelValue::I64(500)
    );
    // HTTP semconv: the status code is `error.type` on a 5xx.
    assert_eq!(attrs(&span)["error.type"], OtelValue::from("500"));

    // The same RPC answered `200` is not an ERROR.
    drop(tcp);
    let (status, _) = rig.rpc("ListModes", &[], &json!({})).await;
    assert_eq!(status, 200);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let ok: Vec<_> = pipeline
            .server()
            .into_iter()
            .filter(|s| attrs(s)["http.response.status_code"] == OtelValue::I64(200))
            .collect();
        if let Some(s) = ok.first() {
            assert_eq!(s.status, Status::Unset);
            assert!(!attrs(s).contains_key("error.type"));
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// An SSE stream's span stays open while the stream is, and ends when it
// ends: normally (the session is cancelled: `event: error`, then close) and
// when the client goes away.
#[tokio::test]
async fn an_sse_span_lasts_until_its_stream_ends() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_millis(50)).await;
    let sid = "7d1e2f3a-4b5c-4d6e-8f70-112233445566";
    let (status, ack) = rig
        .rpc("Send", &[("authorization", BEARER)], &start(sid, "sse"))
        .await;
    assert_eq!((status, ack["ack"]["ok"].clone()), (200, json!(true)));

    // Ends normally.
    let (resp, _conn) = rig
        .request(
            "POST",
            "/macp/v1/StreamSession",
            &[
                ("content-type", "application/json"),
                ("accept", "text/event-stream"),
                ("authorization", BEARER),
            ],
            json!({"subscribe_session_id": sid}).to_string().as_bytes(),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let mut body = resp.into_body();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let open = |p: &Pipeline, name: &str| p.server().iter().all(|s| s.name != name);
    assert!(open(&pipeline, "POST /macp/v1/StreamSession"));
    let (status, _) = rig
        .rpc(
            "CancelSession",
            &[("authorization", BEARER)],
            &json!({"session_id": sid, "reason": "done"}),
        )
        .await;
    assert_eq!(status, 200);
    let read = async {
        while let Some(frame) = body.frame().await {
            frame.unwrap();
        }
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("the stream ends after the cancel");
    let span = pipeline.wait_for("POST /macp/v1/StreamSession").await;
    let lived = span.end_time.duration_since(span.start_time).unwrap();
    assert!(lived >= Duration::from_millis(200), "{lived:?}");
    assert_eq!(span.status, Status::Unset);

    // Ends when the client goes away (the next keepalive write fails).
    let (resp, conn) = rig
        .request(
            "POST",
            "/macp/v1/WatchSignals",
            &[
                ("content-type", "application/json"),
                ("accept", "text/event-stream"),
                ("authorization", BEARER),
            ],
            b"{}",
        )
        .await;
    assert_eq!(resp.status(), 200);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(open(&pipeline, "POST /macp/v1/WatchSignals"));
    drop(resp);
    conn.abort();
    // Still open 200 ms in (above), ended once the connection is gone. Its
    // recorded end time is its last exit (tracing-opentelemetry), the last
    // keepalive poll, so no lower bound is asserted on it here.
    let span = pipeline.wait_for("POST /macp/v1/WatchSignals").await;
    assert_eq!(span.status, Status::Unset);
}

// A malformed `traceparent` starts a new root and is never an error.
#[tokio::test]
async fn a_malformed_traceparent_starts_a_new_root() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    let bad = [
        "00-zzzz-bad-01",
        "00-00000000000000000000000000000000-0000000000000000-01",
        "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
        "",
    ];
    for tp in bad {
        let (status, _) = rig
            .rpc("ListModes", &[("traceparent", tp)], &json!({}))
            .await;
        assert_eq!(status, 200, "{tp:?}");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let spans = loop {
        let spans = pipeline.server();
        if spans.len() >= bad.len() || Instant::now() > deadline {
            break spans;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(spans.len(), bad.len());
    let mut traces = BTreeSet::new();
    for s in &spans {
        assert_eq!(s.parent_span_id, SpanId::INVALID);
        assert_eq!(s.status, Status::Unset);
        assert_ne!(s.span_context.trace_id(), TraceId::INVALID);
        traces.insert(s.span_context.trace_id().to_string());
    }
    assert_eq!(traces.len(), bad.len(), "each a new trace");
}

// No span (SERVER or `process`) and no metric point carries a header value,
// the token, the query, the body or the payload.
#[tokio::test]
async fn nothing_leaks_headers_tokens_or_bodies() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    let sid = "5a6b7c8d-9e0f-4a1b-8c2d-3e4f5a6b7c8d";
    let sentinels = [
        TOKEN,
        "SENTINEL-HEADER-77aa",
        "SENTINEL-STATE-88bb",
        "SENTINEL-QUERY-99cc",
        "SENTINEL-INTENT-00dd",
        "SENTINEL-UA-11ee",
        "SENTINEL-ORIGIN-22ff",
    ];
    let body = serde_json::to_vec(&start(sid, "SENTINEL-INTENT-00dd")).unwrap();
    let (resp, _c) = rig
        .request(
            "POST",
            "/macp/v1/Send?q=SENTINEL-QUERY-99cc",
            &[
                ("content-type", "application/json"),
                ("authorization", BEARER),
                ("x-sentinel", "SENTINEL-HEADER-77aa"),
                ("user-agent", "SENTINEL-UA-11ee"),
                ("origin", "https://SENTINEL-ORIGIN-22ff"),
                ("traceparent", &format!("00-{TRACE_ID}-{PARENT_ID}-01")),
                ("tracestate", "k=SENTINEL-STATE-88bb"),
            ],
            &body,
        )
        .await;
    assert_eq!(resp.status(), 200);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let ack: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(ack["ack"]["ok"], true, "{ack}");
    // A refused request too: a 401 with a wrong token.
    let (status, _) = rig
        .rpc(
            "ListSessions",
            &[("authorization", "Bearer SENTINEL-HEADER-77aa")],
            &json!({}),
        )
        .await;
    assert_eq!(status, 401);

    pipeline.wait_for("POST /macp/v1/Send").await;
    pipeline.wait_for("POST /macp/v1/ListSessions").await;
    let spans = pipeline.all();
    assert!(spans.len() >= 3, "{spans:?}");
    for s in &spans {
        let mut texts = vec![s.name.to_string(), format!("{:?}", s.status)];
        for kv in &s.attributes {
            texts.push(kv.key.to_string());
            texts.push(kv.value.as_str().into_owned());
        }
        for e in s.events.iter() {
            texts.push(e.name.to_string());
            for kv in &e.attributes {
                texts.push(kv.value.as_str().into_owned());
            }
        }
        for text in &texts {
            for sentinel in sentinels {
                assert!(
                    !text.contains(sentinel),
                    "{sentinel} in {:?}: {text}",
                    s.name
                );
            }
        }
    }
    for s in spans.iter().filter(|s| s.span_kind == SpanKind::Server) {
        for k in attrs(s).keys() {
            assert!(SPAN_KEYS.contains(&k.as_str()), "{k}");
        }
    }

    pipeline.meter.force_flush().unwrap();
    let dump = format!("{:?}", pipeline.metrics.get_finished_metrics().unwrap());
    for sentinel in sentinels {
        assert!(!dump.contains(sentinel), "{sentinel} in metrics");
    }
}

// `http.server.request.duration`: the HTTP semantic conventions' name, unit
// and attributes, with explicit boundaries.
#[tokio::test]
async fn the_request_duration_instrument() {
    let (pipeline, telemetry) = Pipeline::install();
    let rig = rig(telemetry, Duration::from_secs(30)).await;
    let (status, _) = rig.rpc("ListModes", &[], &json!({})).await;
    assert_eq!(status, 200);
    let (resp, _c) = rig
        .request(
            "POST",
            "/nowhere",
            &[("content-type", "application/json")],
            b"{}",
        )
        .await;
    assert_eq!(resp.status(), 404);
    pipeline.wait_for("POST /macp/v1/ListModes").await;
    pipeline.wait_for("POST").await;

    pipeline.meter.force_flush().unwrap();
    let rm = pipeline
        .metrics
        .get_finished_metrics()
        .unwrap()
        .pop()
        .expect("one collection");
    let metric = rm
        .scope_metrics()
        .flat_map(|sm| sm.metrics())
        .find(|m| m.name() == "http.server.request.duration")
        .expect("http.server.request.duration");
    assert_eq!(metric.unit(), "s");
    let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data() else {
        panic!("not an f64 histogram: {:?}", metric.data());
    };
    let mut points = Vec::new();
    for p in h.data_points() {
        assert_eq!(
            p.bounds().collect::<Vec<_>>(),
            HTTP_REQUEST_DURATION_BOUNDARIES
        );
        let a: BTreeMap<String, String> = p
            .attributes()
            .map(|kv| (kv.key.to_string(), kv.value.as_str().into_owned()))
            .collect();
        points.push((a, p.count()));
    }
    points.sort();
    let kv = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    };
    let mut expected = vec![
        (
            kv(&[
                ("http.request.method", "POST"),
                ("http.response.status_code", "404"),
                ("url.scheme", "http"),
            ]),
            1,
        ),
        (
            kv(&[
                ("http.request.method", "POST"),
                ("http.response.status_code", "200"),
                ("http.route", "/macp/v1/ListModes"),
                ("url.scheme", "http"),
            ]),
            1,
        ),
    ];
    expected.sort();
    assert_eq!(points, expected);
}
