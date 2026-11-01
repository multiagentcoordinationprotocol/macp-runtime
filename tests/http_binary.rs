//! Real-binary checks of the HTTP listener skeleton (http-listener P4).
//!
//! In every build:
//! - **zero impact**: without `MACP_HTTP_BIND_ADDR` the binary listens on the
//!   gRPC port only, serves a `GetManifest` byte-identical to the in-process
//!   server's, and logs nothing about HTTP.
//!
//! Default build (`http-listener` off): `MACP_HTTP_BIND_ADDR` set is refused
//! at startup. `CARGO_BIN_EXE_macp-runtime` is the binary built with the test
//! run's own features, so this case runs in the default-feature test run
//! (`cargo test --workspace`) and the listener cases below in the
//! `--features http-listener` run (CI `features` job).
//!
//! `http-listener` build: TLS with HTTP/1.1 and HTTP/2 (ALPN), plaintext
//! refused without `MACP_ALLOW_INSECURE`, the §4.1/§4.2 transport statuses,
//! SIGTERM/SIGINT drain with the listener on (and the drain deadline), the
//! HTTP/2 stream bound, and every bad `MACP_HTTP_*` value refused.
//!
//! Waits are readiness polls, never fixed sleeps. Signals are delivered with
//! `kill`, so the file is Unix-only.
#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

use prost::Message;

#[path = "support/binary_child.rs"]
mod binary_child;
use binary_child::{addr_in_use, field, free_port, Child, SPAWN_ATTEMPTS};

/// Spawn the binary with a scrubbed environment, dev-mode defaults
/// (`MACP_ALLOW_INSECURE=1`, memory only) and `env` applied on top; an entry
/// with an empty value removes the variable.
fn spawn(scratch: &std::path::Path, grpc: &str, env: &[(&str, String)]) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_macp-runtime"));
    command.env_clear();
    if let Ok(path) = std::env::var("PATH") {
        command.env("PATH", path);
    }
    command
        .current_dir(scratch)
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", grpc)
        .env("MACP_SHUTDOWN_DRAIN_SECS", "5")
        .env("RUST_LOG", "info");
    for (k, v) in env {
        if v.is_empty() {
            command.env_remove(k);
        } else {
            command.env(k, v);
        }
    }
    Child::spawn(&mut command)
}

/// A running binary whose listeners are bound: `grpc` (a probed port, held
/// by the child) and, with `MACP_HTTP_BIND_ADDR=127.0.0.1:0`, the `http`
/// address read back from its `HTTP listener bound` line.
struct Launched {
    child: Child,
    grpc: String,
    #[cfg_attr(not(feature = "http-listener"), allow(dead_code))]
    http: Option<String>,
}

/// Spawn and wait until the child itself listens on its gRPC port (and has
/// logged its HTTP address when `with_http`). A gRPC port taken between the
/// probe and the bind (`AddrInUse`) is retried with a fresh port, at most
/// `SPAWN_ATTEMPTS` times; any other early exit panics with the output.
fn launch(scratch: &std::path::Path, env: &[(&str, String)], with_http: bool) -> Launched {
    let mut env = env.to_vec();
    if with_http {
        env.push(("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string()));
    }
    for attempt in 1..=SPAWN_ATTEMPTS {
        let port = free_port();
        let grpc = format!("127.0.0.1:{port}");
        let mut child = spawn(scratch, &grpc, &env);
        let retry = |out: &str| {
            assert!(
                addr_in_use(out) && attempt < SPAWN_ATTEMPTS,
                "runtime exited before ready (attempt {attempt}); output:\n{out}"
            );
        };
        let http = if with_http {
            match child.wait_line("HTTP listener bound", Duration::from_secs(30)) {
                Ok(line) => Some(field(&line, "addr").expect("addr= in the bound line")),
                Err(out) => {
                    retry(&out);
                    continue;
                }
            }
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let bound = loop {
            if listening_ports(child.pid()).contains(&port) {
                break true;
            }
            if !child.alive() {
                break false;
            }
            assert!(
                Instant::now() < deadline,
                "gRPC port never bound; output:\n{}",
                child.output()
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        if bound {
            return Launched { child, grpc, http };
        }
        retry(&child.output());
    }
    unreachable!("the last attempt panics")
}

/// Spawn with `env` and expect a startup refusal: non-zero exit, output
/// returned.
fn expect_refusal(env: &[(&str, String)]) -> String {
    let scratch = tempfile::tempdir().unwrap();
    let grpc = format!("127.0.0.1:{}", free_port());
    let mut child = spawn(scratch.path(), &grpc, env);
    let status = child
        .wait_exit(Duration::from_secs(30))
        .unwrap_or_else(|| panic!("{env:?}: still running; expected a startup refusal"));
    let out = child.output();
    assert!(!status.success(), "{env:?}: exited 0; output:\n{out}");
    out
}

async fn wait_grpc_serving(child: &mut Child, addr: &str) {
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !child.alive() {
            panic!("runtime exited before SERVING; output:\n{}", child.output());
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
                if resp.into_inner().status == ServingStatus::Serving as i32 {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    child.kill();
    panic!(
        "runtime never reported SERVING; output:\n{}",
        child.output()
    );
}

/// The TCP ports `pid` is listening on: `lsof`, or `/proc` on Linux without it.
fn listening_ports(pid: u32) -> Vec<u16> {
    let lsof = Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ])
        .output();
    if let Ok(out) = lsof {
        // lsof exits 1 when nothing matches; any listing it printed is valid.
        let text = String::from_utf8_lossy(&out.stdout);
        let mut ports: Vec<u16> = text
            .lines()
            .filter_map(|l| l.strip_prefix('n'))
            .filter_map(|n| n.rsplit(':').next()?.parse().ok())
            .collect();
        ports.sort_unstable();
        ports.dedup();
        return ports;
    }
    proc_listening_ports(pid)
}

/// `/proc/<pid>/fd` socket inodes joined with `/proc/<pid>/net/tcp{,6}`
/// rows in state `0A` (LISTEN).
fn proc_listening_ports(pid: u32) -> Vec<u16> {
    let inodes: Vec<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("neither lsof nor /proc is available")
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .filter_map(|l| {
            let l = l.to_string_lossy().to_string();
            Some(l.strip_prefix("socket:[")?.strip_suffix(']')?.to_string())
        })
        .collect();
    let mut ports = Vec::new();
    for table in ["tcp", "tcp6"] {
        let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")) else {
            continue;
        };
        for row in text.lines().skip(1) {
            let cols: Vec<&str> = row.split_whitespace().collect();
            if cols.len() > 9 && cols[3] == "0A" && inodes.iter().any(|i| i == cols[9]) {
                if let Some(port) = cols[1]
                    .rsplit(':')
                    .next()
                    .and_then(|p| u16::from_str_radix(p, 16).ok())
                {
                    ports.push(port);
                }
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Without `MACP_HTTP_BIND_ADDR` (in either build): only the gRPC port is
/// listening, `GetManifest` is byte-identical to the in-process server's
/// (`transport_endpoints` empty), and no line mentions HTTP.
#[tokio::test(flavor = "multi_thread")]
async fn zero_impact_without_bind_addr() {
    use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
    use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeService;
    use macp_runtime::pb::GetManifestRequest;

    let scratch = tempfile::tempdir().unwrap();
    let Launched {
        mut child, grpc, ..
    } = launch(scratch.path(), &[], false);
    wait_grpc_serving(&mut child, &grpc).await;
    let port: u16 = grpc.rsplit(':').next().unwrap().parse().unwrap();

    assert_eq!(listening_ports(child.pid()), vec![port]);

    let mut client = MacpRuntimeServiceClient::connect(format!("http://{grpc}"))
        .await
        .unwrap();
    let wire = client
        .get_manifest(GetManifestRequest::default())
        .await
        .unwrap()
        .into_inner();
    assert!(wire
        .manifest
        .as_ref()
        .unwrap()
        .transport_endpoints
        .is_empty());

    let runtime = std::sync::Arc::new(macp_runtime::runtime::Runtime::new(
        std::sync::Arc::new(macp_runtime::storage::MemoryBackend),
        std::sync::Arc::new(macp_runtime::registry::SessionRegistry::new()),
        std::sync::Arc::new(macp_runtime::log_store::LogStore::new()),
    ));
    let local = macp_runtime::server::MacpServer::new(
        runtime,
        macp_runtime::security::SecurityLayer::dev_mode(),
    )
    .get_manifest(tonic::Request::new(GetManifestRequest::default()))
    .await
    .unwrap()
    .into_inner();
    assert_eq!(wire.encode_to_vec(), local.encode_to_vec());

    child.signal("INT");
    let status = child
        .wait_exit(Duration::from_secs(10))
        .expect("did not exit after SIGINT");
    let out = child.output();
    assert!(status.success(), "exit {status}; output:\n{out}");
    for needle in ["HTTP", "MACP_HTTP", "http-listener"] {
        assert!(!out.contains(needle), "'{needle}' logged:\n{out}");
    }
}

/// Built without the feature: a configuration that asks for the listener is
/// refused, naming the feature.
#[cfg(not(feature = "http-listener"))]
#[test]
fn feature_off_refuses_bind_addr() {
    let out = expect_refusal(&[("MACP_HTTP_BIND_ADDR", "127.0.0.1:0".to_string())]);
    assert!(
        out.contains("MACP_HTTP_BIND_ADDR is set") && out.contains("`http-listener`"),
        "{out}"
    );
}

#[cfg(feature = "http-listener")]
mod listener {
    use super::*;

    use bytes::Bytes;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::client::conn::{http1, http2};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use std::sync::Arc;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_rustls::rustls;

    struct Http {
        child: Child,
        http: String,
        _scratch: tempfile::TempDir,
    }

    /// Spawn with the HTTP listener on and return once it answers HTTP/1.1
    /// (plaintext) or once `ready` succeeds.
    async fn start_plain(env: &[(&str, String)]) -> Http {
        let scratch = tempfile::tempdir().unwrap();
        let Launched { child, http, .. } = launch(scratch.path(), env, true);
        let mut rt = Http {
            child,
            http: http.unwrap(),
            _scratch: scratch,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if !rt.child.alive() {
                panic!(
                    "runtime exited before ready; output:\n{}",
                    rt.child.output()
                );
            }
            if let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(1), TcpStream::connect(&rt.http)).await
            {
                if let Ok(Ok((status, _, _))) = tokio::time::timeout(
                    Duration::from_secs(2),
                    request_h1(stream, "GET", "/macp/v1/Send", None, ""),
                )
                .await
                {
                    assert_eq!(status, 405);
                    return rt;
                }
            }
            assert!(Instant::now() < deadline, "HTTP listener never answered");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// One HTTP/1.1 request on `io`.
    async fn request_h1<I>(
        io: I,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &'static str,
    ) -> Result<(u16, http::HeaderMap, Bytes), hyper::Error>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (mut send, conn) = http1::handshake(TokioIo::new(io)).await?;
        tokio::spawn(conn);
        let mut b = http::Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        if let Some(ct) = content_type {
            b = b.header("content-type", ct);
        }
        let resp = send
            .send_request(
                b.body(Full::new(Bytes::from_static(body.as_bytes())))
                    .unwrap(),
            )
            .await?;
        let (parts, body) = resp.into_parts();
        let body = body.collect().await?.to_bytes();
        Ok((parts.status.as_u16(), parts.headers, body))
    }

    async fn plain(
        rt: &Http,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: &'static str,
    ) -> (u16, http::HeaderMap, Bytes) {
        let stream = TcpStream::connect(&rt.http).await.unwrap();
        request_h1(stream, method, path, content_type, body)
            .await
            .unwrap()
    }

    fn refuses(addr: &str) -> bool {
        std::net::TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500))
            .is_err()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transport_statuses() {
        let rt = start_plain(&[("MACP_HTTP_PUBLIC_URL", "http://localhost/api".to_string())]).await;
        let json = Some("application/json");

        let (s, h, b) = plain(&rt, "GET", "/macp/v1/Send", json, "").await;
        assert_eq!(s, 405);
        assert_eq!(h.get("allow").unwrap(), "POST");
        assert!(b.is_empty());

        let (s, h, b) = plain(&rt, "POST", "/macp/v1/Nope", json, "{}").await;
        assert_eq!(s, 404);
        assert!(h.get("allow").is_none());
        assert!(b.is_empty());

        for ct in [
            Some("text/plain"),
            None,
            Some("application/json; charset=latin1"),
            Some("application/macp-envelope+json"),
        ] {
            let (s, _, b) = plain(&rt, "POST", "/macp/v1/Send", ct, "{}").await;
            assert_eq!(s, 415, "{ct:?}");
            assert!(b.is_empty());
        }

        // Trailing slash and percent-encoding name no RPC.
        for path in ["/macp/v1/Send/", "/macp/v1/%53end", "/api/macp/v1/Send/"] {
            assert_eq!(plain(&rt, "POST", path, json, "{}").await.0, 404, "{path}");
        }

        // A valid request reaches its RPC, with and without the
        // MACP_HTTP_PUBLIC_URL prefix, and with charset=UTF-8: `Send` with no
        // credential is refused from the headers, `GetManifest` is open.
        for (path, want) in [
            ("/macp/v1/Send", 401),
            ("/api/macp/v1/Send", 401),
            ("/macp/v1/GetManifest", 200),
            ("/api/macp/v1/GetManifest", 200),
        ] {
            let (s, h, _) = plain(
                &rt,
                "POST",
                path,
                Some("application/json; charset=UTF-8"),
                "{}",
            )
            .await;
            assert_eq!(s, want, "{path}");
            assert_eq!(h.get("content-type").unwrap(), "application/json");
        }
        // CORS (P7) and discovery (P8) placeholders.
        assert_eq!(
            plain(&rt, "OPTIONS", "/macp/v1/Send", None, "").await.0,
            404
        );
        assert_eq!(
            plain(&rt, "GET", "/.well-known/macp.json", None, "")
                .await
                .0,
            404
        );
    }

    /// No TLS and no `MACP_ALLOW_INSECURE=1`: refused before anything binds,
    /// by the HTTP listener's own check.
    #[test]
    fn plaintext_refused_without_allow_insecure() {
        let http = "127.0.0.1:0".to_string();
        let out = expect_refusal(&[
            ("MACP_ALLOW_INSECURE", String::new()),
            ("MACP_HTTP_BIND_ADDR", http),
        ]);
        assert!(out.contains("the HTTP listener requires TLS"), "{out}");
    }

    /// Every bad `MACP_HTTP_*` value produces its own error, and a bind
    /// failure is fatal.
    #[test]
    fn bad_env_values_are_refused() {
        let http = || "127.0.0.1:0".to_string();
        let cases: Vec<(Vec<(&str, String)>, &str)> = vec![
            (
                vec![("MACP_HTTP_BIND_ADDR", "not-an-addr".into())],
                "MACP_HTTP_BIND_ADDR: 'not-an-addr' is not a valid socket address",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", "127.0.0.1:50051".into()),
                    ("MACP_BIND_ADDR", "127.0.0.1:50051".into()),
                ],
                "collides with the gRPC listener",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_PUBLIC_URL", "https://h.example/".into()),
                ],
                "MACP_HTTP_PUBLIC_URL: 'https://h.example/' must not end with a trailing slash",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_PUBLIC_URL", "https://h.example?x=1".into()),
                ],
                "must not carry a query or fragment",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_PUBLIC_URL", "h.example".into()),
                ],
                "is not an absolute http(s) URL",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_ALLOW_INSECURE", String::new()),
                    ("MACP_HTTP_PUBLIC_URL", "http://h.example".into()),
                ],
                "must use https unless MACP_ALLOW_INSECURE=1",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_CORS_ORIGINS", "*".into()),
                ],
                "MACP_HTTP_CORS_ORIGINS: '*' is refused",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_CORS_ORIGINS", "https://a.example/path".into()),
                ],
                "is not an origin",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_MAX_BODY_BYTES", "1000".into()),
                ],
                "MACP_HTTP_MAX_BODY_BYTES: 1000 is below the floor of 1463638",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_MAX_BODY_BYTES", "lots".into()),
                ],
                "MACP_HTTP_MAX_BODY_BYTES: 'lots' is not a valid positive integer",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_MAX_CONNECTIONS", "0".into()),
                ],
                "MACP_HTTP_MAX_CONNECTIONS: '0' is not a valid positive integer",
            ),
            (
                vec![
                    ("MACP_HTTP_BIND_ADDR", http()),
                    ("MACP_HTTP_MAX_CONNECTIONS", "many".into()),
                ],
                "MACP_HTTP_MAX_CONNECTIONS: 'many' is not a valid positive integer",
            ),
        ];
        for (env, needle) in cases {
            let out = expect_refusal(&env);
            assert!(
                out.contains(needle),
                "{env:?}: expected '{needle}' in:\n{out}"
            );
        }

        // An occupied port: fatal, unlike the metrics endpoint.
        let taken = binary_child::bind_listener("127.0.0.1:0");
        let addr = taken.local_addr().unwrap().to_string();
        let out = expect_refusal(&[("MACP_HTTP_BIND_ADDR", addr.clone())]);
        assert!(
            out.contains(&format!("MACP_HTTP_BIND_ADDR: cannot bind {addr}")),
            "{out}"
        );
    }

    // ---- TLS -------------------------------------------------------------

    fn tls_connector(
        cert_der: rustls::pki_types::CertificateDer<'static>,
        alpn: &[&[u8]],
    ) -> tokio_rustls::TlsConnector {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).unwrap();
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        tokio_rustls::TlsConnector::from(Arc::new(config))
    }

    async fn tls_connect(
        addr: &str,
        connector: &tokio_rustls::TlsConnector,
    ) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let tcp = TcpStream::connect(addr).await?;
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        connector.connect(name, tcp).await
    }

    /// With TLS, HTTP/1.1 and HTTP/2 both answer, each selected by ALPN.
    #[tokio::test(flavor = "multi_thread")]
    async fn tls_serves_http1_and_http2() {
        let scratch = tempfile::tempdir().unwrap();
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = scratch.path().join("cert.pem");
        let key = scratch.path().join("key.pem");
        std::fs::write(&cert, ck.cert.pem()).unwrap();
        std::fs::write(&key, ck.key_pair.serialize_pem()).unwrap();
        let cert_der = ck.cert.der().clone();

        let Launched {
            mut child, http, ..
        } = launch(
            scratch.path(),
            &[
                ("MACP_TLS_CERT_PATH", cert.display().to_string()),
                ("MACP_TLS_KEY_PATH", key.display().to_string()),
            ],
            true,
        );
        let http = http.unwrap();

        let h1 = tls_connector(cert_der.clone(), &[b"http/1.1"]);
        let deadline = Instant::now() + Duration::from_secs(30);
        let stream = loop {
            if !child.alive() {
                panic!("runtime exited before ready; output:\n{}", child.output());
            }
            if let Ok(Ok(s)) =
                tokio::time::timeout(Duration::from_secs(2), tls_connect(&http, &h1)).await
            {
                break s;
            }
            assert!(Instant::now() < deadline, "TLS listener never answered");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
        let (s, h, _) = request_h1(stream, "GET", "/macp/v1/Send", None, "")
            .await
            .unwrap();
        assert_eq!(s, 405);
        assert_eq!(h.get("allow").unwrap(), "POST");
        let stream = tls_connect(&http, &h1).await.unwrap();
        let (s, _, _) = request_h1(
            stream,
            "POST",
            "/macp/v1/GetManifest",
            Some("application/json"),
            "{}",
        )
        .await
        .unwrap();
        assert_eq!(s, 200);

        // HTTP/2, alone and preferred when both are offered.
        for offer in [&[&b"h2"[..]][..], &[b"http/1.1", b"h2"]] {
            let connector = tls_connector(cert_der.clone(), offer);
            let stream = tls_connect(&http, &connector).await.unwrap();
            assert_eq!(stream.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
            let (mut send, conn) = http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await
                .unwrap();
            tokio::spawn(conn);
            let resp = send
                .send_request(
                    http::Request::builder()
                        .method("GET")
                        .uri("https://localhost/macp/v1/Send")
                        .body(Empty::<Bytes>::new())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.version(), http::Version::HTTP_2);
            assert_eq!(resp.status(), 405);
            assert_eq!(resp.headers().get("allow").unwrap(), "POST");
        }

        // A plaintext client on the TLS port gets no HTTP answer.
        let stream = TcpStream::connect(&http).await.unwrap();
        assert!(tokio::time::timeout(
            Duration::from_secs(5),
            request_h1(stream, "GET", "/macp/v1/Send", None, "")
        )
        .await
        .expect("plaintext request on the TLS port hung")
        .is_err());
    }

    // ---- shutdown --------------------------------------------------------

    /// An HTTP/1.1 request held in flight: its head is sent with
    /// `Expect: 100-continue` and a (dev-mode) credential, and the server's
    /// `100 Continue` (hyper sends it when the handler first reads the body,
    /// which is after authentication) proves the handler is running and
    /// waiting for the body.
    async fn hold_request(addr: &str) -> TcpStream {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"POST /macp/v1/Send HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
                  Authorization: Bearer agent://held\r\n\
                  Content-Length: 2\r\nExpect: 100-continue\r\n\r\n",
            )
            .await
            .unwrap();
        let head = read_head(&mut stream).await;
        assert!(head.starts_with("HTTP/1.1 100"), "{head}");
        stream
    }

    /// Read one response head (through the blank line).
    async fn read_head(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        tokio::time::timeout(Duration::from_secs(10), async {
            while !buf.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await.unwrap() == 0 {
                    break;
                }
                buf.push(byte[0]);
            }
        })
        .await
        .expect("no response head");
        String::from_utf8_lossy(&buf).into_owned()
    }

    async fn wait_refused(rt: &mut Http) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !refuses(&rt.http) {
            assert!(
                rt.child.alive(),
                "runtime exited before the HTTP port closed"
            );
            assert!(
                Instant::now() < deadline,
                "HTTP port still accepting after the signal"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Signal with an HTTP request in flight: the listener stops accepting,
    /// the request still completes, and the process exits 0 well within the
    /// drain deadline once both listeners have drained.
    async fn drains_inflight_http(sig: &str) {
        const DRAIN: u64 = 5;
        let mut rt = start_plain(&[("MACP_SHUTDOWN_DRAIN_SECS", DRAIN.to_string())]).await;
        let mut held = hold_request(&rt.http).await;

        let signalled = Instant::now();
        rt.child.signal(sig);
        wait_refused(&mut rt).await;
        assert!(rt.child.alive(), "exited with an HTTP request in flight");

        held.write_all(b"{}").await.unwrap();
        let head = read_head(&mut held).await;
        // `{}` is not a SendRequest (no envelope): a request-level 400.
        assert!(head.starts_with("HTTP/1.1 400"), "{head}");

        let status = rt
            .child
            .wait_exit(Duration::from_secs(DRAIN))
            .unwrap_or_else(|| panic!("did not exit within {DRAIN} s of {sig}"));
        let out = rt.child.output();
        assert!(status.success(), "{sig}: exit {status}; output:\n{out}");
        assert!(signalled.elapsed() < Duration::from_secs(DRAIN), "{out}");
        assert!(out.contains(&format!("\"SIG{sig}\"")), "{out}");
        assert!(out.contains("HTTP listener closed"), "{out}");
        assert!(out.contains("all connections drained"), "{out}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sigterm_drains_http_listener() {
        drains_inflight_http("TERM").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sigint_drains_http_listener() {
        drains_inflight_http("INT").await;
    }

    /// An HTTP request still open at the deadline: the process exits at the
    /// drain deadline, not before.
    #[tokio::test(flavor = "multi_thread")]
    async fn http_request_open_at_deadline_exits_at_deadline() {
        const DRAIN: u64 = 2;
        let mut rt = start_plain(&[("MACP_SHUTDOWN_DRAIN_SECS", DRAIN.to_string())]).await;
        let _held = hold_request(&rt.http).await; // never released

        let signalled = Instant::now();
        rt.child.signal("TERM");
        wait_refused(&mut rt).await;
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

    // ---- HTTP/2 stream bound ----------------------------------------------

    fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
        let len = payload.len() as u32;
        let mut f = vec![(len >> 16) as u8, (len >> 8) as u8, len as u8, kind, flags];
        f.extend_from_slice(&stream.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    /// HPACK "literal header field without indexing, new name", no Huffman.
    fn hpack(headers: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, value) in headers {
            assert!(name.len() < 127 && value.len() < 127);
            out.push(0x00);
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            out.push(value.len() as u8);
            out.extend_from_slice(value.as_bytes());
        }
        out
    }

    /// A client that ignores `SETTINGS_MAX_CONCURRENT_STREAMS` and opens one
    /// stream beyond it: the extra stream is refused (`RST_STREAM`
    /// `REFUSED_STREAM`), the bounded ones are not. Raw frames, because a
    /// compliant client library would queue the extra stream itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn h2_streams_beyond_the_bound_are_refused() {
        const BOUND: u32 = 2;
        let rt = start_plain(&[("MACP_MAX_CONCURRENT_STREAMS", BOUND.to_string())]).await;
        let mut stream = TcpStream::connect(&rt.http).await.unwrap();

        let mut out = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        out.extend(frame(0x4, 0, 0, &[])); // SETTINGS
        let headers = hpack(&[
            (":method", "POST"),
            (":scheme", "http"),
            (":path", "/macp/v1/Send"),
            (":authority", "x"),
            ("content-type", "application/json"),
            // Authenticated (dev mode), so each handler goes on to read its body.
            ("authorization", "Bearer agent://h2"),
        ]);
        // BOUND + 1 streams, none ending: each handler waits for its body.
        for i in 0..=BOUND {
            out.extend(frame(0x1, 0x4, 1 + 2 * i, &headers)); // HEADERS, END_HEADERS
        }
        stream.write_all(&out).await.unwrap();

        let refused = 1 + 2 * BOUND;
        let mut advertised = None;
        let mut buf = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                while buf.len() >= 9 {
                    let len = (usize::from(buf[0]) << 16)
                        | (usize::from(buf[1]) << 8)
                        | usize::from(buf[2]);
                    if buf.len() < 9 + len {
                        break;
                    }
                    let kind = buf[3];
                    let flags = buf[4];
                    let id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
                    let payload = buf[9..9 + len].to_vec();
                    buf.drain(..9 + len);
                    match kind {
                        // SETTINGS (not an ACK): find MAX_CONCURRENT_STREAMS.
                        0x4 if flags & 0x1 == 0 => {
                            for s in payload.chunks(6) {
                                if s.len() == 6 && u16::from_be_bytes([s[0], s[1]]) == 0x3 {
                                    advertised = Some(u32::from_be_bytes([s[2], s[3], s[4], s[5]]));
                                }
                            }
                        }
                        0x3 => {
                            let code = u32::from_be_bytes(payload[..4].try_into().unwrap());
                            assert_eq!(id, refused, "stream {id} reset with {code}");
                            return code;
                        }
                        0x7 => panic!("GOAWAY instead of a stream refusal: {payload:?}"),
                        _ => {}
                    }
                }
                let mut chunk = [0u8; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0, "connection closed without a stream refusal");
                buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .expect("no RST_STREAM for the stream beyond the bound");
        assert_eq!(result, 0x7, "REFUSED_STREAM");
        assert_eq!(advertised, Some(BOUND));
    }
}
