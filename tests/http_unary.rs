//! The unary RPCs of the HTTP binding (RFC-MACP-0006 §4.1-§4.9), plan
//! `plans/http-listener.md` P5, `http-listener` feature.
//!
//! Every test drives a real `HttpListener` on `127.0.0.1:0` in this process,
//! over real TCP, through the shared client [`Rig::post`], which asserts on
//! every response the binding-wide MUSTs: no `Set-Cookie` (§4.6), never
//! `Content-Type: application/macp-envelope+json` (§4.2), no `Send` answered
//! `429` (§4.4 with today's buckets, which are Envelope decisions), and every
//! error body a valid `macp-http-error.schema.json` body whose code matches
//! the `MACP-Error-Code` header and the registry's HTTP status, with
//! `WWW-Authenticate: Bearer` on a `401`.
//!
//! Most rigs also serve the same `MacpServer` over gRPC (a tonic server on
//! another loopback port), so the two bindings share one runtime, security
//! layer and set of rate buckets, as in the binary: the golden parity test
//! and the rate-limit tests compare the two directly.
//!
//! Static tokens come from `SecurityLayer::with_static_tokens` (no process
//! env), so the tests run in parallel.
#![cfg(feature = "http-listener")]

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use macp_runtime::http::json::{
    decode_message, encode_message, envelope_from_json, method_descriptor,
};
use macp_runtime::http::{HttpConfig, HttpListener};
use macp_runtime::log_store::LogStore;
use macp_runtime::pb;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::{RateLimitConfig, SecurityLayer};
use macp_runtime::server::MacpServer;
use prost_reflect::DynamicMessage;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const DECISION: &str = "macp.mode.decision.v1";
const PROPOSAL: &str = "macp.mode.proposal.v1";
const ORCH: &str = "agent://orchestrator";

/// Static tokens (no dots: the static resolver leaves dotted, JWT-shaped
/// tokens to the JWT resolver, so the examples' tokens are mapped onto these).
const TOKENS: &str = r#"[
  {"token": "tok-orch", "sender": "agent://orchestrator", "can_manage_mode_registry": true},
  {"token": "tok-security", "sender": "agent://security"},
  {"token": "tok-finance", "sender": "agent://finance"},
  {"token": "tok-marketing", "sender": "agent://marketing"},
  {"token": "tok-outsider", "sender": "agent://outsider"},
  {"token": "tok-decision-only", "sender": "agent://decision-only",
   "allowed_modes": ["macp.mode.decision.v1"]}
]"#;

/// The vendored examples' bearer tokens → the tokens configured here.
const EXAMPLE_TOKENS: [(&str, &str); 2] = [
    ("eyJhbGciOiJFZERTQSJ9.example.signature", "tok-orch"),
    ("eyJhbGciOiJFZERTQSJ9.other.signature", "tok-marketing"),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

// ───────────────────────── rig ─────────────────────────

#[derive(Clone)]
struct Opts {
    /// `None`: dev-mode auth (no auth configured), as the binary with
    /// `MACP_ALLOW_INSECURE=1` and no tokens.
    tokens: Option<&'static str>,
    message_limit: usize,
    start_limit: usize,
    max_payload: usize,
    request_timeout_secs: u64,
    public_url: Option<&'static str>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            tokens: Some(TOKENS),
            message_limit: usize::MAX,
            start_limit: usize::MAX,
            max_payload: 1_048_576,
            request_timeout_secs: 30,
            public_url: None,
        }
    }
}

struct Rig {
    http: SocketAddr,
    grpc: MacpRuntimeServiceClient<Channel>,
    max_body: usize,
}

struct Reply {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body is not JSON ({e}): {:?}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    fn error_code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap().to_string()
    }

    fn ack(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        self.json()["ack"].clone()
    }
}

async fn rig(opts: Opts) -> Rig {
    let mut security = SecurityLayer::dev_mode().with_rate_limits(
        RateLimitConfig {
            limit: opts.start_limit,
            window: Duration::from_secs(60),
        },
        RateLimitConfig {
            limit: opts.message_limit,
            window: Duration::from_secs(60),
        },
    );
    security.max_payload_bytes = opts.max_payload;
    if let Some(tokens) = opts.tokens {
        security = security.with_static_tokens(tokens).unwrap();
    }
    let runtime = Arc::new(Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));
    let server = MacpServer::new(runtime, security);

    let mut env: HashMap<&str, String> = HashMap::from([
        ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string()),
        ("MACP_ALLOW_INSECURE", "1".into()),
        ("MACP_MAX_PAYLOAD_BYTES", opts.max_payload.to_string()),
        (
            "MACP_REQUEST_TIMEOUT_SECS",
            opts.request_timeout_secs.to_string(),
        ),
    ]);
    if let Some(url) = opts.public_url {
        env.insert("MACP_HTTP_PUBLIC_URL", url.into());
    }
    let config = HttpConfig::from_env(&move |k| env.get(k).cloned())
        .unwrap()
        .unwrap();
    let max_body = config.max_body_bytes;
    let listener = HttpListener::bind(&config, server.clone()).await.unwrap();
    let http = listener.local_addr().unwrap();
    tokio::spawn(listener.serve(std::future::pending()));

    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let grpc_addr = incoming.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(MacpRuntimeServiceServer::new(server))
            .serve_with_incoming(incoming),
    );
    let grpc = MacpRuntimeServiceClient::connect(format!("http://{grpc_addr}"))
        .await
        .unwrap();
    Rig {
        http,
        grpc,
        max_body,
    }
}

/// One HTTP/1.1 connection; several requests may share it.
struct Conn {
    send: http1::SendRequest<Full<Bytes>>,
}

impl Rig {
    async fn conn(&self) -> Conn {
        let tcp = TcpStream::connect(self.http).await.unwrap();
        let (send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
        tokio::spawn(conn);
        Conn { send }
    }

    /// POST on a fresh connection.
    async fn post(&self, path: &str, headers: &[(&str, &str)], body: impl Into<Bytes>) -> Reply {
        self.conn().await.post(path, headers, body).await
    }

    /// POST JSON with `Authorization: Bearer <token>` (none for `None`).
    async fn call(&self, method: &str, token: Option<&str>, body: &Value) -> Reply {
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut headers = vec![("content-type", "application/json")];
        if let Some(a) = &auth {
            headers.push(("authorization", a.as_str()));
        }
        self.post(
            &format!("/macp/v1/{method}"),
            &headers,
            serde_json::to_vec(body).unwrap(),
        )
        .await
    }

    async fn send(&self, token: &str, envelope: Value) -> Reply {
        self.call("Send", Some(token), &json!({ "envelope": envelope }))
            .await
    }

    /// The same Envelope over gRPC: its Ack.
    async fn grpc_send(&self, token: &str, envelope: &Value) -> pb::Ack {
        let env = envelope_from_json(envelope).unwrap();
        let resp = self
            .grpc
            .clone()
            .send(with_token(
                Some(token),
                pb::SendRequest {
                    envelope: Some(env),
                },
            ))
            .await
            .unwrap();
        resp.into_inner().ack.unwrap()
    }
}

impl Conn {
    async fn post(
        &mut self,
        path: &str,
        headers: &[(&str, &str)],
        body: impl Into<Bytes>,
    ) -> Reply {
        let mut b = http::Request::builder()
            .method("POST")
            .uri(path)
            .header("host", "localhost");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let resp = self
            .send
            .send_request(b.body(Full::new(body.into())).unwrap())
            .await
            .unwrap();
        let (parts, body) = resp.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        let reply = Reply {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
        };
        check_reply(path, &reply);
        reply
    }
}

/// The binding-wide MUSTs, asserted on every response of the suite.
fn check_reply(path: &str, r: &Reply) {
    assert!(
        r.headers.get("set-cookie").is_none(),
        "{path}: Set-Cookie on a response (RFC-MACP-0006 §4.6)"
    );
    for ct in r.headers.get_all("content-type") {
        let ct = ct.to_str().unwrap().to_ascii_lowercase();
        assert!(
            !ct.contains("application/macp-envelope+json"),
            "{path}: response labelled {ct} (§4.2)"
        );
    }
    if path.ends_with("/Send") {
        assert_ne!(r.status, 429, "{path}: Send answered 429");
    }
    if !r.body.is_empty() {
        assert_eq!(
            r.header("content-type"),
            Some("application/json"),
            "{path}: body without application/json"
        );
    }
    if r.status >= 400 && !r.body.is_empty() {
        let v = r.json();
        check_error_body(&v).unwrap_or_else(|e| panic!("{path}: {e}: {v}"));
        let code = v["error"]["code"].as_str().unwrap();
        assert_eq!(r.header("macp-error-code"), Some(code), "{path}");
        let entry = macp_runtime::macp_core::error::registry_entry(code)
            .unwrap_or_else(|| panic!("{path}: {code} not in the registry"));
        assert_eq!(r.status, entry.http, "{path}: {code}");
        let details = v["error"]["details"].as_str().unwrap_or("");
        base64::engine::general_purpose::STANDARD
            .decode(details)
            .unwrap_or_else(|e| panic!("{path}: details not base64: {e}"));
        assert_eq!(v["error"]["message_id"], "", "{path}");
    }
    if r.status == 401 {
        assert_eq!(r.header("www-authenticate"), Some("Bearer"), "{path}");
    }
    if r.body.is_empty() && r.status >= 400 {
        assert!(r.header("macp-error-code").is_none(), "{path}");
    }
}

fn with_token<T>(token: Option<&str>, body: T) -> tonic::Request<T> {
    let mut req = tonic::Request::new(body);
    if let Some(token) = token {
        req.metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    req
}

// ───────────────────────── envelopes ─────────────────────────

fn envelope(
    mode: &str,
    message_type: &str,
    message_id: &str,
    session_id: &str,
    payload: Value,
) -> Value {
    json!({
        "macp_version": "1.0",
        "mode": mode,
        "message_type": message_type,
        "message_id": message_id,
        "session_id": session_id,
        "sender": "",
        "payload": payload,
    })
}

fn start(session_id: &str, participants: &[&str]) -> Value {
    envelope(
        DECISION,
        "SessionStart",
        &format!("{session_id}-start"),
        session_id,
        json!({
            "intent": "http unary",
            "participants": participants,
            "mode_version": "1.0.0",
            "configuration_version": "config.default",
            "policy_version": "policy.default",
            "ttl_ms": 600000,
        }),
    )
}

fn proposal(session_id: &str, message_id: &str) -> Value {
    envelope(
        DECISION,
        "Proposal",
        message_id,
        session_id,
        json!({"proposal_id": message_id, "option": "a", "rationale": "r", "supporting_data": ""}),
    )
}

/// A fresh, valid session id per call.
fn sid() -> String {
    uuid::Uuid::new_v4().to_string()
}

async fn open_session(rig: &Rig, participants: &[&str]) -> String {
    let s = sid();
    let ack = rig.send("tok-orch", start(&s, participants)).await.ack();
    assert_eq!(ack["ok"], true, "{ack}");
    s
}

// ───────────────────────── error-body schema check ─────────────────────────

/// `macp-http-error.schema.json`'s `error.code` pattern.
const CODE_PATTERN: &str = "^[A-Z][A-Z0-9]*(_[A-Z0-9]+)*$";
/// The members of `error` the schema types (all `string`).
const ERROR_STRING_MEMBERS: [&str; 5] = ["code", "message", "session_id", "message_id", "details"];

/// The hand-written check of `macp-http-error.schema.json` (no `jsonschema`
/// dependency): pinned to the schema file by `error_schema_check_matches_the_schema_file`.
fn check_error_body(v: &Value) -> Result<(), String> {
    let obj = v.as_object().ok_or("body is not an object")?;
    let error = obj.get("error").ok_or("no `error` member")?;
    let error = error.as_object().ok_or("`error` is not an object")?;
    if !error.contains_key("code") {
        return Err("`error` has no `code`".into());
    }
    for member in ERROR_STRING_MEMBERS {
        if let Some(value) = error.get(member) {
            if !value.is_string() {
                return Err(format!("`error.{member}` is not a string"));
            }
        }
    }
    let code = error["code"].as_str().unwrap_or_default();
    if !is_upper_snake(code) {
        return Err(format!(
            "`error.code` {code:?} does not match {CODE_PATTERN}"
        ));
    }
    Ok(())
}

/// `^[A-Z][A-Z0-9]*(_[A-Z0-9]+)*$`, by hand.
fn is_upper_snake(code: &str) -> bool {
    let mut segments = code.split('_');
    let first = segments.next().unwrap_or_default();
    let alnum = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    };
    first.bytes().next().is_some_and(|b| b.is_ascii_uppercase())
        && alnum(first)
        && segments.all(alnum)
}

#[test]
fn error_schema_check_matches_the_schema_file() {
    let schema: Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/http/macp-http-error.schema.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["required"], json!(["error"]));
    let top: BTreeSet<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(top, BTreeSet::from(["error"]));
    let error = &schema["properties"]["error"];
    assert_eq!(error["type"], "object");
    assert_eq!(error["required"], json!(["code"]));
    let props = error["properties"].as_object().unwrap();
    assert_eq!(
        props.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        ERROR_STRING_MEMBERS.into_iter().collect::<BTreeSet<_>>(),
        "the checked member set is the schema's"
    );
    for (name, prop) in props {
        assert_eq!(prop["type"], "string", "{name}");
    }
    assert_eq!(props["code"]["pattern"], CODE_PATTERN);
    // Anything the check does not cover must not be in the schema.
    for (k, _) in schema.as_object().unwrap() {
        assert!(
            [
                "$schema",
                "$id",
                "title",
                "description",
                "type",
                "required",
                "properties"
            ]
            .contains(&k.as_str()),
            "unchecked schema keyword {k}"
        );
    }
    for (k, _) in error.as_object().unwrap() {
        assert!(
            ["type", "required", "properties"].contains(&k.as_str()),
            "unchecked error keyword {k}"
        );
    }
}

#[test]
fn every_negative_error_fixture_fails_the_check() {
    let dir = root().join("tests/http/invalid-errors");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(check_error_body(&v).is_err(), "{} passed", path.display());
        n += 1;
    }
    assert_eq!(n, 10);
    // The pattern matcher on its own.
    for ok in ["A", "SESSION_NOT_FOUND", "A1_B2", "X9"] {
        assert!(is_upper_snake(ok), "{ok}");
    }
    for bad in ["", "a", "_A", "A_", "A__B", "1A", "A-B", "Ab"] {
        assert!(!is_upper_snake(bad), "{bad}");
    }
}

// ───────────────────────── vendored examples ─────────────────────────

/// Drop what the comparison ignores: `message`, timestamps and every
/// `*_unix_ms` (accepted_at, started_at, expires_at, ...).
fn normalize(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.retain(|k, _| k != "message" && k != "timestamp" && !k.ends_with("_unix_ms"));
            map.values_mut().for_each(normalize);
        }
        Value::Array(items) => items.iter_mut().for_each(normalize),
        _ => {}
    }
}

/// Replay one vendored example file against `rig`.
async fn replay_examples(rig: &Rig, file: &str) -> usize {
    let doc: Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/http/examples").join(file)).unwrap(),
    )
    .unwrap();
    let base = doc["base"].as_str().unwrap();
    let origin = base.trim_end_matches("/api");
    let exchanges = doc["exchanges"].as_array().unwrap();
    for ex in exchanges {
        let title = ex["title"].as_str().unwrap();
        let req = &ex["request"];
        assert_eq!(req["method"], "POST", "{title}");
        let path = req["url"].as_str().unwrap().strip_prefix(origin).unwrap();
        let mut headers: Vec<(String, String)> = Vec::new();
        for (k, v) in req["headers"].as_object().unwrap() {
            let mut v = v.as_str().unwrap().to_string();
            for (example, ours) in EXAMPLE_TOKENS {
                v = v.replace(example, ours);
            }
            headers.push((k.clone(), v));
        }
        let hdrs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        // The examples' Envelopes carry a fixed March 2026 timestamp, and the
        // TTL deadline runs from the SessionStart timestamp (RFC-MACP-0003
        // §2), so replayed against a live clock their session would already
        // be expired: each Envelope is re-stamped with the present.
        let mut body = req["body"].clone();
        if let Some(ts) = body.pointer_mut("/envelope/timestamp") {
            *ts = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
        let reply = rig
            .post(path, &hdrs, serde_json::to_vec(&body).unwrap())
            .await;
        let want = &ex["response"];
        assert_eq!(
            u64::from(reply.status),
            want["status"].as_u64().unwrap(),
            "{title}: {}",
            String::from_utf8_lossy(&reply.body)
        );
        for (k, v) in want["headers"].as_object().unwrap() {
            assert_eq!(reply.header(k), v.as_str(), "{title}: header {k}");
        }
        match want.get("body") {
            None => assert!(
                reply.body.is_empty(),
                "{title}: transport status with a body"
            ),
            Some(body) => {
                let (mut got, mut want) = (reply.json(), body.clone());
                // Divergence (informative example, not a MUST): a rejected
                // Ack here carries `session_state: UNSPECIFIED`, as on gRPC
                // (`make_error_ack`); the example shows the session's state.
                if want["ack"]["ok"] == false {
                    want["ack"]["session_state"] = got["ack"]["session_state"].clone();
                }
                // Divergence (informative example): this runtime does not
                // credit the SessionStart as participant activity, on either
                // binding (`step::commit` credits the messages after it), so
                // `participant_activity` is empty where the example shows the
                // initiator's one message.
                if want.pointer("/metadata/participant_activity").is_some() {
                    want["metadata"]["participant_activity"] =
                        got["metadata"]["participant_activity"].clone();
                }
                normalize(&mut got);
                normalize(&mut want);
                assert_eq!(got, want, "{title}");
            }
        }
    }
    exchanges.len()
}

/// Each exchange of `send.json` and `errors.json`, replayed in order against
/// the listener (with the examples' `/api` prefix as `MACP_HTTP_PUBLIC_URL`),
/// reproduces status, the required headers and the body (ignoring
/// `message`, timestamps and `accepted_at_unix_ms`).
#[tokio::test]
async fn vendored_examples_replay() {
    let rig = rig(Opts {
        public_url: Some("https://runtime.example.com/api"),
        ..Opts::default()
    })
    .await;
    // The examples' SessionStart binds this policy version.
    let r = rig
        .call(
            "RegisterPolicy",
            Some("tok-orch"),
            &json!({"policy_descriptor": {
                "policy_id": "org-procurement-2026.02",
                "mode": DECISION,
                "description": "examples",
                "rules": {},
                "schema_version": 1
            }}),
        )
        .await;
    assert_eq!(r.json()["ok"], true, "{}", r.json());
    assert_eq!(replay_examples(&rig, "send.json").await, 4);
    assert_eq!(replay_examples(&rig, "errors.json").await, 6);
}

// ───────────────────────── authentication (401) ─────────────────────────

#[tokio::test]
async fn unauthenticated_requests_are_401_on_every_authenticated_rpc() {
    let rig = rig(Opts::default()).await;
    let s = open_session(&rig, &[ORCH]).await;
    let get = json!({ "session_id": s });
    for (label, headers) in [
        ("missing", vec![]),
        (
            "wrong scheme",
            vec![("authorization", "Basic dG9rLW9yY2g=")],
        ),
        (
            "lowercase bearer",
            vec![("authorization", "bearer tok-orch")],
        ),
        ("bad token", vec![("authorization", "Bearer nope")]),
        ("x-macp-token only", vec![("x-macp-token", "tok-orch")]),
        (
            "cookie only",
            vec![("cookie", "authorization=Bearer tok-orch")],
        ),
    ] {
        let mut h = headers.clone();
        h.push(("content-type", "application/json"));
        let r = rig
            .post("/macp/v1/GetSession", &h, serde_json::to_vec(&get).unwrap())
            .await;
        assert_eq!(r.status, 401, "{label}");
        assert_eq!(r.error_code(), "UNAUTHENTICATED", "{label}");
    }

    // The scheme is exact-match on both bindings: gRPC refuses lowercase too,
    // and a correct token is accepted on both.
    let lower = {
        let mut req = tonic::Request::new(pb::GetSessionRequest {
            session_id: s.clone(),
        });
        req.metadata_mut()
            .insert("authorization", "bearer tok-orch".parse().unwrap());
        rig.grpc.clone().get_session(req).await.unwrap_err()
    };
    assert_eq!(lower.code(), tonic::Code::Unauthenticated);
    assert_eq!(
        rig.call("GetSession", Some("tok-orch"), &get).await.status,
        200
    );
    // gRPC honours its own x-macp-token header; HTTP (above) does not (§4.6).
    let mut req = tonic::Request::new(pb::GetSessionRequest { session_id: s });
    req.metadata_mut()
        .insert("x-macp-token", "tok-orch".parse().unwrap());
    assert!(rig.grpc.clone().get_session(req).await.is_ok());

    // Every authenticated row answers 401 with no token, matching gRPC's
    // UNAUTHENTICATED status for the same empty request; the open rows do not.
    for method in [
        "Initialize",
        "GetSession",
        "CancelSession",
        "SuspendSession",
        "ResumeSession",
        "GetManifest",
        "ListModes",
        "ListRoots",
        "ListExtModes",
        "RegisterExtMode",
        "UnregisterExtMode",
        "PromoteMode",
        "ListSessions",
        "RegisterPolicy",
        "UnregisterPolicy",
        "GetPolicy",
        "ListPolicies",
        "WhatIfReplay",
        "GetDecisionReceipt",
        "GetReceiptKeys",
    ] {
        let http = rig.call(method, None, &json!({})).await;
        let grpc_unauthenticated =
            grpc_empty_call(&rig, method).await == Some(tonic::Code::Unauthenticated);
        assert_eq!(
            http.status == 401,
            grpc_unauthenticated,
            "{method}: {}",
            http.status
        );
    }
}

/// gRPC status code of `method` called with an empty request and no token
/// (`None` = OK).
async fn grpc_empty_call(rig: &Rig, method: &str) -> Option<tonic::Code> {
    let mut c = rig.grpc.clone();
    macro_rules! call {
        ($m:ident, $t:ty) => {
            c.$m(tonic::Request::new(<$t>::default()))
                .await
                .err()
                .map(|s| s.code())
        };
    }
    match method {
        "Initialize" => call!(initialize, pb::InitializeRequest),
        "GetSession" => call!(get_session, pb::GetSessionRequest),
        "CancelSession" => call!(cancel_session, pb::CancelSessionRequest),
        "SuspendSession" => call!(suspend_session, pb::SuspendSessionRequest),
        "ResumeSession" => call!(resume_session, pb::ResumeSessionRequest),
        "GetManifest" => call!(get_manifest, pb::GetManifestRequest),
        "ListModes" => call!(list_modes, pb::ListModesRequest),
        "ListRoots" => call!(list_roots, pb::ListRootsRequest),
        "ListExtModes" => call!(list_ext_modes, pb::ListExtModesRequest),
        "RegisterExtMode" => call!(register_ext_mode, pb::RegisterExtModeRequest),
        "UnregisterExtMode" => call!(unregister_ext_mode, pb::UnregisterExtModeRequest),
        "PromoteMode" => call!(promote_mode, pb::PromoteModeRequest),
        "ListSessions" => call!(list_sessions, pb::ListSessionsRequest),
        "RegisterPolicy" => call!(register_policy, pb::RegisterPolicyRequest),
        "UnregisterPolicy" => call!(unregister_policy, pb::UnregisterPolicyRequest),
        "GetPolicy" => call!(get_policy, pb::GetPolicyRequest),
        "ListPolicies" => call!(list_policies, pb::ListPoliciesRequest),
        "WhatIfReplay" => call!(what_if_replay, pb::WhatIfReplayRequest),
        "GetDecisionReceipt" => call!(get_decision_receipt, pb::GetDecisionReceiptRequest),
        "GetReceiptKeys" => call!(get_receipt_keys, pb::GetReceiptKeysRequest),
        other => panic!("{other}"),
    }
}

/// Dev-mode auth (no auth configured) stays exactly as on gRPC: any
/// `Bearer <v>` authenticates as `<v>`, and no credential is refused.
#[tokio::test]
async fn dev_mode_auth_matches_grpc() {
    let rig = rig(Opts {
        tokens: None,
        ..Opts::default()
    })
    .await;
    let s = sid();
    let r = rig.send("agent://dev", start(&s, &["agent://dev"])).await;
    assert_eq!(r.ack()["ok"], true);
    let r = rig
        .call("GetSession", None, &json!({"session_id": s}))
        .await;
    assert_eq!(r.status, 401);
    let r = rig
        .post(
            "/macp/v1/GetSession",
            &[
                ("content-type", "application/json"),
                ("authorization", "bearer agent://dev"),
            ],
            serde_json::to_vec(&json!({"session_id": s})).unwrap(),
        )
        .await;
    assert_eq!(r.status, 401, "lowercase scheme refused in dev mode too");
    let r = rig
        .call("GetSession", Some("agent://dev"), &json!({"session_id": s}))
        .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["metadata"]["initiator"], "agent://dev");
}

/// No credential: 401 even when the body is malformed, carries an oversize
/// payload, or is over the body cap (the body is never read).
#[tokio::test]
async fn no_token_is_401_before_anything_about_the_body() {
    let rig = rig(Opts {
        max_payload: 512,
        ..Opts::default()
    })
    .await;
    let ct = [("content-type", "application/json")];
    // Malformed Envelope (and not JSON at all).
    for body in [
        serde_json::to_vec(&json!({"envelope": {"macp_version": "1.0", "payload_b64": "!!"}}))
            .unwrap(),
        b"not json".to_vec(),
        Vec::new(),
    ] {
        let r = rig.post("/macp/v1/Send", &ct, body).await;
        assert_eq!(r.status, 401);
    }
    // Oversize payload (converted bytes over the limit, body under the cap).
    let big = base64::engine::general_purpose::STANDARD.encode(vec![7u8; 600]);
    let mut e = proposal(&sid(), "m-big");
    e.as_object_mut().unwrap().remove("payload");
    e["payload_b64"] = json!(big);
    let r = rig
        .post(
            "/macp/v1/Send",
            &ct,
            serde_json::to_vec(&json!({"envelope": e})).unwrap(),
        )
        .await;
    assert_eq!(r.status, 401);
    // Over the body cap.
    let r = rig
        .post("/macp/v1/Send", &ct, vec![b' '; rig.max_body + 1])
        .await;
    assert_eq!(r.status, 401);

    // The body is never read: a request whose body never arrives is answered
    // at once (the request timeout here is 30 s), declared under or over the
    // cap.
    for len in [10, rig.max_body + 1] {
        let started = Instant::now();
        let (status, head, _) = raw(
            rig.http,
            format!(
                "POST /macp/v1/Send HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
                 Content-Length: {len}\r\n\r\n"
            ),
        )
        .await;
        assert_eq!(status, 401, "{head}");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            head.to_ascii_lowercase()
                .contains("www-authenticate: bearer"),
            "{head}"
        );
    }
}

/// Write `head` on a raw TCP connection (and nothing more) and read one
/// response: (status, head text, body).
async fn raw(addr: SocketAddr, head: String) -> (u16, String, Vec<u8>) {
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(head.as_bytes()).await.unwrap();
    read_response(&mut tcp, Duration::from_secs(10)).await
}

async fn read_response(tcp: &mut TcpStream, within: Duration) -> (u16, String, Vec<u8>) {
    tokio::time::timeout(within, async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let n = tcp.read(&mut chunk).await.unwrap();
            assert!(n > 0, "closed before a response head");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
        let status: u16 = head[9..12].parse().unwrap();
        let len = head
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = tcp.read(&mut chunk).await.unwrap();
            assert!(n > 0, "closed mid-body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = buf[head_end..head_end + len].to_vec();
        (status, head, body)
    })
    .await
    .expect("no response in time")
}

// ───────────────────────── body cap, malformed bodies ─────────────────────────

#[tokio::test]
async fn body_cap_and_malformed_bodies() {
    let rig = rig(Opts {
        max_payload: 512,
        ..Opts::default()
    })
    .await;
    let auth = [
        ("content-type", "application/json"),
        ("authorization", "Bearer tok-orch"),
    ];
    // 413: Content-Length over the cap, on Send and on another RPC.
    for path in ["/macp/v1/Send", "/macp/v1/ListSessions"] {
        let r = rig.post(path, &auth, vec![b' '; rig.max_body + 1]).await;
        assert_eq!(r.status, 413, "{path}");
        assert_eq!(r.error_code(), "PAYLOAD_TOO_LARGE");
    }
    // 413 for a chunked body with no Content-Length.
    let mut tcp = TcpStream::connect(rig.http).await.unwrap();
    tcp.write_all(
        b"POST /macp/v1/Send HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
          Authorization: Bearer tok-orch\r\nTransfer-Encoding: chunked\r\n\r\n",
    )
    .await
    .unwrap();
    let chunk = vec![b' '; rig.max_body + 1];
    tcp.write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
        .await
        .unwrap();
    tcp.write_all(&chunk).await.unwrap();
    tcp.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    let (status, _, body) = read_response(&mut tcp, Duration::from_secs(10)).await;
    assert_eq!(status, 413);
    check_error_body(&serde_json::from_slice(&body).unwrap()).unwrap();

    // 400: not JSON, not an object, empty, no envelope member, a bare Envelope.
    for body in [
        b"not json".to_vec(),
        b"[]".to_vec(),
        b"\"envelope\"".to_vec(),
        Vec::new(),
        b"{}".to_vec(),
        br#"{"envelope": null}"#.to_vec(),
        serde_json::to_vec(&proposal(&sid(), "bare")).unwrap(),
    ] {
        let r = rig.post("/macp/v1/Send", &auth, body.clone()).await;
        assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(&body));
        assert_eq!(r.error_code(), "INVALID_REQUEST");
    }
    // A JSON number above i64 (or any int32) for an integer field: 400.
    for page_size in ["99999999999999999999", "2147483648", "1.5"] {
        let body = format!(r#"{{"page_size": {page_size}}}"#);
        let r = rig.post("/macp/v1/ListSessions", &auth, body).await;
        assert_eq!(r.status, 400, "{page_size}");
        assert_eq!(r.error_code(), "INVALID_REQUEST");
    }
    // A page_token this runtime never issued: 400 (RFC-MACP-0006 §3.8).
    let r = rig
        .call(
            "ListSessions",
            Some("tok-orch"),
            &json!({"page_token": "nope"}),
        )
        .await;
    assert_eq!(r.status, 400);
    assert_eq!(r.error_code(), "INVALID_REQUEST");
    // A wrong type in a request field: 400.
    let r = rig
        .call("GetSession", Some("tok-orch"), &json!({"session_id": 7}))
        .await;
    assert_eq!(r.status, 400);
}

// ───────────────────────── Send: in-band decisions ─────────────────────────

#[tokio::test]
async fn send_decisions_are_in_band() {
    let rig = rig(Opts {
        max_payload: 512,
        ..Opts::default()
    })
    .await;
    let s = open_session(&rig, &[ORCH, "agent://security"]).await;

    // Spoofed sender: an Envelope decision, so 200 Ack UNAUTHENTICATED.
    let mut e = proposal(&s, "m-spoof");
    e["sender"] = json!("agent://security");
    let ack = rig.send("tok-orch", e).await.ack();
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["error"]["code"], "UNAUTHENTICATED");

    // Payload over the payload limit, body under the cap: in band.
    let mut e = proposal(&s, "m-big");
    e.as_object_mut().unwrap().remove("payload");
    e["payload_b64"] = json!(base64::engine::general_purpose::STANDARD.encode(vec![1u8; 600]));
    let ack = rig.send("tok-orch", e).await.ack();
    assert_eq!(ack["error"]["code"], "PAYLOAD_TOO_LARGE");

    // A payload the codec cannot convert: in band INVALID_ENVELOPE, with the
    // Envelope's identifiers.
    let mut e = proposal(&s, "m-badpayload");
    e["payload"] = json!({"proposal_id": {"nested": true}});
    let ack = rig.send("tok-orch", e).await.ack();
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["error"]["code"], "INVALID_ENVELOPE");
    assert_eq!(ack["message_id"], "m-badpayload");
    assert_eq!(ack["session_id"], s.as_str());
    assert_eq!(ack["error"]["session_id"], s.as_str());

    // A body with an `envelope` member is a SendRequest (§4.4): anything
    // wrong inside it is in band, identifiers copied when they are strings.
    let mut e = proposal(&s, "m-mode-type");
    e["mode"] = json!(1);
    let ack = rig.send("tok-orch", e).await.ack();
    assert_eq!(ack["error"]["code"], "INVALID_ENVELOPE");
    assert_eq!(ack["message_id"], "m-mode-type");
    assert_eq!(ack["session_id"], s.as_str());
    let mut e = proposal(&s, "m-nested-type");
    e["payload"]["option"] = json!(["not", "a", "string"]);
    let ack = rig.send("tok-orch", e).await.ack();
    assert_eq!(ack["error"]["code"], "INVALID_ENVELOPE");
    assert_eq!(ack["message_id"], "m-nested-type");
    for body in [
        r#"{"envelope": 5}"#,
        r#"{"envelope": "x"}"#,
        r#"{"envelope": []}"#,
    ] {
        let r = rig
            .post(
                "/macp/v1/Send",
                &[
                    ("content-type", "application/json"),
                    ("authorization", "Bearer tok-orch"),
                ],
                body,
            )
            .await;
        let ack = r.ack();
        assert_eq!(ack["error"]["code"], "INVALID_ENVELOPE", "{body}");
        assert_eq!(ack["message_id"], "", "{body}");
    }
    // `{"envelope": null}` is a 400: null is absent (RFC-MACP-0001 §10.10),
    // so the body has no envelope member (see `body_cap_and_malformed_bodies`).

    // Both payload forms: in band.
    let mut e = proposal(&s, "m-both");
    e["payload_b64"] = json!("AA==");
    assert_eq!(
        rig.send("tok-orch", e).await.ack()["error"]["code"],
        "INVALID_ENVELOPE"
    );

    // Wrong macp_version: in band (§4.7).
    let mut e = proposal(&s, "m-version");
    e["macp_version"] = json!("9.9");
    assert_eq!(
        rig.send("tok-orch", e).await.ack()["error"]["code"],
        "UNSUPPORTED_PROTOCOL_VERSION"
    );

    // Accepted, then a duplicate (§4.8 retry with the same message_id).
    let e = proposal(&s, "m-ok");
    let first = rig.send("tok-orch", e.clone()).await.ack();
    assert_eq!(first["ok"], true);
    assert_eq!(first["duplicate"], false);
    let again = rig.send("tok-orch", e).await.ack();
    assert_eq!(again["ok"], true);
    assert_eq!(again["duplicate"], true);
}

/// The SessionStart and message buckets are Envelope decisions: `200` Ack
/// `RATE_LIMITED` on HTTP, the same Ack on gRPC (shared buckets), never 429.
#[tokio::test]
async fn rate_limits_are_in_band_and_shared_with_grpc() {
    let rig = rig(Opts {
        message_limit: 2,
        start_limit: 2,
        ..Opts::default()
    })
    .await;
    // SessionStart bucket: two starts, the third refused on both bindings.
    let a = open_session(&rig, &[ORCH]).await;
    let _b = open_session(&rig, &[ORCH]).await;
    let third = start(&sid(), &[ORCH]);
    let ack = rig.send("tok-orch", third.clone()).await.ack();
    assert_eq!(ack["ok"], false);
    assert_eq!(ack["error"]["code"], "RATE_LIMITED");
    let grpc = rig.grpc_send("tok-orch", &third).await;
    assert_eq!(grpc.error.as_ref().unwrap().code, "RATE_LIMITED");
    assert_same_ack(&ack, &grpc);

    // Message bucket: one over HTTP, one over gRPC, then refused on both.
    assert_eq!(
        rig.send("tok-orch", proposal(&a, "p1")).await.ack()["ok"],
        true
    );
    assert!(rig.grpc_send("tok-orch", &proposal(&a, "p2")).await.ok);
    let over = proposal(&a, "p3");
    let ack = rig.send("tok-orch", over.clone()).await.ack();
    assert_eq!(ack["error"]["code"], "RATE_LIMITED");
    let grpc = rig.grpc_send("tok-orch", &over).await;
    assert_same_ack(&ack, &grpc);

    // Rate-limited and malformed: the Envelope is judged first.
    let mut bad = proposal(&a, "p4");
    bad["payload"] = json!({"option": 5});
    assert_eq!(
        rig.send("tok-orch", bad).await.ack()["error"]["code"],
        "INVALID_ENVELOPE"
    );
    let mut shapeless = proposal(&a, "p5");
    shapeless["message_type"] = json!("");
    assert_eq!(
        rig.send("tok-orch", shapeless).await.ack()["error"]["code"],
        "INVALID_ENVELOPE"
    );

    // A rate-limited WhatIfReplay is a gRPC RESOURCE_EXHAUSTED status, so a
    // request-level 429 here.
    let r = rig
        .call(
            "WhatIfReplay",
            Some("tok-orch"),
            &json!({"session_id": a, "candidate": {"policy_id": "c", "mode": DECISION, "rules": {}, "schema_version": 1}}),
        )
        .await;
    assert_eq!(r.status, 429);
    assert_eq!(r.header("macp-error-code"), Some("RATE_LIMITED"));
    assert_eq!(r.json()["error"]["session_id"], a.as_str());
}

/// A forbidden mode is decided before the bucket: `FORBIDDEN`, and the bucket
/// is not charged.
#[tokio::test]
async fn forbidden_mode_is_not_charged_to_the_bucket() {
    let rig = rig(Opts {
        message_limit: 1,
        ..Opts::default()
    })
    .await;
    let s = open_session(&rig, &[ORCH, "agent://decision-only"]).await;
    let other_mode = envelope(
        PROPOSAL,
        "Proposal",
        "x1",
        &s,
        json!({"proposal_id": "x", "option": "o", "rationale": "r"}),
    );
    for _ in 0..3 {
        let ack = rig
            .send("tok-decision-only", other_mode.clone())
            .await
            .ack();
        assert_eq!(ack["error"]["code"], "FORBIDDEN");
    }
    // The one message the bucket allows is still available.
    let ack = rig
        .send("tok-decision-only", proposal(&s, "d1"))
        .await
        .ack();
    assert_eq!(ack["ok"], true, "{ack}");
    // Now exhausted: an allowed mode is RATE_LIMITED, a forbidden one is
    // still FORBIDDEN (mode precedes the bucket).
    assert_eq!(
        rig.send("tok-decision-only", proposal(&s, "d2"))
            .await
            .ack()["error"]["code"],
        "RATE_LIMITED"
    );
    assert_eq!(
        rig.send("tok-decision-only", other_mode).await.ack()["error"]["code"],
        "FORBIDDEN"
    );
}

fn assert_same_ack(http: &Value, grpc: &pb::Ack) {
    let mut grpc_json = message_json(
        "Send",
        &pb::SendResponse {
            ack: Some(grpc.clone()),
        },
    )["ack"]
        .clone();
    let mut http = http.clone();
    normalize(&mut grpc_json);
    normalize(&mut http);
    assert_eq!(http, grpc_json);
}

// ───────────────────────── timeout (slow loris) ─────────────────────────

/// Headers sent, body trickled: the request ends within
/// `MACP_REQUEST_TIMEOUT_SECS` + 1 s, as `500 INTERNAL_ERROR`.
#[tokio::test]
async fn slow_loris_body_times_out() {
    let rig = rig(Opts {
        request_timeout_secs: 1,
        ..Opts::default()
    })
    .await;
    let mut tcp = TcpStream::connect(rig.http).await.unwrap();
    tcp.write_all(
        b"POST /macp/v1/Send HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
          Authorization: Bearer tok-orch\r\nContent-Length: 1000\r\n\r\n",
    )
    .await
    .unwrap();
    let started = Instant::now();
    let (mut rd, mut wr) = tcp.into_split();
    let trickle = tokio::spawn(async move {
        for _ in 0..100 {
            if wr.write_all(b" ").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    tokio::time::timeout(Duration::from_secs(5), async {
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = rd.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("no response to a trickled body");
    let elapsed = started.elapsed();
    trickle.abort();
    let head = String::from_utf8_lossy(&buf).into_owned();
    assert!(head.starts_with("HTTP/1.1 500"), "{head}");
    assert!(
        head.to_ascii_lowercase()
            .contains("macp-error-code: internal_error"),
        "{head}"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
}

// ───────────────────────── §4.6 / §4.7 ─────────────────────────

/// §4.7: nothing needs a prior `Initialize`, and `Initialize` keeps no
/// per-connection state.
#[tokio::test]
async fn initialize_is_stateless_and_never_required() {
    let rig = rig(Opts::default()).await;
    // A fresh connection's first request is a Send, then a GetSession on
    // another fresh connection: both succeed with no Initialize anywhere.
    let s = sid();
    let mut conn = rig.conn().await;
    let first = conn
        .post(
            "/macp/v1/Send",
            &[
                ("content-type", "application/json"),
                ("authorization", "Bearer tok-orch"),
            ],
            serde_json::to_vec(&json!({"envelope": start(&s, &[ORCH])})).unwrap(),
        )
        .await;
    assert_eq!(first.ack()["ok"], true);
    let r = rig
        .call("GetSession", Some("tok-orch"), &json!({"session_id": s}))
        .await;
    assert_eq!(r.status, 200);

    // A failed Initialize on a connection changes nothing for later requests
    // on it, and a successful one is the same on every connection.
    let init = json!({"supported_protocol_versions": ["1.0"]});
    let mut conn = rig.conn().await;
    let ct = [("content-type", "application/json")];
    let bad = conn
        .post(
            "/macp/v1/Initialize",
            &ct,
            serde_json::to_vec(&json!({"supported_protocol_versions": ["0.1"]})).unwrap(),
        )
        .await;
    assert_eq!(bad.status, 400);
    assert_eq!(bad.error_code(), "UNSUPPORTED_PROTOCOL_VERSION");
    let auth = [
        ("content-type", "application/json"),
        ("authorization", "Bearer tok-orch"),
    ];
    let after = conn
        .post(
            "/macp/v1/GetSession",
            &auth,
            serde_json::to_vec(&json!({"session_id": s})).unwrap(),
        )
        .await;
    assert_eq!(after.status, 200);
    let a = conn
        .post(
            "/macp/v1/Initialize",
            &ct,
            serde_json::to_vec(&init).unwrap(),
        )
        .await;
    let b = rig.call("Initialize", None, &init).await;
    assert_eq!(a.status, 200);
    assert_eq!(a.json(), b.json());
    assert_eq!(a.json()["selected_protocol_version"], "1.0");
}

/// §4.6: a `Cookie` header changes nothing, and no response sets one (the
/// latter asserted on every response by the shared client).
#[tokio::test]
async fn cookies_are_neutral() {
    let rig = rig(Opts::default()).await;
    let s = open_session(&rig, &[ORCH]).await;
    let body = serde_json::to_vec(&json!({"session_id": s})).unwrap();
    for auth in [Some("Bearer tok-orch"), None, Some("Bearer nope")] {
        let mut plain = vec![("content-type", "application/json")];
        if let Some(a) = auth {
            plain.push(("authorization", a));
        }
        let mut cookie = plain.clone();
        cookie.push(("cookie", "macp_session=abc; authorization=Bearer tok-orch"));
        let a = rig.post("/macp/v1/GetSession", &plain, body.clone()).await;
        let b = rig.post("/macp/v1/GetSession", &cookie, body.clone()).await;
        assert_eq!(a.status, b.status, "{auth:?}");
        let (mut ja, mut jb) = (a.json(), b.json());
        normalize(&mut ja);
        normalize(&mut jb);
        assert_eq!(ja, jb, "{auth:?}");
    }
}

/// Control-plane `409` on a retry passes through unchanged (§4.8).
#[tokio::test]
async fn control_plane_retry_conflict_passes_through() {
    let rig = rig(Opts::default()).await;
    let s = open_session(&rig, &[ORCH]).await;
    let req = json!({"session_id": s, "reason": "done"});
    let first = rig.call("CancelSession", Some("tok-orch"), &req).await;
    assert_eq!(first.status, 200, "{}", first.json());
    let retry = rig.call("CancelSession", Some("tok-orch"), &req).await;
    let grpc = rig
        .grpc
        .clone()
        .cancel_session(with_token(
            Some("tok-orch"),
            pb::CancelSessionRequest {
                session_id: s.clone(),
                reason: "done".into(),
            },
        ))
        .await;
    match grpc {
        Ok(resp) => {
            assert_eq!(retry.status, 200);
            assert_eq!(
                normalized(retry.json()),
                normalized(message_json("CancelSession", resp.get_ref()))
            );
        }
        Err(status) => {
            let code = status
                .metadata()
                .get("macp-error-code")
                .unwrap()
                .to_str()
                .unwrap();
            assert_eq!(retry.error_code(), code);
            assert_eq!(retry.status, 409, "{}", retry.json());
            assert_eq!(retry.json()["error"]["session_id"], s.as_str());
        }
    }
}

// ───────────────────────── golden parity ─────────────────────────

fn normalized(mut v: Value) -> Value {
    zero_times(&mut v);
    v
}

/// Zero (not drop) every `*_unix_ms` and `timestamp`, so a field present on
/// one side and absent on the other still differs.
fn zero_times(v: &mut Value) {
    match v {
        Value::Object(map) => {
            for (k, val) in map.iter_mut() {
                if k.ends_with("_unix_ms") {
                    *val = json!(0);
                } else if k == "timestamp" {
                    *val = json!("");
                } else {
                    zero_times(val);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(zero_times),
        _ => {}
    }
}

/// Replace every occurrence of a key of `renames` with its value, inside
/// every string (ids also appear inside refusal messages).
fn rename(v: &mut Value, renames: &[(&str, &str)]) {
    match v {
        Value::String(s) => {
            for (from, to) in renames {
                if s.contains(from) {
                    *s = s.replace(from, to);
                }
            }
        }
        Value::Object(map) => map.values_mut().for_each(|x| rename(x, renames)),
        Value::Array(items) => items.iter_mut().for_each(|x| rename(x, renames)),
        _ => {}
    }
}

/// The canonical JSON of the response `msg` of `method`.
fn message_json<M: prost::Message>(method: &str, msg: &M) -> Value {
    let mut d = DynamicMessage::new(method_descriptor(method).unwrap().output());
    d.transcode_from(msg).unwrap();
    encode_message(&d)
}

/// One parity case: the HTTP reply for `method` decodes to the same prost
/// message gRPC returned (timestamps zeroed, HTTP-side ids renamed onto the
/// gRPC side's), or both refuse with the same code, details and session.
async fn parity<M: prost::Message + Default>(
    rig: &Rig,
    method: &str,
    token: Option<&str>,
    http_request: Value,
    grpc: Result<tonic::Response<M>, tonic::Status>,
    renames: &[(&str, &str)],
) {
    let reply = rig.call(method, token, &http_request).await;
    match grpc {
        Ok(resp) => {
            assert_eq!(
                reply.status,
                200,
                "{method}: {}",
                String::from_utf8_lossy(&reply.body)
            );
            // The HTTP body decodes to the generated response type...
            let desc = method_descriptor(method).unwrap().output();
            let decoded: M = decode_message(&desc, &reply.json())
                .unwrap()
                .transcode_to()
                .unwrap();
            // ...equal to gRPC's, compared on their canonical forms.
            let mut http = message_json(method, &decoded);
            rename(&mut http, renames);
            assert_eq!(
                normalized(http),
                normalized(message_json(method, resp.get_ref())),
                "{method}"
            );
        }
        Err(status) => {
            let code = status
                .metadata()
                .get("macp-error-code")
                .unwrap_or_else(|| panic!("{method}: uncoded status {status:?}"))
                .to_str()
                .unwrap()
                .to_string();
            let body = reply.json();
            assert_eq!(body["error"]["code"], code.as_str(), "{method}");
            let details = status
                .metadata()
                .get_bin("macp-error-details-bin")
                .map(|v| v.to_bytes().unwrap().to_vec())
                .unwrap_or_default();
            assert_eq!(
                body["error"]["details"],
                base64::engine::general_purpose::STANDARD.encode(details),
                "{method}"
            );
            let mut session = body["error"]["session_id"].clone();
            rename(&mut session, renames);
            let want_session = http_request
                .get("session_id")
                .map(|s| {
                    let mut s = s.clone();
                    rename(&mut s, renames);
                    s
                })
                .unwrap_or(json!(""));
            assert_eq!(session, want_session, "{method}");
        }
    }
}

/// For every unary RPC, the HTTP response decodes to the same prost message
/// gRPC returns for the same request (both bindings on one `MacpServer`).
/// Read-only RPCs hit the same object; mutating ones hit twin objects set up
/// identically (`*-g` over gRPC, `*-h` over HTTP), whose ids are renamed
/// before comparing.
#[tokio::test]
async fn golden_parity_with_grpc() {
    let rig = rig(Opts::default()).await;
    let g = rig.grpc.clone();
    let t = Some("tok-orch");
    let mut covered = BTreeSet::new();
    macro_rules! case {
        ($name:literal, $http:expr, $call:ident, $req:expr) => {
            case!($name, t, $http, $call, $req, &[])
        };
        ($name:literal, $tok:expr, $http:expr, $call:ident, $req:expr, $renames:expr) => {{
            covered.insert($name);
            let grpc = g.clone().$call(with_token($tok, $req)).await;
            parity(&rig, $name, $tok, $http, grpc, $renames).await;
        }};
    }

    // Discovery.
    let init = json!({"supported_protocol_versions": ["1.0"]});
    case!(
        "Initialize",
        None,
        init,
        initialize,
        pb::InitializeRequest {
            supported_protocol_versions: vec!["1.0".into()],
            ..Default::default()
        },
        &[]
    );
    case!(
        "Initialize",
        None,
        json!({}),
        initialize,
        pb::InitializeRequest::default(),
        &[]
    );
    case!(
        "GetManifest",
        None,
        json!({}),
        get_manifest,
        pb::GetManifestRequest::default(),
        &[]
    );
    case!(
        "GetManifest",
        None,
        json!({"agent_id": "nope"}),
        get_manifest,
        pb::GetManifestRequest {
            agent_id: "nope".into()
        },
        &[]
    );
    case!(
        "ListModes",
        None,
        json!({}),
        list_modes,
        pb::ListModesRequest::default(),
        &[]
    );
    case!(
        "ListRoots",
        None,
        json!({}),
        list_roots,
        pb::ListRootsRequest::default(),
        &[]
    );
    case!(
        "ListExtModes",
        None,
        json!({}),
        list_ext_modes,
        pb::ListExtModesRequest::default(),
        &[]
    );
    case!(
        "GetReceiptKeys",
        None,
        json!({}),
        get_receipt_keys,
        pb::GetReceiptKeysRequest::default(),
        &[]
    );

    // Send: twin SessionStarts, then a refused Proposal on each.
    let (sg, sh) = (sid(), sid());
    let start_g = start(&sg, &[ORCH, "agent://security"]);
    let start_h = start(&sh, &[ORCH, "agent://security"]);
    let renames_s: Vec<(&str, &str)> = vec![(sh.as_str(), sg.as_str())];
    let mut start_h_named = start_h.clone();
    start_h_named["message_id"] = start_g["message_id"].clone();
    case!(
        "Send",
        t,
        json!({"envelope": start_h_named}),
        send,
        pb::SendRequest {
            envelope: Some(envelope_from_json(&start_g).unwrap())
        },
        &renames_s
    );
    let out_g = proposal(&sg, "m-out");
    let out_h = proposal(&sh, "m-out");
    case!(
        "Send",
        Some("tok-outsider"),
        json!({"envelope": out_h}),
        send,
        pb::SendRequest {
            envelope: Some(envelope_from_json(&out_g).unwrap())
        },
        &renames_s
    );

    // Session reads, on the same session.
    case!(
        "GetSession",
        json!({"session_id": sg}),
        get_session,
        pb::GetSessionRequest {
            session_id: sg.clone()
        }
    );
    case!(
        "GetSession",
        json!({"session_id": "99999999-9999-4999-8999-999999999999"}),
        get_session,
        pb::GetSessionRequest {
            session_id: "99999999-9999-4999-8999-999999999999".into()
        }
    );
    case!(
        "GetSession",
        Some("tok-outsider"),
        json!({"session_id": sg}),
        get_session,
        pb::GetSessionRequest {
            session_id: sg.clone()
        },
        &[]
    );
    case!(
        "ListSessions",
        json!({"page_size": 1}),
        list_sessions,
        pb::ListSessionsRequest {
            page_size: 1,
            page_token: String::new()
        }
    );
    case!(
        "ListSessions",
        json!({"page_token": "bogus"}),
        list_sessions,
        pb::ListSessionsRequest {
            page_size: 0,
            page_token: "bogus".into()
        }
    );
    case!(
        "GetDecisionReceipt",
        json!({"session_id": sg}),
        get_decision_receipt,
        pb::GetDecisionReceiptRequest {
            session_id: sg.clone()
        }
    );

    // A resolved session's receipt: issued once, so both bindings read the
    // same bytes; `receipt_json` stays a JSON string on HTTP.
    let r = sid();
    rig.send("tok-orch", start(&r, &[ORCH])).await.ack();
    for e in [
        proposal(&r, "rp"),
        envelope(
            DECISION,
            "Vote",
            "rv",
            &r,
            json!({"proposal_id": "rp", "vote": "APPROVE", "reason": "ok"}),
        ),
        envelope(
            DECISION,
            "Commitment",
            "rc",
            &r,
            json!({
                "commitment_id": "c1", "action": "decision.selected", "authority_scope": "s",
                "reason": "r", "mode_version": "1.0.0", "policy_version": "policy.default",
                "configuration_version": "config.default", "outcome_positive": true
            }),
        ),
    ] {
        let ack = rig.send("tok-orch", e).await.ack();
        assert_eq!(ack["ok"], true, "{ack}");
    }
    case!(
        "GetDecisionReceipt",
        json!({"session_id": r}),
        get_decision_receipt,
        pb::GetDecisionReceiptRequest {
            session_id: r.clone()
        }
    );
    let receipt = rig
        .call("GetDecisionReceipt", t, &json!({"session_id": r}))
        .await
        .json();
    assert!(receipt["receipt_json"].is_string(), "{receipt}");

    // WhatIfReplay (read-only) on the same session, and an invalid candidate.
    let candidate = json!({"policy_id": "cand", "mode": DECISION, "description": "", "rules": {}, "schema_version": 1});
    let candidate_pb = pb::PolicyDescriptor {
        policy_id: "cand".into(),
        mode: DECISION.into(),
        rules: "{}".into(),
        schema_version: 1,
        ..Default::default()
    };
    case!(
        "WhatIfReplay",
        json!({"session_id": r, "candidate": candidate}),
        what_if_replay,
        pb::WhatIfReplayRequest {
            session_id: r.clone(),
            candidate: Some(candidate_pb.clone())
        }
    );
    case!(
        "WhatIfReplay",
        json!({"session_id": r}),
        what_if_replay,
        pb::WhatIfReplayRequest {
            session_id: r.clone(),
            candidate: None
        }
    );

    // Session control: twin sessions.
    let (ug, uh) = (sid(), sid());
    for s in [&ug, &uh] {
        rig.send("tok-orch", start(s, &[ORCH])).await.ack();
    }
    let renames_u: Vec<(&str, &str)> = vec![(uh.as_str(), ug.as_str())];
    case!(
        "SuspendSession",
        t,
        json!({"session_id": uh, "reason": "pause"}),
        suspend_session,
        pb::SuspendSessionRequest {
            session_id: ug.clone(),
            reason: "pause".into()
        },
        &renames_u
    );
    case!(
        "ResumeSession",
        t,
        json!({"session_id": uh, "reason": "go"}),
        resume_session,
        pb::ResumeSessionRequest {
            session_id: ug.clone(),
            reason: "go".into()
        },
        &renames_u
    );
    case!(
        "ResumeSession",
        t,
        json!({"session_id": uh, "reason": "again"}),
        resume_session,
        pb::ResumeSessionRequest {
            session_id: ug.clone(),
            reason: "again".into()
        },
        &renames_u
    );
    case!(
        "CancelSession",
        Some("tok-security"),
        json!({"session_id": uh}),
        cancel_session,
        pb::CancelSessionRequest {
            session_id: ug.clone(),
            reason: String::new()
        },
        &renames_u
    );
    case!(
        "CancelSession",
        t,
        json!({"session_id": uh, "reason": "stop"}),
        cancel_session,
        pb::CancelSessionRequest {
            session_id: ug.clone(),
            reason: "stop".into()
        },
        &renames_u
    );

    // Policy registry: twin policies.
    let descriptor = |id: &str| json!({"policy_id": id, "mode": DECISION, "description": "d", "rules": {"voting": {"algorithm": "majority", "threshold": 0.5}}, "schema_version": 1});
    let descriptor_pb = |id: &str| pb::PolicyDescriptor {
        policy_id: id.into(),
        mode: DECISION.into(),
        description: "d".into(),
        rules: r#"{"voting":{"algorithm":"majority","threshold":0.5}}"#.into(),
        schema_version: 1,
        ..Default::default()
    };
    let renames_p: &[(&str, &str)] = &[("pol-h", "pol-g")];
    case!(
        "RegisterPolicy",
        t,
        json!({"policy_descriptor": descriptor("pol-h")}),
        register_policy,
        pb::RegisterPolicyRequest {
            policy_descriptor: Some(descriptor_pb("pol-g"))
        },
        renames_p
    );
    case!(
        "RegisterPolicy",
        t,
        json!({"policy_descriptor": descriptor("pol-h")}),
        register_policy,
        pb::RegisterPolicyRequest {
            policy_descriptor: Some(descriptor_pb("pol-g"))
        },
        renames_p
    );
    case!(
        "RegisterPolicy",
        Some("tok-security"),
        json!({"policy_descriptor": descriptor("x")}),
        register_policy,
        pb::RegisterPolicyRequest {
            policy_descriptor: Some(descriptor_pb("x"))
        },
        &[]
    );
    case!(
        "GetPolicy",
        json!({"policy_id": "pol-g"}),
        get_policy,
        pb::GetPolicyRequest {
            policy_id: "pol-g".into()
        }
    );
    case!(
        "GetPolicy",
        json!({"policy_id": "nope"}),
        get_policy,
        pb::GetPolicyRequest {
            policy_id: "nope".into()
        }
    );
    case!(
        "ListPolicies",
        json!({"mode": DECISION}),
        list_policies,
        pb::ListPoliciesRequest {
            mode: DECISION.into()
        }
    );
    case!(
        "UnregisterPolicy",
        t,
        json!({"policy_id": "pol-h"}),
        unregister_policy,
        pb::UnregisterPolicyRequest {
            policy_id: "pol-g".into()
        },
        renames_p
    );
    case!(
        "UnregisterPolicy",
        t,
        json!({"policy_id": "pol-h"}),
        unregister_policy,
        pb::UnregisterPolicyRequest {
            policy_id: "pol-g".into()
        },
        renames_p
    );

    // Extension modes: twin modes.
    let mode = |name: &str| {
        json!({"mode_descriptor": {
            "mode": name, "mode_version": "1.0.0", "title": "t", "description": "d",
            "determinism_class": "semantic-deterministic", "participant_model": "peer",
            "message_types": ["Contribute"], "terminal_message_types": ["Commitment"], "schema_uris": {}
        }})
    };
    let mode_pb = |name: &str| pb::RegisterExtModeRequest {
        mode_descriptor: Some(pb::ModeDescriptor {
            mode: name.into(),
            mode_version: "1.0.0".into(),
            title: "t".into(),
            description: "d".into(),
            determinism_class: "semantic-deterministic".into(),
            participant_model: "peer".into(),
            message_types: vec!["Contribute".into()],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: HashMap::new(),
        }),
    };
    let renames_m: &[(&str, &str)] = &[
        ("ext.parity-h.v1", "ext.parity-g.v1"),
        ("ext.promo-h.v1", "ext.promo-g.v1"),
    ];
    case!(
        "RegisterExtMode",
        t,
        mode("ext.parity-h.v1"),
        register_ext_mode,
        mode_pb("ext.parity-g.v1"),
        renames_m
    );
    case!(
        "RegisterExtMode",
        t,
        mode("ext.parity-h.v1"),
        register_ext_mode,
        mode_pb("ext.parity-g.v1"),
        renames_m
    );
    case!(
        "RegisterExtMode",
        Some("tok-security"),
        mode("ext.x.v1"),
        register_ext_mode,
        mode_pb("ext.x.v1"),
        &[]
    );
    case!(
        "ListExtModes",
        None,
        json!({}),
        list_ext_modes,
        pb::ListExtModesRequest::default(),
        &[]
    );
    case!(
        "UnregisterExtMode",
        t,
        json!({"mode": "ext.parity-h.v1"}),
        unregister_ext_mode,
        pb::UnregisterExtModeRequest {
            mode: "ext.parity-g.v1".into()
        },
        renames_m
    );
    case!(
        "UnregisterExtMode",
        t,
        json!({"mode": "ext.parity-h.v1"}),
        unregister_ext_mode,
        pb::UnregisterExtModeRequest {
            mode: "ext.parity-g.v1".into()
        },
        renames_m
    );
    case!(
        "RegisterExtMode",
        t,
        mode("ext.promo-h.v1"),
        register_ext_mode,
        mode_pb("ext.promo-g.v1"),
        renames_m
    );
    case!(
        "PromoteMode",
        t,
        json!({"mode": "ext.promo-h.v1"}),
        promote_mode,
        pb::PromoteModeRequest {
            mode: "ext.promo-g.v1".into(),
            promoted_mode_name: String::new()
        },
        renames_m
    );
    case!(
        "PromoteMode",
        t,
        json!({"mode": "ext.promo-h.v1"}),
        promote_mode,
        pb::PromoteModeRequest {
            mode: "ext.promo-g.v1".into(),
            promoted_mode_name: String::new()
        },
        renames_m
    );

    // Every unary RPC of the service is covered.
    let unary: BTreeSet<String> = macp_runtime::http::json::descriptor_pool()
        .get_service_by_name(macp_runtime::http::json::SERVICE_NAME)
        .unwrap()
        .methods()
        .filter(|m| !m.is_client_streaming() && !m.is_server_streaming())
        .map(|m| m.name().to_string())
        .collect();
    assert_eq!(
        covered
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        unary
    );
    assert_eq!(unary.len(), 21);
}

/// Streaming RPCs are P6: until then a clear `501` with no body.
#[tokio::test]
async fn streaming_rpcs_are_placeholders_until_p6() {
    let rig = rig(Opts::default()).await;
    for method in [
        "StreamSession",
        "WatchModeRegistry",
        "WatchRoots",
        "WatchSignals",
        "WatchSessions",
        "WatchPolicies",
    ] {
        let r = rig.call(method, Some("tok-orch"), &json!({})).await;
        assert_eq!(r.status, 501, "{method}");
        assert!(r.body.is_empty());
    }
}
