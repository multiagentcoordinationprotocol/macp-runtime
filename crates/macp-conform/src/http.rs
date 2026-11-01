//! The HTTP target: the Server profile over the HTTP/JSON binding
//! (RFC-MACP-0006 §4), against any runtime that serves it.
//!
//! Every RPC is `POST {base}/macp/v1/{Method}` with an `application/json`
//! body (§4.1, §4.2), authenticated with `Authorization: Bearer <token>`
//! (§4.6), where the token is the sender's name unless one was configured
//! with [`HttpTarget::with_token`], exactly as for the gRPC target.
//!
//! **An independent, minimal codec.** The runner does not reuse any
//! runtime's JSON codec, so the runtime's codec is never tested against
//! itself:
//!
//! - Envelopes always go out with `payload_b64`, which RFC-MACP-0001 §10.12
//!   always permits, carrying the very bytes the gRPC target would send. The
//!   JSON `payload` form is therefore **not** exercised by an HTTP run.
//! - Responses are read with `serde_json` for the few fields the runner
//!   needs (the Ack, `SessionMetadata.state`, `RegisterPolicyResponse`,
//!   `RuntimeInfo`). Defaults may be absent or `null` (§10.10).
//! - History Envelopes arrive with `payload_b64` (decoded as is) or with a
//!   JSON `payload` (§10.12), which is re-encoded to Protobuf bytes by a
//!   small table of the payload messages the fixtures use ([`payload_bytes`]).
//!
//! **History** is the subscribe form of `StreamSession` as server-sent
//! events (§4.3), `after_sequence = 0`, read until the response ends (a
//! terminal Envelope ends it normally), until an `event: error` reporting
//! `SESSION_NOT_OPEN` (the session ended by cancel or expiry, which delivers
//! no Envelope), or until it goes idle (an open session's stream has no
//! end-of-replay marker, as on gRPC).
//!
//! What a server cannot show is reported as for the gRPC target (no clock,
//! mode state, replay or hash API; see the crate README).

use crate::target::{
    Capabilities, ConformanceTarget, HistoryEntry, Observable, Observation, Profile, Submission,
    TargetError,
};
use async_trait::async_trait;
use base64::Engine as _;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1;
use hyper::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use macp_core::PolicyDefinition;
use macp_pb::pb::Envelope;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

/// Identity used for the policy-registration RPCs.
const ADMIN_SENDER: &str = "macp-conform";
/// The fixed part of every RPC path (RFC-MACP-0006 §4.1).
const RPC_PATH: &str = "/macp/v1/";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A connection's byte stream: TCP, or TLS over TCP.
trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

type Sender = http1::SendRequest<Full<Bytes>>;

/// Where the binding is and how to reach it.
struct Endpoint {
    /// `host:port`, for the `Host` header and the TCP connect.
    authority: String,
    host: String,
    /// The path prefix of the base URL, no trailing slash (`""` for none).
    prefix: String,
    tls: Option<TlsConnector>,
}

impl Endpoint {
    fn parse(url: &str) -> Result<Self, String> {
        let uri: Uri = url
            .parse()
            .map_err(|e| format!("bad HTTP target URL {url}: {e}"))?;
        let tls = match uri.scheme_str() {
            Some("https") => true,
            Some("http") => false,
            _ => {
                return Err(format!(
                    "unsupported HTTP target URL {url} (use http:// or https://)"
                ))
            }
        };
        if uri.query().is_some() {
            return Err(format!("HTTP target URL {url} has a query"));
        }
        // `Uri::host` keeps an IPv6 literal's brackets.
        let host = uri
            .host()
            .ok_or_else(|| format!("HTTP target URL {url} has no host"))?
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_string();
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let authority = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Ok(Self {
            authority,
            host,
            prefix: uri.path().trim_end_matches('/').to_string(),
            tls: tls.then(tls_connector),
        })
    }

    /// A new HTTP/1.1 connection; its driver runs on a spawned task until
    /// the connection closes.
    async fn connect(&self) -> Result<Sender, String> {
        let open = async {
            let tcp = TcpStream::connect(&self.authority)
                .await
                .map_err(|e| format!("cannot connect to {}: {e}", self.authority))?;
            let _ = tcp.set_nodelay(true);
            let io: Box<dyn Io> = match &self.tls {
                None => Box::new(tcp),
                Some(connector) => {
                    let name = ServerName::try_from(self.host.clone())
                        .map_err(|e| format!("{}: not a TLS server name: {e}", self.host))?;
                    Box::new(
                        connector
                            .connect(name, tcp)
                            .await
                            .map_err(|e| format!("TLS handshake with {}: {e}", self.authority))?,
                    )
                }
            };
            Ok::<_, String>(io)
        };
        let io = tokio::time::timeout(CONNECT_TIMEOUT, open)
            .await
            .map_err(|_| format!("connecting to {} timed out", self.authority))??;
        let (sender, conn) = http1::handshake(TokioIo::new(io))
            .await
            .map_err(|e| format!("HTTP/1.1 handshake with {}: {e}", self.authority))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(sender)
    }
}

/// See the module docs.
pub struct HttpTarget {
    url: String,
    endpoint: Endpoint,
    /// The keep-alive connection unary RPCs reuse; a stream gets its own.
    idle: Option<Sender>,
    tokens: HashMap<String, String>,
    stream_idle: Duration,
}

impl HttpTarget {
    /// Connect to the binding's base URL (§4.1): `http://host:port` in the
    /// clear or `https://host:port` over TLS (WebPKI roots), with an
    /// optional path prefix. The URL is checked here; the first connection
    /// opens with the first RPC.
    pub async fn connect(url: &str) -> Result<Self, TargetError> {
        Ok(Self {
            url: url.to_string(),
            endpoint: Endpoint::parse(url).map_err(TargetError)?,
            idle: None,
            tokens: HashMap::new(),
            stream_idle: Duration::from_millis(250),
        })
    }

    /// Authenticate as `sender` with this bearer token instead of the
    /// sender's name.
    pub fn with_token(mut self, sender: impl Into<String>, token: impl Into<String>) -> Self {
        self.tokens.insert(sender.into(), token.into());
        self
    }

    /// How long a quiet subscribe stream is taken to have finished replaying
    /// history (default 250 ms).
    pub fn with_stream_idle(mut self, idle: Duration) -> Self {
        self.stream_idle = idle;
        self
    }

    /// The server's `Initialize` `runtime_info`: `(name, version)`.
    pub async fn runtime_info(&mut self) -> Result<(String, String), TargetError> {
        let body = json!({ "supported_protocol_versions": [macp_core::MACP_VERSION] });
        let resp = self.unary("Initialize", ADMIN_SENDER, &body).await?;
        let info = &resp["runtime_info"];
        Ok((str_field(info, "name"), str_field(info, "version")))
    }

    fn request(
        &self,
        method: &str,
        sender: &str,
        body: &Value,
        stream: bool,
    ) -> Result<Request<Full<Bytes>>, TargetError> {
        let token = self
            .tokens
            .get(sender)
            .map(String::as_str)
            .unwrap_or(sender);
        let mut builder = Request::post(format!("{}{RPC_PATH}{method}", self.endpoint.prefix))
            .header(HOST, &self.endpoint.authority)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json");
        if stream {
            builder = builder.header(ACCEPT, "text/event-stream");
        }
        builder
            .body(Full::new(Bytes::from(body.to_string())))
            .map_err(|e| {
                TargetError(format!(
                    "{method}: cannot build the request ({e}); is {sender:?} a valid bearer token?"
                ))
            })
    }

    /// A unary RPC answered `200`; any other status is a [`TargetError`]
    /// naming the request-level error (§4.5).
    async fn unary(
        &mut self,
        method: &str,
        sender: &str,
        body: &Value,
    ) -> Result<Value, TargetError> {
        match self.call(method, sender, body).await? {
            Reply::Ok(v) => Ok(v),
            Reply::Refused { status, code, body } => Err(TargetError(format!(
                "{method}: HTTP {status} {}: {body}",
                code.as_deref().unwrap_or("(no MACP error code)")
            ))),
        }
    }

    /// The idle keep-alive connection if it is still usable, else a new one.
    async fn sender(&mut self) -> Result<Sender, TargetError> {
        if let Some(mut sender) = self.idle.take() {
            if sender.ready().await.is_ok() {
                return Ok(sender);
            }
        }
        self.endpoint.connect().await.map_err(TargetError)
    }

    async fn call(
        &mut self,
        method: &str,
        sender: &str,
        body: &Value,
    ) -> Result<Reply, TargetError> {
        let req = self.request(method, sender, body, false)?;
        let mut conn = self.sender().await?;
        let exchange = async {
            let resp = conn.send_request(req).await.map_err(|e| e.to_string())?;
            let (parts, body) = resp.into_parts();
            let bytes = body
                .collect()
                .await
                .map_err(|e| format!("reading the body: {e}"))?
                .to_bytes();
            Ok::<_, String>((parts, bytes))
        };
        let (parts, bytes) = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| TargetError(format!("{method}: timed out")))?
            .map_err(|e| TargetError(format!("{method}: {e}")))?;
        self.idle = Some(conn);
        let status = parts.status.as_u16();
        if status == 200 {
            let v = serde_json::from_slice(&bytes)
                .map_err(|e| TargetError(format!("{method}: 200 body is not JSON: {e}")))?;
            return Ok(Reply::Ok(v));
        }
        // §4.5: the code is in the body's `error.code`, and SHOULD be in the
        // header; a transport status (404/405/406/415) carries neither.
        let header_code = parts
            .headers
            .get("macp-error-code")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body_code = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["code"].as_str().map(String::from));
        Ok(Reply::Refused {
            status,
            code: body_code.or(header_code),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn history(
        &mut self,
        session_id: &str,
        reader: &str,
    ) -> Result<Vec<Envelope>, TargetError> {
        let body = json!({ "subscribe_session_id": session_id, "after_sequence": 0 });
        let req = self.request("StreamSession", reader, &body, true)?;
        // Its own connection: the stream may stay open after we stop reading.
        let mut conn = self.endpoint.connect().await.map_err(TargetError)?;
        let resp = tokio::time::timeout(REQUEST_TIMEOUT, conn.send_request(req))
            .await
            .map_err(|_| TargetError("StreamSession: timed out".into()))?
            .map_err(|e| TargetError(format!("StreamSession: {e}")))?;
        let (parts, mut body) = resp.into_parts();
        if parts.status.as_u16() != 200 {
            let text = tokio::time::timeout(REQUEST_TIMEOUT, body.collect())
                .await
                .ok()
                .and_then(Result::ok)
                .map(|c| String::from_utf8_lossy(&c.to_bytes()).into_owned())
                .unwrap_or_default();
            return Err(TargetError(format!(
                "StreamSession: HTTP {}: {text}",
                parts.status.as_u16()
            )));
        }
        let mut parser = SseParser::default();
        let mut out = Vec::new();
        loop {
            let frame = match tokio::time::timeout(self.stream_idle, body.frame()).await {
                // Quiet: replay is over and the session is still live.
                Err(_) => break,
                Ok(None) => break,
                Ok(Some(Ok(frame))) => frame,
                Ok(Some(Err(e))) => return Err(TargetError(format!("StreamSession: {e}"))),
            };
            let Ok(chunk) = frame.into_data() else {
                continue;
            };
            for event in parser.feed(&chunk).map_err(TargetError)? {
                match stream_item(&event).map_err(TargetError)? {
                    StreamItem::Envelope(env) => out.push(*env),
                    StreamItem::End => return Ok(out),
                }
            }
        }
        Ok(out)
    }
}

enum Reply {
    Ok(Value),
    Refused {
        status: u16,
        code: Option<String>,
        body: String,
    },
}

/// rustls on the ring provider, trusting the WebPKI roots (the gRPC target's
/// trust store), ALPN `http/1.1`.
fn tls_connector() -> TlsConnector {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("ring supports the default protocol versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

fn str_field(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

fn base64_encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Standard or URL-safe alphabet, padded or not (RFC-MACP-0001 §10.3).
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    let lenient =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&base64::alphabet::STANDARD, lenient)
        .decode(s)
        .or_else(|_| GeneralPurpose::new(&base64::alphabet::URL_SAFE, lenient).decode(s))
        .map_err(|e| format!("not base64: {e}"))
}

/// Milliseconds since the Unix epoch as RFC 3339 UTC with milliseconds
/// (RFC-MACP-0001 §10.11 item 1).
pub(crate) fn rfc3339_millis(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// An Envelope in the canonical JSON mapping, always with `payload_b64`.
pub(crate) fn envelope_json(env: &Envelope) -> Value {
    json!({
        "macp_version": env.macp_version,
        "mode": env.mode,
        "message_type": env.message_type,
        "message_id": env.message_id,
        "session_id": env.session_id,
        "sender": env.sender,
        "timestamp": rfc3339_millis(env.timestamp_unix_ms),
        "payload_b64": base64_encode(&env.payload),
    })
}

/// An Envelope from its canonical JSON form: the string fields, and the
/// payload from `payload_b64` or, re-encoded, from a JSON `payload`. The
/// timestamp is not needed by the runner and is not read.
pub(crate) fn envelope_from_json(v: &Value) -> Result<Envelope, String> {
    let s = |key: &str| -> Result<String, String> {
        match &v[key] {
            Value::Null => Ok(String::new()),
            Value::String(s) => Ok(s.clone()),
            other => Err(format!("Envelope.{key} is not a string: {other}")),
        }
    };
    let mode = s("mode")?;
    let message_type = s("message_type")?;
    let payload = match (&v["payload_b64"], &v["payload"]) {
        (Value::String(b64), Value::Null) => base64_decode(b64)?,
        (Value::Null, Value::Null) => Vec::new(),
        (Value::Null, p @ Value::Object(_)) => payload_bytes(&mode, &message_type, p)?,
        (Value::Null, other) => return Err(format!("Envelope.payload is not an object: {other}")),
        _ => return Err("Envelope has both payload and payload_b64".into()),
    };
    Ok(Envelope {
        macp_version: s("macp_version")?,
        mode,
        message_type,
        message_id: s("message_id")?,
        session_id: s("session_id")?,
        sender: s("sender")?,
        timestamp_unix_ms: 0,
        payload,
    })
}

// ───────────────────── payload JSON → Protobuf bytes ─────────────────────

/// A field's Protobuf type, as far as the payload messages need.
#[derive(Clone, Copy)]
enum Kind {
    Str,
    Bytes,
    Bool,
    Double,
    U32,
    I64,
    RepeatedStr,
    /// A message-typed field (explicit presence): its fields.
    Message(&'static [(&'static str, u32, Kind)]),
    RepeatedMessage(&'static [(&'static str, u32, Kind)]),
    /// `map<string, bytes>`, entries in key order.
    MapStrBytes,
}

use Kind::*;

type Fields = &'static [(&'static str, u32, Kind)];

const ROOT: Fields = &[("uri", 1, Str), ("name", 2, Str)];
const SESSION_START: Fields = &[
    ("intent", 1, Str),
    ("participants", 2, RepeatedStr),
    ("mode_version", 3, Str),
    ("configuration_version", 4, Str),
    ("policy_version", 5, Str),
    ("ttl_ms", 6, I64),
    ("roots", 7, RepeatedMessage(ROOT)),
    ("context_id", 8, Str),
    ("extensions", 9, MapStrBytes),
    ("max_suspend_ms", 10, I64),
];
const COMMITMENT_REF: Fields = &[("session_id", 1, Str), ("commitment_hash", 2, Str)];
const COMMITMENT: Fields = &[
    ("commitment_id", 1, Str),
    ("action", 2, Str),
    ("authority_scope", 3, Str),
    ("reason", 4, Str),
    ("mode_version", 5, Str),
    ("policy_version", 6, Str),
    ("configuration_version", 7, Str),
    ("outcome_positive", 8, Bool),
    ("supersedes", 9, Message(COMMITMENT_REF)),
];

/// The payload message `(mode, message_type)` selects (RFC-MACP-0001
/// §10.12), for the messages the fixtures use.
fn payload_fields(mode: &str, message_type: &str) -> Option<Fields> {
    match message_type {
        "Commitment" => return Some(COMMITMENT),
        "SessionStart" => return Some(SESSION_START),
        _ => {}
    }
    Some(match (mode, message_type) {
        ("macp.mode.decision.v1", "Proposal") => &[
            ("proposal_id", 1, Str),
            ("option", 2, Str),
            ("rationale", 3, Str),
            ("supporting_data", 4, Bytes),
        ],
        ("macp.mode.decision.v1", "Evaluation") => &[
            ("proposal_id", 1, Str),
            ("recommendation", 2, Str),
            ("confidence", 3, Double),
            ("reason", 4, Str),
        ],
        ("macp.mode.decision.v1", "Objection") => &[
            ("proposal_id", 1, Str),
            ("reason", 2, Str),
            ("severity", 3, Str),
        ],
        ("macp.mode.decision.v1", "Vote") => &[
            ("proposal_id", 1, Str),
            ("vote", 2, Str),
            ("reason", 3, Str),
        ],
        ("macp.mode.proposal.v1", "Proposal") => &[
            ("proposal_id", 1, Str),
            ("title", 2, Str),
            ("summary", 3, Str),
            ("details", 4, Bytes),
            ("tags", 5, RepeatedStr),
        ],
        ("macp.mode.proposal.v1", "CounterProposal") => &[
            ("proposal_id", 1, Str),
            ("supersedes_proposal_id", 2, Str),
            ("title", 3, Str),
            ("summary", 4, Str),
            ("details", 5, Bytes),
        ],
        ("macp.mode.proposal.v1", "Accept") => &[("proposal_id", 1, Str), ("reason", 2, Str)],
        ("macp.mode.proposal.v1", "Reject") => &[
            ("proposal_id", 1, Str),
            ("terminal", 2, Bool),
            ("reason", 3, Str),
        ],
        ("macp.mode.proposal.v1", "Withdraw") => &[("proposal_id", 1, Str), ("reason", 2, Str)],
        ("macp.mode.task.v1", "TaskRequest") => &[
            ("task_id", 1, Str),
            ("title", 2, Str),
            ("instructions", 3, Str),
            ("requested_assignee", 4, Str),
            ("input", 5, Bytes),
            ("deadline_unix_ms", 6, I64),
        ],
        ("macp.mode.task.v1", "TaskAccept" | "TaskReject") => &[
            ("task_id", 1, Str),
            ("assignee", 2, Str),
            ("reason", 3, Str),
        ],
        ("macp.mode.task.v1", "TaskUpdate") => &[
            ("task_id", 1, Str),
            ("status", 2, Str),
            ("progress", 3, Double),
            ("message", 4, Str),
            ("partial_output", 5, Bytes),
        ],
        ("macp.mode.task.v1", "TaskComplete") => &[
            ("task_id", 1, Str),
            ("assignee", 2, Str),
            ("output", 3, Bytes),
            ("summary", 4, Str),
        ],
        ("macp.mode.task.v1", "TaskFail") => &[
            ("task_id", 1, Str),
            ("assignee", 2, Str),
            ("error_code", 3, Str),
            ("reason", 4, Str),
            ("retryable", 5, Bool),
        ],
        ("macp.mode.handoff.v1", "HandoffOffer") => &[
            ("handoff_id", 1, Str),
            ("target_participant", 2, Str),
            ("scope", 3, Str),
            ("reason", 4, Str),
        ],
        ("macp.mode.handoff.v1", "HandoffContext") => &[
            ("handoff_id", 1, Str),
            ("content_type", 2, Str),
            ("context", 3, Bytes),
        ],
        ("macp.mode.handoff.v1", "HandoffAccept") => &[
            ("handoff_id", 1, Str),
            ("accepted_by", 2, Str),
            ("reason", 3, Str),
            ("implicit", 4, Bool),
        ],
        ("macp.mode.handoff.v1", "HandoffDecline") => &[
            ("handoff_id", 1, Str),
            ("declined_by", 2, Str),
            ("reason", 3, Str),
        ],
        ("macp.mode.quorum.v1", "ApprovalRequest") => &[
            ("request_id", 1, Str),
            ("action", 2, Str),
            ("summary", 3, Str),
            ("details", 4, Bytes),
            ("required_approvals", 5, U32),
        ],
        ("macp.mode.quorum.v1", "Approve" | "Reject" | "Abstain") => {
            &[("request_id", 1, Str), ("reason", 2, Str)]
        }
        ("ext.multi_round.v1", "Contribute") => &[("value", 1, Str)],
        _ => return None,
    })
}

/// A JSON `payload` (RFC-MACP-0001 §10.12) as the Protobuf bytes it stands
/// for: fields in field-number order, proto3 defaults omitted, so the bytes
/// are the canonical encoding (§10.13), which is what a runtime that emits a
/// JSON `payload` only for losslessly round-tripping bytes held.
pub(crate) fn payload_bytes(mode: &str, message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    let fields = payload_fields(mode, message_type).ok_or_else(|| {
        format!("JSON payload for ({mode}, {message_type}), which this runner has no schema for")
    })?;
    encode_message(fields, p).map_err(|e| format!("{message_type} payload: {e}"))
}

fn encode_message(fields: Fields, v: &Value) -> Result<Vec<u8>, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| format!("expected an object, got {v}"))?;
    let mut buf = Vec::new();
    for &(name, tag, kind) in fields {
        let field = obj.get(name).unwrap_or(&Value::Null);
        encode_field(&mut buf, tag, kind, field).map_err(|e| format!("{name}: {e}"))?;
    }
    Ok(buf)
}

fn key(buf: &mut Vec<u8>, tag: u32, wire_type: u32) {
    prost::encoding::encode_varint(u64::from(tag << 3 | wire_type), buf);
}

fn length_delimited(buf: &mut Vec<u8>, tag: u32, bytes: &[u8]) {
    key(buf, tag, 2);
    prost::encoding::encode_varint(bytes.len() as u64, buf);
    buf.extend_from_slice(bytes);
}

/// A 64-bit integer as a JSON number or a quoted decimal (§10.9).
fn integer(v: &Value) -> Result<i128, String> {
    match v {
        Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .ok_or_else(|| format!("{n} is not an integer")),
        Value::String(s) => s
            .parse::<i128>()
            .map_err(|_| format!("{s:?} is not an integer")),
        other => Err(format!("{other} is not an integer")),
    }
}

fn encode_field(buf: &mut Vec<u8>, tag: u32, kind: Kind, v: &Value) -> Result<(), String> {
    if v.is_null() {
        return Ok(());
    }
    match kind {
        Str => {
            let s = v.as_str().ok_or_else(|| format!("{v} is not a string"))?;
            if !s.is_empty() {
                length_delimited(buf, tag, s.as_bytes());
            }
        }
        Bytes => {
            let s = v.as_str().ok_or_else(|| format!("{v} is not a string"))?;
            let bytes = base64_decode(s)?;
            if !bytes.is_empty() {
                length_delimited(buf, tag, &bytes);
            }
        }
        Bool => {
            if v.as_bool().ok_or_else(|| format!("{v} is not a bool"))? {
                key(buf, tag, 0);
                buf.push(1);
            }
        }
        Double => {
            let d = match v {
                Value::Number(n) => n.as_f64().ok_or_else(|| format!("{n} is not a number"))?,
                Value::String(s) if s == "NaN" => f64::NAN,
                Value::String(s) if s == "Infinity" => f64::INFINITY,
                Value::String(s) if s == "-Infinity" => f64::NEG_INFINITY,
                other => return Err(format!("{other} is not a number")),
            };
            // As prost: omitted when it compares equal to 0.0 (so -0.0 too).
            if d != 0.0 {
                key(buf, tag, 1);
                buf.extend_from_slice(&d.to_le_bytes());
            }
        }
        U32 => {
            let n =
                u32::try_from(integer(v)?).map_err(|_| format!("{v} is out of uint32 range"))?;
            if n != 0 {
                key(buf, tag, 0);
                prost::encoding::encode_varint(u64::from(n), buf);
            }
        }
        I64 => {
            let n = i64::try_from(integer(v)?).map_err(|_| format!("{v} is out of int64 range"))?;
            if n != 0 {
                key(buf, tag, 0);
                prost::encoding::encode_varint(n as u64, buf);
            }
        }
        RepeatedStr => {
            let items = v.as_array().ok_or_else(|| format!("{v} is not an array"))?;
            for item in items {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("{item} is not a string"))?;
                length_delimited(buf, tag, s.as_bytes());
            }
        }
        Message(fields) => {
            // Explicit presence: emitted when set, even if empty.
            let inner = encode_message(fields, v)?;
            length_delimited(buf, tag, &inner);
        }
        RepeatedMessage(fields) => {
            let items = v.as_array().ok_or_else(|| format!("{v} is not an array"))?;
            for item in items {
                let inner = encode_message(fields, item)?;
                length_delimited(buf, tag, &inner);
            }
        }
        MapStrBytes => {
            let map = v
                .as_object()
                .ok_or_else(|| format!("{v} is not an object"))?;
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for k in keys {
                // An entry is a message {1: key, 2: value}, defaults omitted.
                let mut entry = Vec::new();
                encode_field(&mut entry, 1, Str, &Value::String(k.clone()))?;
                encode_field(&mut entry, 2, Bytes, &map[k])?;
                length_delimited(buf, tag, &entry);
            }
        }
    }
    Ok(())
}

// ─────────────────────────── server-sent events ───────────────────────────

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SseEvent {
    /// The `event:` field; `None` for an ordinary message event.
    pub(crate) event: Option<String>,
    /// The `id:` field (a subscribe Envelope's ordinal, §4.3).
    pub(crate) id: Option<String>,
    pub(crate) data: String,
}

/// An incremental parser for the binding's SSE framing (§4.3; the WHATWG
/// event-stream format). Lines end in LF, CRLF or CR; a blank line
/// dispatches the event; comment lines (`:`) are ignored, as are `retry:`
/// and unknown fields. The binding puts each message on **one** `data:`
/// line, so a second `data:` line in one event is an error, not a join.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    current: SseEvent,
    has_data: bool,
    /// The last byte consumed was a CR ending a line: a following LF belongs
    /// to it.
    after_cr: bool,
}

impl SseParser {
    /// Feed bytes; returns the events they complete.
    pub(crate) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, String> {
        let mut events = Vec::new();
        for &b in bytes {
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.buf);
                    if let Some(ev) = self.line(&line)? {
                        events.push(ev);
                    }
                }
                _ => self.buf.push(b),
            }
        }
        Ok(events)
    }

    fn line(&mut self, line: &[u8]) -> Result<Option<SseEvent>, String> {
        if line.is_empty() {
            if !self.has_data {
                // No data: nothing to dispatch (per the format, the buffers
                // reset).
                self.current = SseEvent::default();
                return Ok(None);
            }
            self.has_data = false;
            return Ok(Some(std::mem::take(&mut self.current)));
        }
        if line[0] == b':' {
            return Ok(None);
        }
        let line = std::str::from_utf8(line).map_err(|_| "SSE line is not UTF-8".to_string())?;
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => {
                if self.has_data {
                    return Err(
                        "SSE event has more than one data: line (RFC-MACP-0006 §4.3 puts each \
                         message on one line)"
                            .into(),
                    );
                }
                self.has_data = true;
                self.current.data = value.to_string();
            }
            "event" => self.current.event = Some(value.to_string()),
            "id" => self.current.id = Some(value.to_string()),
            _ => {}
        }
        Ok(None)
    }
}

/// What one subscribe-stream event means to [`HttpTarget::history`].
#[derive(Debug)]
pub(crate) enum StreamItem {
    Envelope(Box<Envelope>),
    /// History is complete: the session ended by cancel or expiry
    /// (`event: error` `SESSION_NOT_OPEN`, §4.3).
    End,
}

pub(crate) fn stream_item(ev: &SseEvent) -> Result<StreamItem, String> {
    let data: Value = serde_json::from_str(&ev.data)
        .map_err(|e| format!("SSE data is not JSON ({e}): {}", ev.data))?;
    match ev.event.as_deref() {
        None | Some("message") => {}
        Some("error") => {
            let code = data["error"]["code"].as_str().unwrap_or_default();
            if code == "SESSION_NOT_OPEN" {
                return Ok(StreamItem::End);
            }
            return Err(format!(
                "StreamSession ended with event: error {code}: {data}"
            ));
        }
        Some(other) => return Err(format!("unexpected SSE event type {other:?}")),
    }
    if !data["error"].is_null() {
        // Non-fatal StreamSessionResponse.error: the gRPC target reports
        // the same inline error as a failure too.
        return Err(format!("StreamSession error event: {}", data["error"]));
    }
    let env = &data["envelope"];
    if !env.is_object() {
        return Err(format!("SSE event carries no envelope: {data}"));
    }
    if ev.id.is_none() {
        return Err("subscribe Envelope event has no id: field (RFC-MACP-0006 §4.3)".into());
    }
    envelope_from_json(env).map(|e| StreamItem::Envelope(Box::new(e)))
}

fn state_name(state: &Value) -> String {
    let name = match state {
        Value::String(s) => s.as_str(),
        // Decoders accept an enum's number too (proto3 JSON).
        Value::Number(n) => match n.as_i64() {
            Some(1) => "SESSION_STATE_OPEN",
            Some(2) => "SESSION_STATE_RESOLVED",
            Some(3) => "SESSION_STATE_EXPIRED",
            Some(4) => "SESSION_STATE_SUSPENDED",
            Some(5) => "SESSION_STATE_CANCELLED",
            _ => "",
        },
        _ => "",
    };
    match name {
        "SESSION_STATE_OPEN" => "Open".into(),
        "SESSION_STATE_RESOLVED" => "Resolved".into(),
        "SESSION_STATE_EXPIRED" => "Expired".into(),
        "SESSION_STATE_SUSPENDED" => "Suspended".into(),
        "SESSION_STATE_CANCELLED" => "Cancelled".into(),
        _ => format!("unknown({state})"),
    }
}

#[async_trait]
impl ConformanceTarget for HttpTarget {
    fn profile(&self) -> Profile {
        Profile::Server
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn describe(&self) -> String {
        format!("{} (HTTP binding)", self.url)
    }

    async fn begin_fixture(&mut self, _clock_start: Option<i64>) -> Result<(), TargetError> {
        Ok(())
    }

    async fn register_policy(&mut self, def: PolicyDefinition) -> Result<(), TargetError> {
        // `rules` inline as a JSON object (RFC-MACP-0001 §10.11 item 3).
        let body = json!({
            "policy_descriptor": {
                "policy_id": def.policy_id,
                "mode": def.mode,
                "description": def.description,
                "rules": def.rules,
                "schema_version": def.schema_version,
                "registered_at_unix_ms": 0,
            }
        });
        let first = self.unary("RegisterPolicy", ADMIN_SENDER, &body).await?;
        if first["ok"].as_bool() == Some(true) {
            return Ok(());
        }
        // Most likely already registered by an earlier run: replace it.
        let _ = self
            .call(
                "UnregisterPolicy",
                ADMIN_SENDER,
                &json!({ "policy_id": def.policy_id }),
            )
            .await;
        let second = self.unary("RegisterPolicy", ADMIN_SENDER, &body).await?;
        if second["ok"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(TargetError(format!(
                "RegisterPolicy {} refused: {} (first attempt: {})",
                def.policy_id,
                str_field(&second, "error"),
                str_field(&first, "error")
            )))
        }
    }

    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError> {
        let body = json!({ "envelope": envelope_json(&envelope) });
        match self.call("Send", &envelope.sender, &body).await? {
            // §4.4: every decision about the Envelope is the 200 Ack.
            Reply::Ok(resp) => {
                let ack = &resp["ack"];
                if !ack.is_object() {
                    return Err(TargetError("SendResponse has no ack".into()));
                }
                if ack["ok"].as_bool().unwrap_or(false) {
                    Ok(Submission::Accepted {
                        duplicate: ack["duplicate"].as_bool().unwrap_or(false),
                    })
                } else {
                    Ok(Submission::Rejected {
                        code: str_field(&ack["error"], "code"),
                    })
                }
            }
            // RFC-MACP-0006 §4.4: only the three request-level layers (401
            // unauthenticated, 413 body cap, 400 not a SendRequest) may answer
            // `Send` with a non-200; they are protocol rejections, as a coded
            // gRPC status is for the gRPC target. Every decision about the
            // Envelope is a 200 Ack, so any other coded status (a 403, 404,
            // 429, ...) is a mapping bug, never a rejection.
            Reply::Refused {
                status: 400 | 401 | 413,
                code: Some(code),
                ..
            } => Ok(Submission::Rejected { code }),
            Reply::Refused { status, body, .. } => {
                Err(TargetError(format!("Send: HTTP {status}: {body}")))
            }
        }
    }

    async fn observe(
        &mut self,
        session_id: &str,
        initiator: &str,
    ) -> Result<Observation, TargetError> {
        let resp = self
            .unary(
                "GetSession",
                initiator,
                &json!({ "session_id": session_id }),
            )
            .await?;
        let meta = &resp["metadata"];
        if !meta.is_object() {
            return Err(TargetError("GetSessionResponse has no metadata".into()));
        }
        let state = state_name(&meta["state"]);
        let history = self.history(session_id, initiator).await?;
        let resolution = if state == "Resolved" {
            match history
                .iter()
                .rev()
                .find(|e| e.message_type == "Commitment")
            {
                Some(c) => Observable::Observed(Some(c.payload.clone())),
                None => Observable::Unobservable,
            }
        } else {
            Observable::Observed(None)
        };
        let mut obs = Observation::new(state).with_history(
            history
                .iter()
                .filter(|e| e.message_type != "SessionStart")
                .map(HistoryEntry::from_envelope)
                .collect(),
        );
        obs.resolution = resolution;
        Ok(obs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_pb::pb::{CommitmentPayload, CommitmentRef, Root, SessionStartPayload};
    use macp_pb::{handoff_pb, proposal_pb, quorum_pb, task_pb};
    use prost::Message;

    fn parse(input: &[u8]) -> Result<Vec<SseEvent>, String> {
        SseParser::default().feed(input)
    }

    #[test]
    fn sse_parses_events_ids_and_ignores_comments() {
        let events = parse(
            b": keepalive\n\nid: 1\ndata: {\"a\":1}\n\n: hi\nretry: 5\nfoo: bar\ndata:{\"b\":2}\n\n",
        )
        .unwrap();
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: None,
                    id: Some("1".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    id: None,
                    data: "{\"b\":2}".into()
                },
            ]
        );
    }

    #[test]
    fn sse_accepts_crlf_and_cr_and_split_chunks() {
        let whole = b"id: 7\r\ndata: x\r\n\r\nevent: error\rdata: y\r\r";
        let mut p = SseParser::default();
        let mut got = Vec::new();
        for b in whole.iter() {
            got.extend(p.feed(std::slice::from_ref(b)).unwrap());
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id.as_deref(), Some("7"));
        assert_eq!(got[0].data, "x");
        assert_eq!(got[1].event.as_deref(), Some("error"));
        assert_eq!(got[1].data, "y");
        // An event is not dispatched before its blank line.
        assert!(SseParser::default().feed(b"data: x\n").unwrap().is_empty());
    }

    #[test]
    fn sse_rejects_multi_line_data() {
        let err = parse(b"data: {\"a\":\ndata: 1}\n\n").unwrap_err();
        assert!(err.contains("more than one data"), "{err}");
    }

    #[test]
    fn sse_event_without_data_is_not_dispatched() {
        assert!(parse(b"event: error\n\n: c\n\n").unwrap().is_empty());
    }

    #[test]
    fn stream_items() {
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "Vote".into(),
            message_id: "m1".into(),
            session_id: "s".into(),
            sender: "agent://a".into(),
            timestamp_unix_ms: 0,
            payload: vec![10, 1, 120],
        };
        let ev = SseEvent {
            event: None,
            id: Some("2".into()),
            data: json!({ "envelope": envelope_json(&env) }).to_string(),
        };
        match stream_item(&ev).unwrap() {
            StreamItem::Envelope(got) => assert_eq!(*got, env),
            StreamItem::End => panic!("not an end"),
        }
        // An Envelope event must carry its ordinal.
        let no_id = SseEvent { id: None, ..ev };
        assert!(stream_item(&no_id).is_err());

        let closed = SseEvent {
            event: Some("error".into()),
            id: None,
            data: r#"{"error":{"code":"SESSION_NOT_OPEN","message":"cancelled"}}"#.into(),
        };
        assert!(matches!(stream_item(&closed).unwrap(), StreamItem::End));
        let lagged = SseEvent {
            data: r#"{"error":{"code":"STREAM_LAGGED"}}"#.into(),
            ..closed.clone()
        };
        assert!(stream_item(&lagged).unwrap_err().contains("STREAM_LAGGED"));
        // The non-fatal inline error is an ordinary event.
        let inline = SseEvent {
            event: None,
            id: None,
            data: r#"{"error":{"code":"INVALID_ENVELOPE"}}"#.into(),
        };
        assert!(stream_item(&inline).is_err());
        let garbage = SseEvent {
            data: "not json".into(),
            ..inline
        };
        assert!(stream_item(&garbage).is_err());
    }

    #[test]
    fn rfc3339() {
        assert_eq!(rfc3339_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_millis(1_700_000_000_123),
            "2023-11-14T22:13:20.123Z"
        );
        assert_eq!(rfc3339_millis(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339_millis(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn envelope_round_trips_through_payload_b64() {
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: String::new(),
            message_type: "Signal".into(),
            message_id: "m".into(),
            session_id: String::new(),
            sender: "agent://a".into(),
            timestamp_unix_ms: 0,
            payload: (0u8..=255).collect(),
        };
        let v = envelope_json(&env);
        assert!(v.get("payload").is_none());
        assert_eq!(envelope_from_json(&v).unwrap(), env);
    }

    #[test]
    fn json_payloads_re_encode_to_the_prost_bytes() {
        let c = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "a".into(),
            authority_scope: "s".into(),
            reason: "r".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg".into(),
            outcome_positive: true,
            supersedes: Some(CommitmentRef::default()),
        };
        let j = json!({"commitment_id":"c1","action":"a","authority_scope":"s","reason":"r",
            "mode_version":"1.0.0","policy_version":"policy.default","configuration_version":"cfg",
            "outcome_positive":true,"supersedes":{"session_id":"","commitment_hash":""}});
        assert_eq!(
            payload_bytes("macp.mode.handoff.v1", "Commitment", &j).unwrap(),
            c.encode_to_vec()
        );
        let unset = json!({"commitment_id":"c1","outcome_positive":false,"supersedes":null});
        let c2 = CommitmentPayload {
            commitment_id: "c1".into(),
            ..Default::default()
        };
        assert_eq!(
            payload_bytes("m", "Commitment", &unset).unwrap(),
            c2.encode_to_vec()
        );

        let start = SessionStartPayload {
            intent: "i".into(),
            participants: vec!["agent://a".into(), "agent://b".into()],
            mode_version: "1.0.0".into(),
            ttl_ms: 60_000,
            roots: vec![Root {
                uri: "file:///r".into(),
                name: String::new(),
            }],
            extensions: [("k".to_string(), b"v".to_vec())].into_iter().collect(),
            ..Default::default()
        };
        let j = json!({"intent":"i","participants":["agent://a","agent://b"],
            "mode_version":"1.0.0","configuration_version":"","policy_version":"",
            "ttl_ms":60000,"roots":[{"uri":"file:///r","name":""}],"context_id":"",
            "extensions":{"k": base64_encode(b"v")},"max_suspend_ms":0});
        assert_eq!(
            payload_bytes("macp.mode.decision.v1", "SessionStart", &j).unwrap(),
            start.encode_to_vec()
        );

        let h = handoff_pb::HandoffContextPayload {
            handoff_id: "h".into(),
            content_type: "text/plain".into(),
            context: b"ctx\xff".to_vec(),
        };
        let j = json!({"handoff_id":"h","content_type":"text/plain",
            "context": base64_encode(b"ctx\xff")});
        assert_eq!(
            payload_bytes("macp.mode.handoff.v1", "HandoffContext", &j).unwrap(),
            h.encode_to_vec()
        );

        let t = task_pb::TaskRequestPayload {
            task_id: "t".into(),
            deadline_unix_ms: 1_700_000_000_000,
            ..Default::default()
        };
        for deadline in [json!(1_700_000_000_000i64), json!("1700000000000")] {
            let j = json!({"task_id":"t","title":"","input":"","deadline_unix_ms": deadline});
            assert_eq!(
                payload_bytes("macp.mode.task.v1", "TaskRequest", &j).unwrap(),
                t.encode_to_vec()
            );
        }
        let u = task_pb::TaskUpdatePayload {
            task_id: "t".into(),
            progress: 0.5,
            ..Default::default()
        };
        assert_eq!(
            payload_bytes(
                "macp.mode.task.v1",
                "TaskUpdate",
                &json!({"task_id":"t","progress":0.5,"partial_output":""})
            )
            .unwrap(),
            u.encode_to_vec()
        );

        let q = quorum_pb::ApprovalRequestPayload {
            request_id: "q".into(),
            required_approvals: 3,
            ..Default::default()
        };
        assert_eq!(
            payload_bytes(
                "macp.mode.quorum.v1",
                "ApprovalRequest",
                &json!({"request_id":"q","required_approvals":3})
            )
            .unwrap(),
            q.encode_to_vec()
        );

        let p = proposal_pb::ProposalPayload {
            proposal_id: "p".into(),
            tags: vec!["x".into(), "".into()],
            ..Default::default()
        };
        assert_eq!(
            payload_bytes(
                "macp.mode.proposal.v1",
                "Proposal",
                &json!({"proposal_id":"p","tags":["x",""]})
            )
            .unwrap(),
            p.encode_to_vec()
        );
    }

    #[test]
    fn json_payload_errors() {
        assert!(payload_bytes("ext.unknown.v1", "X", &json!({})).is_err());
        assert!(payload_bytes("macp.mode.quorum.v1", "Approve", &json!({"reason": 1})).is_err());
        assert!(payload_bytes(
            "macp.mode.quorum.v1",
            "ApprovalRequest",
            &json!({"required_approvals": -1})
        )
        .is_err());
        let both = json!({"payload_b64": "", "payload": {}});
        assert!(envelope_from_json(&both).is_err());
    }

    #[test]
    fn state_names() {
        assert_eq!(state_name(&json!("SESSION_STATE_RESOLVED")), "Resolved");
        assert_eq!(state_name(&json!(5)), "Cancelled");
        assert!(state_name(&json!("NOPE")).starts_with("unknown"));
    }

    #[test]
    fn endpoint_parsing() {
        let e = Endpoint::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            (e.authority.as_str(), e.prefix.as_str()),
            ("127.0.0.1:8080", "")
        );
        assert!(e.tls.is_none());
        let e = Endpoint::parse("https://macp.example.com/api/").unwrap();
        assert_eq!(e.authority, "macp.example.com:443");
        assert_eq!(e.host, "macp.example.com");
        assert_eq!(e.prefix, "/api");
        assert!(e.tls.is_some());
        let e = Endpoint::parse("http://[::1]:9/").unwrap();
        assert_eq!((e.authority.as_str(), e.host.as_str()), ("[::1]:9", "::1"));
        assert_eq!(Endpoint::parse("http://h").unwrap().authority, "h:80");
        assert!(Endpoint::parse("http://h/x?q=1").is_err());
        assert!(Endpoint::parse("grpcs://h:1").is_err());
    }

    #[tokio::test]
    async fn connect_refuses_other_schemes() {
        assert!(HttpTarget::connect("grpc://127.0.0.1:1").await.is_err());
        assert!(HttpTarget::connect("http://127.0.0.1:1/api/").await.is_ok());
        assert!(HttpTarget::connect("https://example.invalid").await.is_ok());
    }
}
