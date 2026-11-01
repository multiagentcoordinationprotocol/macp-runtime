//! CORS (RFC-MACP-0006 §4.10) and discovery (§4.11, RFC-MACP-0005 §6 and
//! §7.1) of the HTTP binding, plan `plans/http-listener.md` P7 and P8,
//! `http-listener` feature.
//!
//! Each test drives a real `HttpListener` on `127.0.0.1:0` in this process
//! over real TCP (HTTP/1.1), and the discovery tests also serve the same
//! `MacpServer` over gRPC, wired as `main` wires them: the server gets
//! `with_http_endpoint(MACP_HTTP_PUBLIC_URL)` before it is cloned for the two
//! listeners. Static tokens, no process env, so the tests run in parallel;
//! every read is bounded.
#![cfg(feature = "http-listener")]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use macp_runtime::http::json::{encode_message, method_descriptor};
use macp_runtime::http::{HttpConfig, HttpListener};
use macp_runtime::log_store::LogStore;
use macp_runtime::pb;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use prost_reflect::DynamicMessage;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const TOKENS: &str = r#"[{"token": "tok-orch", "sender": "agent://orchestrator"}]"#;
const LISTED: &str = "https://app.example";
const BOUND: Duration = Duration::from_secs(10);

struct Rig {
    http: SocketAddr,
    grpc: MacpRuntimeServiceClient<Channel>,
}

struct Reply {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|v| v.to_str().unwrap())
    }

    fn cors_names(&self) -> Vec<&str> {
        let mut n: Vec<_> = self
            .headers
            .keys()
            .map(|k| k.as_str())
            .filter(|k| k.starts_with("access-control-"))
            .collect();
        n.sort();
        n
    }
}

async fn rig(vars: &[(&str, &str)]) -> Rig {
    let security = SecurityLayer::dev_mode()
        .with_static_tokens(TOKENS)
        .unwrap();
    let runtime = Arc::new(Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));
    let mut env: HashMap<String, String> = HashMap::from([
        ("MACP_HTTP_BIND_ADDR".into(), "127.0.0.1:0".into()),
        ("MACP_ALLOW_INSECURE".into(), "1".into()),
    ]);
    for (k, v) in vars {
        env.insert((*k).into(), (*v).into());
    }
    let config = HttpConfig::from_env(&move |k| env.get(k).cloned())
        .unwrap()
        .unwrap();
    // As `main` does it: before either listener gets a clone.
    let mut server = MacpServer::new(runtime, security);
    if let Some(base) = &config.public_url {
        server = server.with_http_endpoint(base.clone());
    }
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
    Rig { http, grpc }
}

impl Rig {
    /// One request on a fresh connection. The body is collected unless the
    /// response is an event stream (only its head is read; dropping the
    /// connection ends the stream).
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &'static str,
    ) -> Reply {
        tokio::time::timeout(BOUND, async {
            let tcp = TcpStream::connect(self.http).await.unwrap();
            let (mut send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
            tokio::spawn(conn);
            let mut b = http::Request::builder()
                .method(method)
                .uri(path)
                .header("host", "localhost");
            for (k, v) in headers {
                b = b.header(*k, *v);
            }
            let resp = send
                .send_request(
                    b.body(Full::new(Bytes::from_static(body.as_bytes())))
                        .unwrap(),
                )
                .await
                .unwrap();
            let (parts, body) = resp.into_parts();
            let stream = parts
                .headers
                .get("content-type")
                .is_some_and(|v| v == "text/event-stream");
            let body = if stream || method == "HEAD" {
                Bytes::new()
            } else {
                body.collect().await.unwrap().to_bytes()
            };
            Reply {
                status: parts.status.as_u16(),
                headers: parts.headers,
                body,
            }
        })
        .await
        .expect("response within the bound")
    }
}

// ───────────────────────── P7: CORS ─────────────────────────

/// §4.10 over real TCP: a listed preflight carries exactly the five
/// headers; an unlisted, `null` or absent origin gets `204` and no
/// `Access-Control-*` at all; actual responses to a listed origin (success,
/// request-level error, transport status, SSE stream, well-known document)
/// carry `Allow-Origin`, `Expose-Headers: MACP-Error-Code` and
/// `Vary: Origin`; nothing ever carries `Allow-Credentials`.
#[tokio::test]
async fn cors_over_real_tcp() {
    // Configured with a trailing slash and a default port: normalised.
    let rig = rig(&[(
        "MACP_HTTP_CORS_ORIGINS",
        "https://app.example/, http://localhost:3000",
    )])
    .await;
    let preflight = |origin: Option<&'static str>| {
        let mut h = vec![
            ("access-control-request-method", "POST"),
            (
                "access-control-request-headers",
                "authorization, content-type",
            ),
        ];
        if let Some(o) = origin {
            h.push(("origin", o));
        }
        h
    };

    for origin in [LISTED, "https://APP.example:443", "http://localhost:3000"] {
        let r = rig
            .request("OPTIONS", "/macp/v1/Send", &preflight(Some(origin)), "")
            .await;
        assert_eq!(r.status, 204, "{origin}");
        assert_eq!(
            r.cors_names(),
            [
                "access-control-allow-headers",
                "access-control-allow-methods",
                "access-control-allow-origin",
                "access-control-max-age",
            ],
            "{origin}"
        );
        assert_eq!(r.header("access-control-allow-origin"), Some(origin));
        assert_eq!(r.header("access-control-allow-methods"), Some("POST"));
        assert_eq!(
            r.header("access-control-allow-headers"),
            Some("Authorization, Content-Type, Last-Event-ID, traceparent, tracestate")
        );
        assert_eq!(r.header("access-control-max-age"), Some("600"));
        assert_eq!(r.header("vary"), Some("Origin"));
        assert!(r.body.is_empty());
    }
    for origin in [
        Some("https://evil.example"),
        Some("http://app.example"),
        Some("https://app.example:8443"),
        Some("null"),
        None,
    ] {
        let r = rig
            .request("OPTIONS", "/macp/v1/Send", &preflight(origin), "")
            .await;
        assert_eq!(r.status, 204, "{origin:?}");
        assert!(r.cors_names().is_empty(), "{origin:?}: {:?}", r.headers);
        assert!(r.body.is_empty());
    }

    let json = ("content-type", "application/json");
    let listed = ("origin", LISTED);
    let bearer = ("authorization", "Bearer tok-orch");
    let sse = ("accept", "text/event-stream");
    for (method, path, headers, want) in [
        ("POST", "/macp/v1/ListModes", vec![json, listed], 200),
        ("POST", "/macp/v1/Send", vec![json, listed], 401),
        (
            "POST",
            "/macp/v1/GetSession",
            vec![json, listed, bearer],
            404,
        ),
        ("GET", "/macp/v1/Send", vec![listed], 405),
        ("POST", "/macp/v1/Send", vec![listed], 415),
        ("POST", "/macp/v1/Nope", vec![json, listed], 404),
        (
            "POST",
            "/macp/v1/WatchSignals",
            vec![json, listed, bearer, sse],
            200,
        ),
        ("GET", "/.well-known/macp.json", vec![listed], 200),
    ] {
        let body = if path.ends_with("GetSession") {
            r#"{"session_id": "01HV0Q2A8W0Y2XQ5N1R8Y2K9B3"}"#
        } else {
            "{}"
        };
        let r = rig.request(method, path, &headers, body).await;
        assert_eq!(r.status, want, "{method} {path}");
        assert_eq!(
            r.cors_names(),
            [
                "access-control-allow-origin",
                "access-control-expose-headers"
            ],
            "{method} {path}"
        );
        assert_eq!(r.header("access-control-allow-origin"), Some(LISTED));
        assert_eq!(
            r.header("access-control-expose-headers"),
            Some("MACP-Error-Code")
        );
        assert_eq!(r.header("vary"), Some("Origin"));

        // The same request from an unlisted origin: same status, no CORS.
        let mut other = headers.clone();
        other.retain(|(k, _)| *k != "origin");
        other.push(("origin", "https://evil.example"));
        let r = rig.request(method, path, &other, body).await;
        assert_eq!(r.status, want, "{method} {path}");
        assert!(r.cors_names().is_empty(), "{method} {path}");
        // Still `Vary: Origin`, so a shared cache never serves this copy
        // (without Allow-Origin) to a listed origin.
        assert_eq!(r.header("vary"), Some("Origin"), "{method} {path}");
    }
}

/// The default (no `MACP_HTTP_CORS_ORIGINS`) is closed: every preflight is
/// denied and no response carries a CORS header.
#[tokio::test]
async fn empty_allowlist_is_closed() {
    let rig = rig(&[]).await;
    let r = rig
        .request(
            "OPTIONS",
            "/macp/v1/Send",
            &[
                ("origin", LISTED),
                ("access-control-request-method", "POST"),
            ],
            "",
        )
        .await;
    assert_eq!(r.status, 204);
    assert!(r.cors_names().is_empty(), "{:?}", r.headers);
    let r = rig
        .request(
            "POST",
            "/macp/v1/ListModes",
            &[("origin", LISTED), ("content-type", "application/json")],
            "{}",
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(r.cors_names().is_empty());
    assert!(r.header("vary").is_none());
}

// ───────────────────────── P8: discovery ─────────────────────────

fn manifest_json(manifest: &pb::AgentManifest) -> Value {
    let mut m = DynamicMessage::new(
        method_descriptor("GetManifest")
            .unwrap()
            .output()
            .get_field_by_name("manifest")
            .unwrap()
            .kind()
            .as_message()
            .unwrap()
            .clone(),
    );
    m.transcode_from(manifest).unwrap();
    encode_message(&m)
}

impl Rig {
    async fn grpc_manifest(&self) -> pb::AgentManifest {
        self.grpc
            .clone()
            .get_manifest(pb::GetManifestRequest::default())
            .await
            .unwrap()
            .into_inner()
            .manifest
            .unwrap()
    }

    async fn http_manifest(&self, path: &str) -> Value {
        let r = self
            .request("POST", path, &[("content-type", "application/json")], "{}")
            .await;
        assert_eq!(r.status, 200, "{path}");
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        v["manifest"].clone()
    }

    async fn well_known(&self) -> (Reply, Value) {
        let r = self.request("GET", "/.well-known/macp.json", &[], "").await;
        assert_eq!(r.status, 200);
        assert_eq!(
            r.header("content-type"),
            Some("application/macp-manifest+json")
        );
        let v = serde_json::from_slice(&r.body).unwrap();
        (r, v)
    }
}

/// With `MACP_HTTP_PUBLIC_URL` set (prefix included), gRPC `GetManifest`,
/// HTTP `GetManifest` (with and without the prefix) and the well-known
/// document all carry the same single `macp.transport.http.v1` entry, whose
/// `uri` is `{base}` with no `/macp/v1/`; the well-known document is at the
/// origin root only, unauthenticated, byte-equal to the canonical JSON of
/// the gRPC manifest, and `HEAD` returns its head.
#[tokio::test]
async fn transport_endpoint_on_both_bindings_and_well_known() {
    let base = "http://runtime.example.com:8080/api/macp";
    let rig = rig(&[("MACP_HTTP_PUBLIC_URL", base)]).await;

    let grpc = rig.grpc_manifest().await;
    assert_eq!(
        grpc.transport_endpoints,
        [pb::TransportEndpoint {
            transport: "macp.transport.http.v1".into(),
            uri: base.into(),
            content_types: vec!["application/json".into()],
            metadata: HashMap::new(),
        }]
    );
    let want = manifest_json(&grpc);
    assert_eq!(
        want["transport_endpoints"],
        json!([{
            "transport": "macp.transport.http.v1",
            "uri": base,
            "content_types": ["application/json"],
            "metadata": {}
        }])
    );
    for path in ["/macp/v1/GetManifest", "/api/macp/macp/v1/GetManifest"] {
        assert_eq!(rig.http_manifest(path).await, want, "{path}");
    }

    let (r, doc) = rig.well_known().await;
    assert_eq!(doc, want);
    assert_eq!(r.body, serde_json::to_vec(&want).unwrap());

    let head = rig.request("HEAD", "/.well-known/macp.json", &[], "").await;
    assert_eq!(head.status, 200);
    assert_eq!(
        head.header("content-type"),
        Some("application/macp-manifest+json")
    );
    assert_eq!(
        head.header("content-length"),
        Some(r.body.len().to_string().as_str())
    );

    // At the origin root only, never under the prefix.
    for path in [
        "/api/macp/.well-known/macp.json",
        "/.well-known/macp.json/",
        "/.well-known/macp",
    ] {
        assert_eq!(
            rig.request("GET", path, &[], "").await.status,
            404,
            "{path}"
        );
    }
    // GET and HEAD only.
    let r = rig
        .request(
            "POST",
            "/.well-known/macp.json",
            &[("content-type", "application/json")],
            "{}",
        )
        .await;
    assert_eq!(r.status, 405);
    assert_eq!(r.header("allow"), Some("GET, HEAD"));
    assert!(r.body.is_empty());
}

/// Without a public URL the listener still serves the well-known document,
/// and `transport_endpoints` is `[]` on both bindings.
#[tokio::test]
async fn no_public_url_no_entry() {
    let rig = rig(&[]).await;
    let grpc = rig.grpc_manifest().await;
    assert!(grpc.transport_endpoints.is_empty());
    let want = manifest_json(&grpc);
    assert_eq!(want["transport_endpoints"], json!([]));
    assert_eq!(rig.http_manifest("/macp/v1/GetManifest").await, want);
    let (_, doc) = rig.well_known().await;
    assert_eq!(doc, want);
}
