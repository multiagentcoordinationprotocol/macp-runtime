//! The streaming RPCs of the HTTP binding as server-sent events
//! (RFC-MACP-0006 §4.3), plan `plans/http-listener.md` P6,
//! `http-listener` feature.
//!
//! Every test drives a real `HttpListener` on `127.0.0.1:0` in this process
//! over real TCP (HTTP/1.1, and HTTP/2 for the induced lag), sharing one
//! runtime the test can also drive directly. Static tokens come from
//! `SecurityLayer::with_static_tokens`, so the tests run in parallel. The
//! keepalive interval is shortened through the doc-hidden
//! `HttpListener::with_sse_keepalive`; nothing waits on a fixed sleep except
//! where the subject is time itself (the request timeout not covering a
//! stream), and every read is bounded.
//!
//! The bus/lifecycle race (a `Resolved` event before its Commitment, 200
//! times) and the exact error-event framing are unit tests in
//! `src/http/sse.rs`, behind a `#[cfg(test)]` hook.
#![cfg(feature = "http-listener")]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use macp_runtime::http::json::envelope_from_json;
use macp_runtime::http::{HttpConfig, HttpListener};
use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::ManualClock;
use macp_runtime::pb;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeService;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use serde_json::{json, Value};
use tokio::net::TcpStream;

const DECISION: &str = "macp.mode.decision.v1";
const ORCH: &str = "agent://orchestrator";
const SECURITY: &str = "agent://security";
const FINANCE: &str = "agent://finance";

const TOKENS: &str = r#"[
  {"token": "tok-orch", "sender": "agent://orchestrator"},
  {"token": "tok-security", "sender": "agent://security"},
  {"token": "tok-finance", "sender": "agent://finance"},
  {"token": "tok-outsider", "sender": "agent://outsider"},
  {"token": "tok-observer", "sender": "agent://observer", "is_observer": true},
  {"token": "tok-telemetry", "sender": "agent://telemetry.observer"}
]"#;

/// Every read on a stream must make progress within this.
const READ_WITHIN: Duration = Duration::from_secs(10);

fn root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root().join("tests/http/examples").join(name)).unwrap())
        .unwrap()
}

// ───────────────────────── rig ─────────────────────────

struct Opts {
    keepalive: Duration,
    /// Compact terminal sessions' logs (the runtime default).
    compact: bool,
    clock: Option<Arc<ManualClock>>,
    request_timeout_secs: u64,
    /// Dev-mode auth only (no configured tokens): any bearer value is its
    /// own sender, and senders count as unauthenticated (RFC-MACP-0004 §3).
    dev_auth: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            // Long enough that no keepalive interleaves a test that does not
            // look for one.
            keepalive: Duration::from_secs(3600),
            compact: false,
            clock: None,
            request_timeout_secs: 30,
            dev_auth: false,
        }
    }
}

struct Rig {
    http: SocketAddr,
    runtime: Arc<Runtime>,
    server: MacpServer,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    served: tokio::task::JoinHandle<()>,
    /// The file backend's directory, when compacting (the memory backend
    /// cannot compact).
    _dir: Option<tempfile::TempDir>,
}

async fn rig(opts: Opts) -> Rig {
    let security = if opts.dev_auth {
        SecurityLayer::dev_mode()
    } else {
        SecurityLayer::dev_mode()
            .with_static_tokens(TOKENS)
            .unwrap()
    };
    let dir = opts.compact.then(|| tempfile::tempdir().unwrap());
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = match &dir {
        Some(dir) => {
            Arc::new(macp_runtime::storage::FileBackend::new(dir.path().to_path_buf()).unwrap())
        }
        None => Arc::new(macp_runtime::storage::MemoryBackend),
    };
    let mut runtime = Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_compact_terminal_logs(opts.compact);
    if let Some(clock) = opts.clock {
        runtime = runtime.with_clock(clock);
    }
    let runtime = Arc::new(runtime);
    let server = MacpServer::new(Arc::clone(&runtime), security);
    let env: BTreeMap<&str, String> = BTreeMap::from([
        ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string()),
        ("MACP_ALLOW_INSECURE", "1".into()),
        (
            "MACP_REQUEST_TIMEOUT_SECS",
            opts.request_timeout_secs.to_string(),
        ),
    ]);
    let config = HttpConfig::from_env(&move |k| env.get(k).cloned())
        .unwrap()
        .unwrap();
    let listener = HttpListener::bind(&config, server.clone())
        .await
        .unwrap()
        .with_sse_keepalive(opts.keepalive);
    let http = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let served = tokio::spawn(listener.serve(async {
        let _ = rx.await;
    }));
    Rig {
        http,
        runtime,
        server,
        shutdown: Some(tx),
        served,
        _dir: dir,
    }
}

/// One SSE event, or a comment.
#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Data {
        id: Option<u64>,
        event: Option<String>,
        data: Value,
    },
    Comment(String),
}

impl Ev {
    fn id(&self) -> Option<u64> {
        match self {
            Ev::Data { id, .. } => *id,
            Ev::Comment(_) => None,
        }
    }

    fn data(&self) -> &Value {
        match self {
            Ev::Data { data, .. } => data,
            Ev::Comment(c) => panic!("comment {c:?}, not an event"),
        }
    }

    fn is_error(&self) -> bool {
        matches!(self, Ev::Data { event: Some(e), .. } if e == "error")
    }

    fn message_id(&self) -> String {
        self.data()["envelope"]["message_id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

/// A response: an event stream when `200`, else its body.
struct Sse {
    status: u16,
    headers: http::HeaderMap,
    body: Incoming,
    buf: Vec<u8>,
}

impl Sse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    /// The whole (non-stream) body as JSON, checked as a §4.5 error body.
    async fn error_json(self) -> Value {
        let status = self.status;
        let code_header = self.header("macp-error-code").map(str::to_string);
        assert_eq!(self.header("content-type"), Some("application/json"));
        let bytes = self.body.collect().await.unwrap().to_bytes();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        check_error(&v);
        let code = v["error"]["code"].as_str().unwrap();
        assert_eq!(code_header.as_deref(), Some(code));
        let entry = macp_runtime::macp_core::error::registry_entry(code).unwrap();
        assert_eq!(status, entry.http, "{v}");
        v
    }

    async fn body_bytes(self) -> Bytes {
        self.body.collect().await.unwrap().to_bytes()
    }

    /// The next event; `None` once the response ended (with nothing left
    /// over). Panics if nothing arrives within [`READ_WITHIN`].
    async fn next(&mut self) -> Option<Ev> {
        loop {
            if let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
                let block: Vec<u8> = self.buf.drain(..end + 2).collect();
                return Some(parse_block(&block[..end]));
            }
            let frame = tokio::time::timeout(READ_WITHIN, self.body.frame())
                .await
                .expect("no event within the read deadline");
            match frame {
                None => {
                    assert!(self.buf.is_empty(), "truncated event: {:?}", self.buf);
                    return None;
                }
                Some(frame) => {
                    if let Ok(data) = frame.expect("stream body error").into_data() {
                        self.buf.extend_from_slice(&data);
                    }
                }
            }
        }
    }

    /// The next non-comment event.
    async fn next_event(&mut self) -> Option<Ev> {
        loop {
            match self.next().await? {
                Ev::Comment(_) => continue,
                ev => return Some(ev),
            }
        }
    }

    /// Every remaining event (comments dropped) until the response ends.
    async fn rest(&mut self) -> Vec<Ev> {
        let mut out = Vec::new();
        while let Some(ev) = self.next_event().await {
            out.push(ev);
        }
        out
    }
}

/// One SSE block: `id:`, `event:` and a single `data:` line, or a comment.
/// No `retry:` field is ever sent; `data:` is one line of JSON.
fn parse_block(block: &[u8]) -> Ev {
    let text = std::str::from_utf8(block).unwrap();
    if let Some(comment) = text.strip_prefix(": ") {
        assert!(!comment.contains('\n'), "{text:?}");
        return Ev::Comment(comment.to_string());
    }
    let (mut id, mut event, mut data) = (None, None, None);
    for line in text.split('\n') {
        let (field, value) = line.split_once(": ").unwrap_or((line, ""));
        match field {
            "id" => id = Some(value.parse().unwrap()),
            "event" => event = Some(value.to_string()),
            "data" => {
                assert!(data.is_none(), "two data lines: {text:?}");
                data = Some(serde_json::from_str(value).unwrap());
            }
            other => panic!("unexpected SSE field {other:?} in {text:?}"),
        }
    }
    let data: Value = data.unwrap_or_else(|| panic!("no data: {text:?}"));
    if event.as_deref() == Some("error") {
        assert!(id.is_none(), "an error event carries no id");
        check_error(&data);
    }
    Ev::Data { id, event, data }
}

/// `macp-http-error.schema.json`'s shape (the full check is pinned in
/// `tests/http_unary.rs`): `error` with string members and `message_id: ""`.
fn check_error(v: &Value) {
    let error = v["error"].as_object().unwrap_or_else(|| panic!("{v}"));
    for member in ["code", "message", "session_id", "message_id", "details"] {
        assert!(error[member].is_string(), "{member}: {v}");
    }
    assert_eq!(error["message_id"], "");
    base64::engine::general_purpose::STANDARD
        .decode(error["details"].as_str().unwrap())
        .unwrap();
}

impl Rig {
    /// POST a streaming RPC on a fresh HTTP/1.1 connection.
    async fn open(&self, method: &str, headers: &[(&str, &str)], body: &Value) -> Sse {
        let tcp = TcpStream::connect(self.http).await.unwrap();
        let (mut send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
        tokio::spawn(conn);
        let mut b = http::Request::builder()
            .method("POST")
            .uri(format!("/macp/v1/{method}"))
            .header("host", "localhost")
            .header("content-type", "application/json");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let req = b
            .body(Full::new(Bytes::from(serde_json::to_vec(body).unwrap())))
            .unwrap();
        let resp = send.send_request(req).await.unwrap();
        into_sse(resp)
    }

    /// `StreamSession` subscribe with `Accept: text/event-stream`.
    async fn subscribe(&self, token: &str, sid: &str, extra: &[(&str, &str)]) -> Sse {
        let auth = format!("Bearer {token}");
        let mut headers = vec![
            ("authorization", auth.as_str()),
            ("accept", "text/event-stream"),
        ];
        headers.extend_from_slice(extra);
        self.open(
            "StreamSession",
            &headers,
            &json!({"subscribe_session_id": sid}),
        )
        .await
    }

    /// A unary JSON call.
    async fn call(&self, method: &str, token: &str, body: &Value) -> Value {
        let auth = format!("Bearer {token}");
        let sse = self.open(method, &[("authorization", &auth)], body).await;
        assert_eq!(sse.status, 200);
        serde_json::from_slice(&sse.body_bytes().await).unwrap()
    }

    /// Submit over HTTP `Send`; must be accepted.
    async fn send(&self, token: &str, envelope: Value) {
        let v = self
            .call("Send", token, &json!({ "envelope": envelope }))
            .await;
        assert_eq!(v["ack"]["ok"], true, "{v}");
    }

    /// Accept in process (fast path for bulk), as `sender`.
    async fn accept(&self, sender: &str, envelope: Value) {
        let mut env = envelope_from_json(&envelope).unwrap();
        env.sender = sender.into();
        self.runtime.process(&env, None).await.expect("accepted");
    }

    /// The session's accepted envelopes in log (ordinal) order.
    async fn log_ids(&self, sid: &str) -> Vec<String> {
        self.runtime
            .get_session_envelopes_after(sid, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.message_id)
            .collect()
    }

    async fn cancel(&self, sid: &str) {
        let v = self
            .call(
                "CancelSession",
                "tok-orch",
                &json!({"session_id": sid, "reason": "done"}),
            )
            .await;
        assert!(v.is_object(), "{v}");
    }
}

fn into_sse(resp: http::Response<Incoming>) -> Sse {
    let (parts, body) = resp.into_parts();
    assert!(parts.headers.get("set-cookie").is_none());
    Sse {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
        buf: Vec::new(),
    }
}

/// The stream response headers of §4.3.
fn assert_stream(sse: &Sse) {
    assert_eq!(sse.status, 200);
    assert_eq!(sse.header("content-type"), Some("text/event-stream"));
    assert_eq!(sse.header("cache-control"), Some("no-store"));
}

// ───────────────────────── envelopes ─────────────────────────

fn envelope(kind: &str, message_id: &str, sid: &str, payload: Value) -> Value {
    json!({
        "macp_version": "1.0",
        "mode": DECISION,
        "message_type": kind,
        "message_id": message_id,
        "session_id": sid,
        "sender": "",
        "payload": payload,
    })
}

fn start(sid: &str) -> Value {
    envelope(
        "SessionStart",
        &format!("{sid}-start"),
        sid,
        json!({
            "intent": "http sse",
            "participants": participants(),
            "mode_version": "1.0.0",
            "configuration_version": "config.default",
            "policy_version": "policy.default",
            "ttl_ms": 600000,
        }),
    )
}

/// The three named agents and [`VOTERS`] more (bulk voters, accepted in
/// process, so that a session can carry hundreds of envelopes after its
/// first Vote, when Decision admits only Votes).
fn participants() -> Vec<String> {
    let mut all = vec![ORCH.to_string(), SECURITY.into(), FINANCE.into()];
    all.extend((0..VOTERS).map(voter));
    all
}

const VOTERS: usize = 20;

fn voter(i: usize) -> String {
    format!("agent://voter{i}")
}

fn proposal(sid: &str, id: &str) -> Value {
    envelope(
        "Proposal",
        id,
        sid,
        json!({"proposal_id": id, "option": "a", "rationale": "r", "supporting_data": ""}),
    )
}

fn vote(sid: &str, id: &str, proposal_id: &str) -> Value {
    envelope(
        "Vote",
        id,
        sid,
        json!({"proposal_id": proposal_id, "vote": "APPROVE", "reason": "ok"}),
    )
}

fn commitment(sid: &str) -> Value {
    envelope(
        "Commitment",
        "commit",
        sid,
        json!({
            "commitment_id": "c1", "action": "decision.selected", "authority_scope": "s",
            "reason": "r", "mode_version": "1.0.0", "policy_version": "policy.default",
            "configuration_version": "config.default", "outcome_positive": true
        }),
    )
}

fn sid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// A session with `n` ordinals: the SessionStart and `n - 1` proposals
/// (`p0`, `p1`, ...), accepted in process.
async fn session_with(rig: &Rig, n: u64) -> String {
    let s = sid();
    rig.accept(ORCH, start(&s)).await;
    for i in 0..n - 1 {
        rig.accept(SECURITY, proposal(&s, &format!("p{i}"))).await;
    }
    s
}

/// Resolve a session with a proposal `p0`: a Vote, then the Commitment.
async fn resolve(rig: &Rig, sid: &str) {
    rig.send("tok-finance", vote(sid, "v-final", "p0")).await;
    rig.send("tok-orch", commitment(sid)).await;
}

/// An example's event with timestamps dropped (re-stamped here).
fn without_timestamp(mut v: Value) -> Value {
    if let Some(env) = v.get_mut("envelope").and_then(Value::as_object_mut) {
        env.remove("timestamp");
    }
    v
}

// ───────────────────────── vendored examples ─────────────────────────

/// `stream_session_subscribe.json` exchange 1: resume with `Last-Event-ID:
/// 41` ⇒ id 42, keepalive, id 43, then `STREAM_LAGGED` for a consumer that
/// stopped reading. HTTP/2 with a 2 KiB stream window: once the client stops
/// reading, the server can send nothing more, hyper stops polling the body,
/// and the bus (capacity 256) drops envelopes for this receiver.
#[tokio::test]
async fn example_subscribe_resume_keepalive_and_lag() {
    let ex = example("stream_session_subscribe.json");
    let x = &ex["exchanges"][0];
    let want = x["response"]["events"].as_array().unwrap();
    let rig = rig(Opts {
        keepalive: Duration::from_millis(150),
        ..Opts::default()
    })
    .await;
    let s = session_with(&rig, 41).await;

    let tcp = TcpStream::connect(rig.http).await.unwrap();
    let (mut send, conn) = http2::Builder::new(TokioExecutor::new())
        .initial_stream_window_size(2048)
        .initial_connection_window_size(2048)
        .adaptive_window(false)
        .handshake(TokioIo::new(tcp))
        .await
        .unwrap();
    tokio::spawn(conn);
    let mut body = x["request"]["body"].clone();
    body["subscribe_session_id"] = json!(s);
    let mut req = http::Request::builder()
        .method("POST")
        .uri(format!("http://{}/macp/v1/StreamSession", rig.http))
        .header("authorization", "Bearer tok-observer")
        .header("content-type", "application/json");
    for (k, v) in x["request"]["headers"].as_object().unwrap() {
        if !k.eq_ignore_ascii_case("authorization") && !k.eq_ignore_ascii_case("content-type") {
            req = req.header(k.as_str(), v.as_str().unwrap());
        }
    }
    let req = req
        .body(Full::new(Bytes::from(serde_json::to_vec(&body).unwrap())))
        .unwrap();
    let mut sse = into_sse(send.send_request(req).await.unwrap());
    assert_stream(&sse);

    // 42: the example's Proposal, sent over HTTP by agent://security.
    let mut p = want[0]["data"]["envelope"].clone();
    p["session_id"] = json!(s);
    p.as_object_mut().unwrap().remove("timestamp");
    rig.send("tok-security", p).await;
    let ev = sse.next().await.unwrap();
    assert_eq!(ev.id(), Some(42));
    let mut expected = without_timestamp(want[0]["data"].clone());
    expected["envelope"]["session_id"] = json!(s);
    assert_eq!(without_timestamp(ev.data().clone()), expected);
    assert!(ev.data()["envelope"]["timestamp"].is_string());

    // Nothing carried for the interval: the keepalive comment.
    assert_eq!(sse.next().await.unwrap(), Ev::Comment("keepalive".into()));

    // 43: the example's Vote by agent://finance.
    let mut v = want[2]["data"]["envelope"].clone();
    v["session_id"] = json!(s);
    v.as_object_mut().unwrap().remove("timestamp");
    rig.send("tok-finance", v).await;
    let ev = sse.next_event().await.unwrap();
    assert_eq!(ev.id(), Some(43));
    let mut expected = without_timestamp(want[2]["data"].clone());
    expected["envelope"]["session_id"] = json!(s);
    assert_eq!(without_timestamp(ev.data().clone()), expected);

    // Stop reading; flood the session well past the bus capacity.
    for i in 0..400 {
        let p = format!("p{}", i / VOTERS);
        rig.accept(&voter(i % VOTERS), vote(&s, &format!("flood{i}"), &p))
            .await;
    }
    let rest = sse.rest().await;
    let last = rest.last().unwrap();
    assert!(last.is_error(), "{rest:?}");
    let error = &last.data()["error"];
    let mut want_error = want[3]["data"]["error"].clone();
    want_error["session_id"] = json!(s);
    want_error["message"] = error["message"].clone();
    assert_eq!(error, &want_error);
    // Everything before the error is contiguous from 44.
    let ids: Vec<u64> = rest.iter().filter_map(Ev::id).collect();
    assert_eq!(ids, (44..44 + ids.len() as u64).collect::<Vec<_>>());
    assert!(ids.len() < 400, "no lag observed: {} delivered", ids.len());
}

/// `stream_session_subscribe.json` exchanges 2 and 3: `409
/// HISTORY_UNAVAILABLE` with the exact details for a resume below the
/// compacted base (41 ordinals), and `400 INVALID_REQUEST` for a
/// conflicting `Last-Event-ID`, neither opening a stream.
#[tokio::test]
async fn example_history_unavailable_and_conflict() {
    let ex = example("stream_session_subscribe.json");
    let rig = rig(Opts {
        compact: true,
        ..Opts::default()
    })
    .await;
    let s = session_with(&rig, 41).await;
    rig.cancel(&s).await;

    for (i, x) in ex["exchanges"].as_array().unwrap()[1..].iter().enumerate() {
        let mut body = x["request"]["body"].clone();
        body["subscribe_session_id"] = json!(s);
        let mut headers = vec![("authorization", "Bearer tok-observer")];
        let hs = x["request"]["headers"].as_object().unwrap();
        for (k, v) in hs {
            if !k.eq_ignore_ascii_case("authorization") && !k.eq_ignore_ascii_case("content-type") {
                headers.push((k.as_str(), v.as_str().unwrap()));
            }
        }
        let sse = rig.open("StreamSession", &headers, &body).await;
        let want = &x["response"];
        assert_eq!(sse.status as u64, want["status"].as_u64().unwrap(), "{i}");
        let v = sse.error_json().await;
        let mut expected = want["body"].clone();
        expected["error"]["session_id"] = json!(s);
        if i == 0 {
            // The message names the base; the details are byte-exact.
            assert!(v["error"]["message"].as_str().unwrap().contains("41"));
            expected["error"]["message"] = v["error"]["message"].clone();
        }
        assert_eq!(v, expected, "{i}");
    }

    // At the base the subscribe opens: nothing to replay, and the session
    // already ended by cancel, so the stream ends with SESSION_NOT_OPEN.
    let mut sse = rig
        .subscribe("tok-observer", &s, &[("last-event-id", "41")])
        .await;
    assert_stream(&sse);
    let rest = sse.rest().await;
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert_eq!(rest[0].data()["error"]["code"], "SESSION_NOT_OPEN");
}

/// `watch_signals.json`: a Signal sent with `Send` is observed as
/// `WatchSignalsResponse` with `sender_authenticated` beside the Envelope,
/// no `id:`, then a keepalive comment.
#[tokio::test]
async fn example_watch_signals() {
    let ex = example("watch_signals.json");
    let rig = rig(Opts {
        keepalive: Duration::from_millis(150),
        ..Opts::default()
    })
    .await;
    let watch = &ex["exchanges"][0];
    let mut sse = rig
        .open(
            "WatchSignals",
            &[
                ("authorization", "Bearer tok-observer"),
                ("accept", "text/event-stream"),
            ],
            &watch["request"]["body"],
        )
        .await;
    assert_stream(&sse);
    // The subscription exists once the response head is out.
    let mut signal = ex["exchanges"][1]["request"]["body"]["envelope"].clone();
    signal.as_object_mut().unwrap().remove("timestamp");
    rig.send("tok-telemetry", signal).await;
    let want = watch["response"]["events"].as_array().unwrap();
    let ev = sse.next().await.unwrap();
    assert_eq!(ev.id(), None);
    assert_eq!(
        without_timestamp(ev.data().clone()),
        without_timestamp(want[0]["data"].clone())
    );
    assert_eq!(sse.next().await.unwrap(), Ev::Comment("keepalive".into()));
}

/// Without configured authentication, a Signal's sender is self-asserted:
/// the event carries `sender_authenticated: false` beside the Envelope.
#[tokio::test]
async fn watch_signals_unauthenticated_sender() {
    let ex = example("watch_signals.json");
    let rig = rig(Opts {
        dev_auth: true,
        ..Opts::default()
    })
    .await;
    let mut sse = rig
        .open(
            "WatchSignals",
            &[
                ("authorization", "Bearer agent://observer"),
                ("accept", "text/event-stream"),
            ],
            &json!({}),
        )
        .await;
    assert_stream(&sse);
    let mut signal = ex["exchanges"][1]["request"]["body"]["envelope"].clone();
    signal.as_object_mut().unwrap().remove("timestamp");
    rig.send("agent://telemetry.observer", signal).await;
    let ev = sse.next().await.unwrap();
    assert_eq!(ev.id(), None);
    let mut want = without_timestamp(ex["exchanges"][0]["response"]["events"][0]["data"].clone());
    want["sender_authenticated"] = json!(false);
    assert_eq!(without_timestamp(ev.data().clone()), want);
}

// ───────────────────────── Accept and pre-stream checks ─────────────────────────

/// `406` (no body, no code) when `Accept` excludes `text/event-stream`;
/// `*/*`, `text/*`, a q-valued list and a missing `Accept` all open the
/// stream. `406` comes before authentication.
#[tokio::test]
async fn accept_negotiation() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 1).await;
    for method in ["StreamSession", "WatchRoots"] {
        let body = if method == "StreamSession" {
            json!({"subscribe_session_id": s})
        } else {
            json!({})
        };
        for refused in ["application/json", "text/html", "text/event-stream;q=0"] {
            for auth in [Some("Bearer tok-orch"), None] {
                let mut headers = vec![("accept", refused)];
                if let Some(a) = auth {
                    headers.push(("authorization", a));
                }
                let sse = rig.open(method, &headers, &body).await;
                assert_eq!(sse.status, 406, "{method} {refused}");
                assert!(sse.header("macp-error-code").is_none());
                assert!(sse.body_bytes().await.is_empty());
            }
        }
        for admitted in [
            Some("text/event-stream"),
            Some("*/*"),
            Some("text/*"),
            Some("text/event-stream;q=0.5, application/json"),
            None,
        ] {
            let mut headers = vec![("authorization", "Bearer tok-orch")];
            if let Some(a) = admitted {
                headers.push(("accept", a));
            }
            let mut sse = rig.open(method, &headers, &body).await;
            assert_stream(&sse);
            assert!(sse.next_event().await.is_some(), "{method} {admitted:?}");
        }
    }
}

/// Every refusal before the first event is request-level and opens no
/// stream.
#[tokio::test]
async fn subscribe_failure_paths() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 2).await;
    let accept = ("accept", "text/event-stream");
    let orch = ("authorization", "Bearer tok-orch");

    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, Value, u16, &'a str);
    let cases: Vec<Case> = vec![
        (
            "non-integer Last-Event-ID",
            vec![("last-event-id", "4x")],
            json!({"subscribe_session_id": s}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "negative Last-Event-ID",
            vec![("last-event-id", "-1")],
            json!({"subscribe_session_id": s}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "conflicting after_sequence",
            vec![("last-event-id", "2")],
            json!({"subscribe_session_id": s, "after_sequence": 1}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "envelope in body",
            vec![],
            json!({"subscribe_session_id": s, "envelope": proposal(&s, "x")}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "empty subscribe_session_id",
            vec![],
            json!({"subscribe_session_id": ""}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "no subscribe_session_id",
            vec![],
            json!({}),
            400,
            "INVALID_REQUEST",
        ),
        (
            "not JSON",
            vec![],
            json!("not an object"),
            400,
            "INVALID_REQUEST",
        ),
        (
            "unknown session",
            vec![],
            json!({"subscribe_session_id": sid()}),
            404,
            "SESSION_NOT_FOUND",
        ),
    ];
    for (name, extra, body, status, code) in cases {
        let mut headers = vec![accept, orch];
        headers.extend(extra);
        let sse = rig.open("StreamSession", &headers, &body).await;
        assert_eq!(sse.status, status, "{name}");
        let v = sse.error_json().await;
        assert_eq!(v["error"]["code"], code, "{name}: {v}");
    }
    // A matching Last-Event-ID and after_sequence is fine.
    let mut sse = rig
        .open(
            "StreamSession",
            &[accept, orch, ("last-event-id", "1")],
            &json!({"subscribe_session_id": s, "after_sequence": 1}),
        )
        .await;
    assert_stream(&sse);
    assert_eq!(sse.next_event().await.unwrap().id(), Some(2));

    // Forbidden subscriber: not initiator, participant or observer.
    let sse = rig.subscribe("tok-outsider", &s, &[]).await;
    assert_eq!(sse.status, 403);
    let v = sse.error_json().await;
    assert_eq!(v["error"]["code"], "FORBIDDEN");
    assert_eq!(v["error"]["session_id"], json!(s));
    // No credential.
    let sse = rig
        .open(
            "StreamSession",
            &[accept],
            &json!({"subscribe_session_id": s}),
        )
        .await;
    assert_eq!(sse.status, 401);
    assert_eq!(sse.header("www-authenticate"), Some("Bearer"));
    sse.error_json().await;
}

/// Which streaming RPCs authenticate is the gRPC handlers' answer: without a
/// credential, gRPC refuses exactly the rows HTTP answers `401`.
#[tokio::test]
async fn streaming_rows_authenticate_like_grpc() {
    let rig = rig(Opts::default()).await;
    let accept = ("accept", "text/event-stream");
    let grpc_refuses = [
        (
            "WatchModeRegistry",
            MacpRuntimeService::watch_mode_registry(
                &rig.server,
                tonic::Request::new(pb::WatchModeRegistryRequest::default()),
            )
            .await
            .is_err(),
        ),
        (
            "WatchRoots",
            MacpRuntimeService::watch_roots(
                &rig.server,
                tonic::Request::new(pb::WatchRootsRequest::default()),
            )
            .await
            .is_err(),
        ),
        (
            "WatchSignals",
            MacpRuntimeService::watch_signals(
                &rig.server,
                tonic::Request::new(pb::WatchSignalsRequest::default()),
            )
            .await
            .is_err(),
        ),
        (
            "WatchSessions",
            MacpRuntimeService::watch_sessions(
                &rig.server,
                tonic::Request::new(pb::WatchSessionsRequest::default()),
            )
            .await
            .is_err(),
        ),
        (
            "WatchPolicies",
            MacpRuntimeService::watch_policies(
                &rig.server,
                tonic::Request::new(pb::WatchPoliciesRequest::default()),
            )
            .await
            .is_err(),
        ),
    ];
    for (method, refused) in grpc_refuses {
        let mut sse = rig.open(method, &[accept], &json!({})).await;
        if refused {
            assert_eq!(sse.status, 401, "{method}");
            sse.error_json().await;
        } else {
            assert_stream(&sse);
            // Each open watch stream starts with the current state.
            let ev = sse.next_event().await.unwrap();
            assert_eq!(ev.id(), None, "{method}: watch events carry no id");
        }
    }
    // With a credential, the authenticated watches open too.
    let mut sse = rig
        .open(
            "WatchSessions",
            &[accept, ("authorization", "Bearer tok-observer")],
            &json!({}),
        )
        .await;
    assert_stream(&sse);
    let s = sid();
    rig.send("tok-orch", start(&s)).await;
    loop {
        let ev = sse.next_event().await.unwrap();
        assert_eq!(ev.id(), None);
        if ev.data().to_string().contains(&s) {
            break;
        }
    }
}

// ───────────────────────── how a subscribe stream ends ─────────────────────────

/// The terminal Commitment ends the response normally (no `event: error`),
/// live and on a subscribe to an already-resolved session.
#[tokio::test]
async fn commitment_ends_the_stream_normally() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 2).await;
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    assert_stream(&sse);
    assert_eq!(sse.next_event().await.unwrap().id(), Some(1));
    assert_eq!(sse.next_event().await.unwrap().id(), Some(2));
    resolve(&rig, &s).await;
    let rest = sse.rest().await;
    assert_eq!(
        rest.iter().map(|e| e.id()).collect::<Vec<_>>(),
        [Some(3), Some(4)]
    );
    assert_eq!(rest[1].data()["envelope"]["message_type"], "Commitment");

    // Already resolved: replay through the Commitment, then a normal end.
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    let all = sse.rest().await;
    assert_eq!(
        all.iter().filter_map(Ev::id).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(!all.iter().any(Ev::is_error));
    // Resuming at the terminal ordinal: nothing to deliver, a normal end.
    let mut sse = rig
        .subscribe("tok-observer", &s, &[("last-event-id", "4")])
        .await;
    assert_stream(&sse);
    assert!(sse.rest().await.is_empty());
}

/// With terminal-log compaction (the runtime default), a resolved session
/// resumed at its last ordinal ends normally; below it, `409`.
#[tokio::test]
async fn resolved_and_compacted() {
    let rig = rig(Opts {
        compact: true,
        ..Opts::default()
    })
    .await;
    let s = session_with(&rig, 2).await;
    resolve(&rig, &s).await;
    let mut sse = rig
        .subscribe("tok-observer", &s, &[("last-event-id", "4")])
        .await;
    assert_stream(&sse);
    assert!(sse.rest().await.is_empty());
    let sse = rig
        .subscribe("tok-observer", &s, &[("last-event-id", "3")])
        .await;
    assert_eq!(sse.status, 409);
    let v = sse.error_json().await;
    let details = base64::engine::general_purpose::STANDARD
        .decode(v["error"]["details"].as_str().unwrap())
        .unwrap();
    assert_eq!(details, br#"{"lowest_available_sequence":4}"#);
}

/// Cancel ⇒ every envelope accepted before it, then `event: error`
/// `SESSION_NOT_OPEN`.
#[tokio::test]
async fn cancel_ends_with_session_not_open_after_buffered_envelopes() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 2).await;
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    assert_eq!(sse.next_event().await.unwrap().id(), Some(1));
    assert_eq!(sse.next_event().await.unwrap().id(), Some(2));
    for i in 0..5 {
        rig.accept(SECURITY, proposal(&s, &format!("late{i}")))
            .await;
    }
    rig.cancel(&s).await;
    let rest = sse.rest().await;
    let ids: Vec<u64> = rest.iter().filter_map(Ev::id).collect();
    assert_eq!(ids, [3, 4, 5, 6, 7], "{rest:?}");
    let last = rest.last().unwrap();
    assert!(last.is_error(), "{rest:?}");
    assert_eq!(last.data()["error"]["code"], "SESSION_NOT_OPEN");
    assert_eq!(last.data()["error"]["session_id"], json!(s));
    assert!(last.data()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cancelled"));
}

/// TTL expiry ⇒ the same ending.
#[tokio::test]
async fn ttl_expiry_ends_with_session_not_open() {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now().timestamp_millis()));
    let rig = rig(Opts {
        clock: Some(Arc::clone(&clock)),
        ..Opts::default()
    })
    .await;
    let s = sid();
    rig.send("tok-orch", start(&s)).await;
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    assert_eq!(sse.next_event().await.unwrap().id(), Some(1));
    rig.send("tok-security", proposal(&s, "p0")).await;
    clock.advance(600_001);
    rig.runtime.cleanup_expired_sessions().await;
    let rest = sse.rest().await;
    assert_eq!(rest.len(), 2, "{rest:?}");
    assert_eq!(rest[0].id(), Some(2));
    assert_eq!(rest[1].data()["error"]["code"], "SESSION_NOT_OPEN");
    assert!(rest[1].data()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("expired"));
}

/// A subscribe to a session already cancelled replays, then the error.
#[tokio::test]
async fn already_cancelled_replays_then_errors() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 3).await;
    rig.cancel(&s).await;
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    assert_stream(&sse);
    let rest = sse.rest().await;
    assert_eq!(
        rest.iter().filter_map(Ev::id).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(rest.len(), 4);
    assert_eq!(rest[3].data()["error"]["code"], "SESSION_NOT_OPEN");
}

// ───────────────────────── resume ─────────────────────────

/// Read N events, drop the connection, then resubscribe with
/// `Last-Event-ID: k` for every k in 0..=N while new envelopes keep
/// arriving; each resubscribe reads past the live boundary. The union of
/// deliveries is gapless, each resume starts at k + 1, and every ordinal
/// maps to the same `message_id` as the log order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_from_every_position_while_appending() {
    const N: u64 = 12;
    let rig = Arc::new(rig(Opts::default()).await);
    let s = session_with(&rig, N).await;

    let mut seen: BTreeMap<u64, String> = BTreeMap::new();
    let record = |ev: &Ev, seen: &mut BTreeMap<u64, String>| {
        let id = ev.id().unwrap();
        let mid = ev.message_id();
        if let Some(prev) = seen.insert(id, mid.clone()) {
            assert_eq!(prev, mid, "ordinal {id} relabelled");
        }
        id
    };
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    for want in 1..=N {
        let ev = sse.next_event().await.unwrap();
        assert_eq!(record(&ev, &mut seen), want);
    }
    drop(sse);

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (rig, stop, s) = (Arc::clone(&rig), Arc::clone(&stop), s.clone());
        tokio::spawn(async move {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) && i < 5000 {
                rig.accept(SECURITY, proposal(&s, &format!("w{i}"))).await;
                i += 1;
                // Pace the writer so each resume's read stays short.
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    };

    for k in 0..=N {
        let high_water_before = rig.log_ids(&s).await.len() as u64;
        let header = k.to_string();
        let mut sse = rig
            .subscribe("tok-observer", &s, &[("last-event-id", &header)])
            .await;
        assert_stream(&sse);
        let mut expect = k + 1;
        // Past the high water at open: envelopes accepted after it are live.
        while expect <= high_water_before + 3 {
            let ev = sse.next_event().await.unwrap();
            assert!(!ev.is_error(), "{ev:?}");
            assert_eq!(record(&ev, &mut seen), expect, "resume at {k}");
            expect += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.await.unwrap();

    let log = rig.log_ids(&s).await;
    let max = *seen.keys().last().unwrap();
    assert_eq!(
        seen.keys().copied().collect::<Vec<_>>(),
        (1..=max).collect::<Vec<_>>(),
        "gapless union"
    );
    for (ordinal, mid) in &seen {
        assert_eq!(&log[(*ordinal - 1) as usize], mid, "ordinal {ordinal}");
    }
    // GetSession agrees the session is still open (no terminal entry).
    let v = rig
        .call("GetSession", "tok-orch", &json!({"session_id": s}))
        .await;
    assert_eq!(v["metadata"]["state"], "SESSION_STATE_OPEN", "{v}");
}

// ───────────────────────── lifetime ─────────────────────────

/// A client that goes away drops the stream, and with it the bus and
/// lifecycle receivers (the stream owns them and a runtime handle, whose
/// count returns to its baseline).
#[tokio::test]
async fn client_disconnect_drops_the_stream() {
    let rig = rig(Opts::default()).await;
    let s = session_with(&rig, 1).await;
    let baseline = Arc::strong_count(&rig.runtime);
    let mut sse = rig.subscribe("tok-observer", &s, &[]).await;
    assert_eq!(sse.next_event().await.unwrap().id(), Some(1));
    assert!(Arc::strong_count(&rig.runtime) > baseline);
    drop(sse);
    let deadline = Instant::now() + READ_WITHIN;
    while Arc::strong_count(&rig.runtime) > baseline {
        assert!(Instant::now() < deadline, "stream not dropped");
        // A publish makes the server notice the closed connection.
        rig.accept(SECURITY, proposal(&s, &uuid::Uuid::new_v4().to_string()))
            .await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The request timeout bounds only the pre-stream checks: a stream outlives
/// it, carrying keepalives.
#[tokio::test]
async fn streams_outlive_the_request_timeout() {
    let rig = rig(Opts {
        request_timeout_secs: 1,
        keepalive: Duration::from_millis(100),
        ..Opts::default()
    })
    .await;
    let mut sse = rig
        .open("WatchRoots", &[("accept", "text/event-stream")], &json!({}))
        .await;
    assert_stream(&sse);
    let opened = Instant::now();
    assert!(matches!(sse.next().await, Some(Ev::Data { .. })));
    while opened.elapsed() < Duration::from_millis(1600) {
        assert_eq!(sse.next().await, Some(Ev::Comment("keepalive".into())));
    }
}

/// Shutdown ends open streams at once, as a lost connection (no
/// `event: error`), so the drain completes without waiting for the
/// deadline.
#[tokio::test]
async fn shutdown_ends_streams_and_the_drain_completes() {
    let mut rig = rig(Opts::default()).await;
    let s = session_with(&rig, 1).await;
    let mut sub = rig.subscribe("tok-observer", &s, &[]).await;
    assert_eq!(sub.next_event().await.unwrap().id(), Some(1));
    let mut watch = rig
        .open("WatchRoots", &[("accept", "text/event-stream")], &json!({}))
        .await;
    assert!(watch.next_event().await.is_some());
    rig.shutdown.take().unwrap().send(()).unwrap();
    assert!(sub.rest().await.is_empty());
    assert!(watch.rest().await.is_empty());
    tokio::time::timeout(READ_WITHIN, &mut rig.served)
        .await
        .expect("drain held open by a stream")
        .unwrap();
}
