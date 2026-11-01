//! The RPC table and the unary RPCs of the HTTP binding (RFC-MACP-0006 §4.1,
//! §4.2, §4.4, §4.5).
//!
//! Every row names one `macp.v1.MACPRuntimeService` method. A unary row
//! calls the same `MacpRuntimeService` trait method gRPC serves, with a
//! `tonic::Request` built from the request headers ([`super::metadata`]) and
//! the request message decoded from JSON by the canonical codec, so both
//! bindings run one implementation. The table is hand-written (each row needs
//! its typed trait method) and pinned by tests against the service
//! descriptor and against the telemetry method list, so an RPC added to the
//! proto fails to compile (the trait method is missing) and then fails the
//! pin until it has a row here.
//!
//! **Order of a unary request** (the precedence 401 > 413 > 400 > in-band):
//!
//! 1. **Authentication from the headers, before any byte of the body is
//!    read** (§4.4), for every RPC that authenticates on gRPC (`authenticated`
//!    in the table); a refusal is `401` + `WWW-Authenticate: Bearer`. The
//!    handler authenticates again from the same metadata, so the identity it
//!    acts on is the one gRPC would derive. The pre-check charges no rate
//!    bucket.
//! 2. The body, collected under the body cap (`413 PAYLOAD_TOO_LARGE`).
//! 3. The canonical JSON decode (`400 INVALID_REQUEST` for a body that is not
//!    the request message).
//! 4. The handler. A refused RPC is its gRPC status mapped through the
//!    registry ([`super::errors`]); `Send` instead answers `200` with the Ack
//!    whatever it decides (§4.4), including the per-sender rate limits,
//!    which depend on the decoded Envelope's `message_type` and so are not
//!    request-level checks "made before the Envelope is decoded".
//!
//! Streaming rows (`StreamSession`, the `Watch*` RPCs) take the same steps
//! as server-sent events (§4.3, [`super::sse`]), with one check before all
//! of them: an `Accept` header that does not admit `text/event-stream` is the
//! transport `406`, and no stream is opened.

use bytes::Bytes;
use http::header::{HeaderMap, ACCEPT, CONTENT_LENGTH};
use http::{Response, StatusCode};
use http_body_util::{BodyExt, Limited};
use hyper::body::Body as HttpBody;
use macp_pb::pb::{SendRequest, SendResponse};
use prost_reflect::{DynamicMessage, MethodDescriptor, ReflectMessage, Value};
use tonic::metadata::MetadataMap;
use tonic::{Extensions, Status};

use super::errors::{empty, json_response, status_response, RequestError};
use super::json::{self, JsonError};
use super::metadata::metadata_from_headers;
use super::server::Body;
use super::sse;
use crate::pb::macp_runtime_service_server::MacpRuntimeService;
use crate::server::MacpServer;

/// How a row is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Unary; `authenticated` when the gRPC handler authenticates the caller
    /// (every RPC except the open discovery ones).
    Unary { authenticated: bool },
    /// Server-streaming (`Watch*`), served as SSE (§4.3); `authenticated`
    /// as for `Unary` (the gRPC handlers of `WatchModeRegistry` and
    /// `WatchRoots` read no credential, like `ListModes`/`ListRoots`).
    ServerStream { authenticated: bool },
    /// Bidirectional on gRPC (`StreamSession`); its passive-subscribe form is
    /// SSE on HTTP (§4.3). Always authenticated.
    Bidi,
}

/// One RPC of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) name: &'static str,
    pub(crate) kind: Kind,
}

macro_rules! dispatch_table {
    (
        unary { $( $uname:literal => $method:ident ( $req:ty ), authenticated: $auth:literal; )* }
        streaming { $( $sname:literal => $skind:expr; )* }
    ) => {
        /// Every RPC of `macp.v1.MACPRuntimeService`.
        pub(crate) const ROWS: &[Row] = &[
            $( Row { name: $uname, kind: Kind::Unary { authenticated: $auth } }, )*
            $( Row { name: $sname, kind: $skind }, )*
        ];

        /// Call the unary RPC `method` with `request` (a message of its input
        /// type); `None` if it names no unary row.
        async fn call_unary(
            server: &MacpServer,
            method: &MethodDescriptor,
            metadata: MetadataMap,
            request: DynamicMessage,
        ) -> Option<Result<DynamicMessage, Status>> {
            match method.name() {
                $(
                    $uname => {
                        let request: $req = match request.transcode_to() {
                            Ok(r) => r,
                            Err(e) => return Some(Err(Status::invalid_argument(format!(
                                "request does not transcode: {e}"
                            )))),
                        };
                        let request =
                            tonic::Request::from_parts(metadata, Extensions::new(), request);
                        let result = MacpRuntimeService::$method(server, request).await;
                        Some(result.and_then(|resp| to_dynamic(method, resp.get_ref())))
                    }
                )*
                _ => None,
            }
        }
    };
}

dispatch_table! {
    unary {
        "Initialize" => initialize(crate::pb::InitializeRequest), authenticated: false;
        "Send" => send(crate::pb::SendRequest), authenticated: true;
        "GetSession" => get_session(crate::pb::GetSessionRequest), authenticated: true;
        "CancelSession" => cancel_session(crate::pb::CancelSessionRequest), authenticated: true;
        "SuspendSession" => suspend_session(crate::pb::SuspendSessionRequest), authenticated: true;
        "ResumeSession" => resume_session(crate::pb::ResumeSessionRequest), authenticated: true;
        "GetManifest" => get_manifest(crate::pb::GetManifestRequest), authenticated: false;
        "ListModes" => list_modes(crate::pb::ListModesRequest), authenticated: false;
        "ListRoots" => list_roots(crate::pb::ListRootsRequest), authenticated: false;
        "ListExtModes" => list_ext_modes(crate::pb::ListExtModesRequest), authenticated: false;
        "RegisterExtMode" => register_ext_mode(crate::pb::RegisterExtModeRequest), authenticated: true;
        "UnregisterExtMode" => unregister_ext_mode(crate::pb::UnregisterExtModeRequest), authenticated: true;
        "PromoteMode" => promote_mode(crate::pb::PromoteModeRequest), authenticated: true;
        "ListSessions" => list_sessions(crate::pb::ListSessionsRequest), authenticated: true;
        "RegisterPolicy" => register_policy(crate::pb::RegisterPolicyRequest), authenticated: true;
        "UnregisterPolicy" => unregister_policy(crate::pb::UnregisterPolicyRequest), authenticated: true;
        "GetPolicy" => get_policy(crate::pb::GetPolicyRequest), authenticated: true;
        "ListPolicies" => list_policies(crate::pb::ListPoliciesRequest), authenticated: true;
        "WhatIfReplay" => what_if_replay(crate::pb::WhatIfReplayRequest), authenticated: true;
        "GetDecisionReceipt" => get_decision_receipt(crate::pb::GetDecisionReceiptRequest), authenticated: true;
        "GetReceiptKeys" => get_receipt_keys(crate::pb::GetReceiptKeysRequest), authenticated: false;
    }
    streaming {
        "StreamSession" => Kind::Bidi;
        "WatchModeRegistry" => Kind::ServerStream { authenticated: false };
        "WatchRoots" => Kind::ServerStream { authenticated: false };
        "WatchSignals" => Kind::ServerStream { authenticated: true };
        "WatchSessions" => Kind::ServerStream { authenticated: true };
        "WatchPolicies" => Kind::ServerStream { authenticated: true };
    }
}

/// The row of `name`.
pub(crate) fn row(name: &str) -> Option<&'static Row> {
    ROWS.iter().find(|r| r.name == name)
}

/// A generated response as a message of the method's output type.
pub(crate) fn to_dynamic<T: prost::Message>(
    method: &MethodDescriptor,
    resp: &T,
) -> Result<DynamicMessage, Status> {
    let mut message = DynamicMessage::new(method.output());
    message
        .transcode_from(resp)
        .map_err(|e| Status::internal(format!("response does not transcode: {e}")))?;
    Ok(message)
}

/// Serve one RPC whose route, method and media type are already checked.
/// The caller bounds it with the request timeout, which therefore covers a
/// stream's pre-stream checks but not its body (§4.3: the events flow after
/// the response head).
pub(crate) async fn serve<B>(
    server: &MacpServer,
    max_body_bytes: usize,
    method: MethodDescriptor,
    req: http::Request<B>,
    sse: sse::Context,
) -> Response<Body>
where
    B: HttpBody<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let Some(row) = row(method.name()) else {
        // The table is pinned to the descriptor, so the router never routes
        // here a method without a row.
        return empty(StatusCode::NOT_FOUND);
    };
    let (authenticated, streaming) = match row.kind {
        Kind::Unary { authenticated } => (authenticated, false),
        Kind::ServerStream { authenticated } => (authenticated, true),
        Kind::Bidi => (true, true),
    };
    let (parts, body) = req.into_parts();
    // (0) Streams only: `Accept` must admit `text/event-stream` (§4.3).
    if streaming && !sse::accepts_event_stream(parts.headers.get_all(ACCEPT).iter()) {
        return empty(StatusCode::NOT_ACCEPTABLE);
    }
    let metadata = metadata_from_headers(&parts.headers);

    // (1) Authentication from the headers, before the body is read.
    let identity = if authenticated {
        match server.security().authenticate_metadata(&metadata).await {
            Ok(identity) => Some(identity),
            Err(e) => return RequestError::unauthenticated(&e).into_response(),
        }
    } else {
        None
    };

    // (2) The body, under the cap.
    let bytes = match read_body(&parts.headers, body, max_body_bytes).await {
        Ok(bytes) => bytes,
        Err(e) => return e.into_response(),
    };

    if streaming {
        return sse::serve(
            server,
            &method,
            &parts.headers,
            metadata,
            identity,
            &bytes,
            sse,
        )
        .await;
    }

    if method.name() == "Send" {
        let Some(identity) = identity else {
            // `Send` is an authenticated row (pinned by a test).
            return RequestError::new("UNAUTHENTICATED", "missing bearer token").into_response();
        };
        return send(server, &identity, &bytes).await;
    }

    // (3) The request message.
    let request = match json::decode_request(method.name(), &bytes) {
        Ok(request) => request,
        Err(e) => return RequestError::new("INVALID_REQUEST", e.reason()).into_response(),
    };
    let session_id = request_session_id(&request);

    // (4) The handler.
    match call_unary(server, &method, metadata, request).await {
        Some(Ok(response)) => json_response(StatusCode::OK, json::encode_response(&response)),
        Some(Err(status)) => status_response(&status, &session_id),
        None => empty(StatusCode::NOT_FOUND),
    }
}

/// `Send` after authentication (RFC-MACP-0006 §4.4): a body that is not a
/// `SendRequest` is `400`; every decision about the Envelope, including one
/// the JSON codec makes about its payload or timestamp, is the `200` Ack.
async fn send(
    server: &MacpServer,
    identity: &crate::security::AuthIdentity,
    body: &[u8],
) -> Response<Body> {
    let ack = match json::decode_request("Send", body) {
        Err(JsonError::InvalidEnvelope {
            reason,
            session_id,
            message_id,
        }) => MacpServer::invalid_envelope_ack(&session_id, &message_id, &reason),
        Err(e) => {
            return RequestError::new(
                "INVALID_REQUEST",
                format!("request body is not a SendRequest: {}", e.reason()),
            )
            .into_response()
        }
        Ok(message) => {
            let envelope = match message.transcode_to::<SendRequest>() {
                Ok(SendRequest {
                    envelope: Some(envelope),
                }) => envelope,
                Ok(SendRequest { envelope: None }) => {
                    return RequestError::new(
                        "INVALID_REQUEST",
                        "request body is not a SendRequest: no envelope member",
                    )
                    .into_response()
                }
                Err(e) => {
                    return RequestError::new(
                        "INVALID_REQUEST",
                        format!("request body is not a SendRequest: {e}"),
                    )
                    .into_response()
                }
            };
            server.send_from_http(identity, envelope).await
        }
    };
    let response = SendResponse { ack: Some(ack) };
    let Some(method) = json::method_descriptor("Send") else {
        return empty(StatusCode::NOT_FOUND);
    };
    match to_dynamic(&method, &response) {
        Ok(message) => json_response(StatusCode::OK, json::encode_response(&message)),
        Err(status) => status_response(&status, ""),
    }
}

/// The request body, collected under `max` bytes: a declared or actual
/// length over the cap is `413 PAYLOAD_TOO_LARGE` (§4.9), before decoding.
async fn read_body<B>(headers: &HeaderMap, body: B, max: usize) -> Result<Bytes, RequestError>
where
    B: HttpBody<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let too_large = || {
        RequestError::new(
            "PAYLOAD_TOO_LARGE",
            format!("request body exceeds the {max}-byte body cap"),
        )
    };
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > max as u64) {
        return Err(too_large());
    }
    match Limited::new(body, max).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => Err(too_large()),
        Err(_) => Err(RequestError::new(
            "INVALID_REQUEST",
            "request body could not be read",
        )),
    }
}

/// The session a request names (`session_id`, or `subscribe_session_id`),
/// for the `session_id` of a request-level error; `""` when none.
fn request_session_id(request: &DynamicMessage) -> String {
    let desc = request.descriptor();
    ["session_id", "subscribe_session_id"]
        .iter()
        .filter_map(|name| desc.get_field_by_name(name))
        .find_map(|field| match request.get_field(&field).as_ref() {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Unit tests only: the Ack of a `200` `Send` response with `ok` inverted
/// (and its `error` dropped), the broken mapping behind
/// `HttpListener::with_broken_ack_mapping`.
#[cfg(test)]
pub(crate) async fn flip_ack(resp: Response<Body>) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let bytes = body
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();
    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) if parts.status == StatusCode::OK => v,
        _ => return Response::from_parts(parts, http_body_util::Full::new(bytes).boxed_unsync()),
    };
    let ok = value["ack"]["ok"].as_bool().unwrap_or(false);
    value["ack"]["ok"] = serde_json::Value::Bool(!ok);
    if let Some(ack) = value["ack"].as_object_mut() {
        ack.remove("error");
    }
    json_response(StatusCode::OK, value.to_string().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn service() -> prost_reflect::ServiceDescriptor {
        json::descriptor_pool()
            .get_service_by_name(json::SERVICE_NAME)
            .unwrap()
    }

    #[test]
    fn table_matches_the_service_descriptor() {
        let table: BTreeSet<&str> = ROWS.iter().map(|r| r.name).collect();
        assert_eq!(table.len(), ROWS.len(), "duplicate row");
        let descriptor: BTreeSet<String> =
            service().methods().map(|m| m.name().to_string()).collect();
        assert_eq!(
            table,
            descriptor
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
        );
        for m in service().methods() {
            let row = row(m.name()).unwrap();
            let (client, server) = (m.is_client_streaming(), m.is_server_streaming());
            match row.kind {
                Kind::Unary { .. } => assert!(!client && !server, "{}", m.name()),
                Kind::ServerStream { .. } => assert!(!client && server, "{}", m.name()),
                Kind::Bidi => assert!(client && server, "{}", m.name()),
            }
        }
        assert_eq!(
            row("Send").unwrap().kind,
            Kind::Unary {
                authenticated: true
            },
            "Send authenticates from headers before its body is read"
        );
    }

    /// The table, the descriptor and the gRPC SERVER span's method list
    /// (`telemetry::grpc::METHODS`, `otel` builds) name the same 27 RPCs.
    #[cfg(feature = "otel")]
    #[test]
    fn table_matches_the_telemetry_method_list() {
        let table: BTreeSet<&str> = ROWS.iter().map(|r| r.name).collect();
        let telemetry: BTreeSet<&str> = crate::telemetry::grpc::METHODS.iter().copied().collect();
        assert_eq!(table, telemetry);
    }

    #[test]
    fn request_session_id_reads_either_field() {
        let get = json::decode_request("GetSession", br#"{"session_id":"s1"}"#).unwrap();
        assert_eq!(request_session_id(&get), "s1");
        let sub =
            json::decode_request("StreamSession", br#"{"subscribe_session_id":"s2"}"#).unwrap();
        assert_eq!(request_session_id(&sub), "s2");
        let none = json::decode_request("ListModes", b"{}").unwrap();
        assert_eq!(request_session_id(&none), "");
    }
}
