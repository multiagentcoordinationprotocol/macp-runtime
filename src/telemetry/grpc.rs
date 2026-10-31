//! The gRPC SERVER span (`otel` feature only).
//!
//! Every `macp.v1.MACPRuntimeService` RPC gets one span of kind `Server`,
//! named `macp.v1.MACPRuntimeService/{Method}`, whose parent is the context
//! the client propagated in its gRPC metadata (W3C `traceparent` and
//! `tracestate`, RFC-MACP-0006 §2.1), extracted through the global text-map
//! propagator that [`super::init::init`] installs. A request without a valid
//! `traceparent` gets a new root span; a malformed one is never an error.
//!
//! The span is built on tower-http's [`TraceLayer`]. tower-http keeps it
//! entered while the handler future is polled **and** while the response body
//! is polled, and holds it until the body ends, so a streaming RPC's span
//! lasts until its stream does and code that runs while the body is polled
//! (the `StreamSession` stream, for instance) sees it as `Span::current()`.
//! Long-lived `Watch*` streams therefore keep their span open until the client
//! goes away.
//!
//! Only `macp.v1.MACPRuntimeService` paths get a span: `grpc.health.v1`,
//! reflection and anything else get [`Span::none`]. tower-http's own log
//! events are all switched off, so the layer adds no log lines.
//!
//! Attributes (OpenTelemetry RPC semantic conventions, gRPC):
//! `rpc.system = "grpc"`, `rpc.service`, `rpc.method` and, once known,
//! `rpc.grpc.status_code`. A method the service does not define is recorded
//! as `_OTHER`, in both the span name and `rpc.method`, so a client cannot
//! grow span-name cardinality. No peer address is recorded.
//!
//! Status: ERROR only for the OpenTelemetry gRPC SERVER-side error set
//! ([`is_server_error`]), with the status code's name as the description and
//! never the `grpc-message`. Every other code, `NOT_FOUND` and
//! `INVALID_ARGUMENT` included, leaves the status UNSET: those are protocol
//! outcomes, not failures of the runtime.
//!
//! Known limitation: tonic applies `Server::timeout` (and the client's
//! `grpc-timeout`) outside any `Server::layer`, and only to the response
//! future (`tonic-0.14.6/src/transport/service/grpc_timeout.rs:80-92`). A
//! request that times out before its response headers go out drops this
//! layer's future, so its span ends without `rpc.grpc.status_code`; tonic
//! answers `CANCELLED` (1) (`status.rs:644-645`), which is outside the error
//! set, so only the code attribute is lost, never an ERROR status. A stream
//! whose headers have gone out is not subject to the timeout. Not fixed:
//! recording it would need a hand-written future wrapper, or moving the
//! timeout inside the layer, which changes timeout semantics.

use std::time::Duration;

use http::{HeaderMap, Request, Response};
use opentelemetry::propagation::Extractor;
use tower::layer::util::Identity;
use tower::util::{Either, MapResponse};
use tower::Layer;
use tower_http::classify::{GrpcEosErrorsAsFailures, GrpcFailureClass};
use tower_http::trace::{
    GrpcMakeClassifier, MakeSpan, OnEos, OnFailure, OnResponse, ResponseBody, Trace, TraceLayer,
};
use tracing::field::Empty;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::{Telemetry, TARGET};
use crate::pb::macp_runtime_service_server::SERVICE_NAME;

/// The path prefix of every RPC that gets a SERVER span.
const PATH_PREFIX: &str = "/macp.v1.MACPRuntimeService/";

/// The out-of-set token for a method the service does not define.
pub const OTHER_METHOD: &str = "_OTHER";

/// Every method of `macp.v1.MACPRuntimeService`, so span names stay bounded.
/// Pinned to the generated service by a unit test.
pub(crate) const METHODS: [&str; 27] = [
    "CancelSession",
    "GetDecisionReceipt",
    "GetManifest",
    "GetPolicy",
    "GetReceiptKeys",
    "GetSession",
    "Initialize",
    "ListExtModes",
    "ListModes",
    "ListPolicies",
    "ListRoots",
    "ListSessions",
    "PromoteMode",
    "RegisterExtMode",
    "RegisterPolicy",
    "ResumeSession",
    "Send",
    "StreamSession",
    "SuspendSession",
    "UnregisterExtMode",
    "UnregisterPolicy",
    "WatchModeRegistry",
    "WatchPolicies",
    "WatchRoots",
    "WatchSessions",
    "WatchSignals",
    "WhatIfReplay",
];

/// Whether gRPC status `code` makes a SERVER span's status ERROR: the
/// OpenTelemetry gRPC semantic conventions' server-side error set, `UNKNOWN`,
/// `DEADLINE_EXCEEDED`, `UNIMPLEMENTED`, `INTERNAL`, `UNAVAILABLE` and
/// `DATA_LOSS`. Every other code, known or not, leaves the status UNSET.
pub fn is_server_error(code: i32) -> bool {
    matches!(code, 2 | 4 | 12 | 13 | 14 | 15)
}

/// The canonical name of gRPC status `code` (`NOT_FOUND`), or `None` outside
/// 0-16.
pub fn code_name(code: i32) -> Option<&'static str> {
    Some(match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        _ => return None,
    })
}

/// The method a request path names, if it is a `MACPRuntimeService` path:
/// the method itself when the service defines it, else [`OTHER_METHOD`].
pub fn method_of(path: &str) -> Option<&'static str> {
    let method = path.strip_prefix(PATH_PREFIX)?;
    Some(
        METHODS
            .iter()
            .find(|m| **m == method)
            .copied()
            .unwrap_or(OTHER_METHOD),
    )
}

/// An OpenTelemetry [`Extractor`] over gRPC request metadata.
pub struct HeaderExtractor<'a>(pub &'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }

    fn get_all(&self, key: &str) -> Option<Vec<&str>> {
        let values: Vec<&str> = self
            .0
            .get_all(key)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        (!values.is_empty()).then_some(values)
    }
}

/// Builds the SERVER span for a request (see the module docs).
#[derive(Clone, Copy, Debug, Default)]
pub struct MakeServerSpan;

impl<B> MakeSpan<B> for MakeServerSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let Some(method) = method_of(request.uri().path()) else {
            return Span::none();
        };
        let span = tracing::info_span!(
            target: TARGET,
            "grpc",
            otel.name = format_args!("{SERVICE_NAME}/{method}"),
            otel.kind = "server",
            otel.status_code = Empty,
            otel.status_description = Empty,
            rpc.system = "grpc",
            rpc.service = SERVICE_NAME,
            rpc.method = method,
            rpc.grpc.status_code = Empty,
        );
        // A disabled span (no OpenTelemetry layer, as when only metrics
        // export) cannot take a parent; skip the extraction altogether.
        if span.is_disabled() {
            return span;
        }
        // An absent or malformed `traceparent` extracts an empty context,
        // which makes this span a root: never inherit an ambient parent.
        let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(request.headers()))
        });
        if let Err(e) = span.set_parent(parent) {
            // Not `TARGET`: this event must never reach the OpenTelemetry layer.
            tracing::debug!(error = %e, "gRPC SERVER span kept as a root: parent not set");
        }
        span
    }
}

/// Record gRPC status `code` (and the status it implies) on `span`.
fn record_status(span: &Span, code: i32) {
    span.record("rpc.grpc.status_code", i64::from(code));
    if is_server_error(code) {
        // Recording the description alone sets ERROR with that description
        // (tracing-opentelemetry 0.33 `record_str`); the code name only,
        // never the `grpc-message`.
        span.record(
            "otel.status_description",
            code_name(code).unwrap_or("UNKNOWN"),
        );
    }
}

/// The status in `headers`' `grpc-status`, if present and numeric.
fn grpc_status(headers: &HeaderMap) -> Option<i32> {
    headers
        .get("grpc-status")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Records a trailers-only response's status (tonic answers errors raised
/// before the first message, `UNIMPLEMENTED` and `NOT_FOUND` included, with
/// `grpc-status` in the response headers and no body). Emits no event.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordTrailersOnly;

impl<B> OnResponse<B> for RecordTrailersOnly {
    fn on_response(self, response: &Response<B>, _latency: Duration, span: &Span) {
        if let Some(code) = grpc_status(response.headers()) {
            record_status(span, code);
        }
    }
}

/// Records the status carried in the trailers at the end of the body.
/// Emits no event.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordEos;

impl OnEos for RecordEos {
    fn on_eos(self, trailers: Option<&HeaderMap>, _duration: Duration, span: &Span) {
        if let Some(code) = trailers.and_then(grpc_status) {
            record_status(span, code);
        }
    }
}

/// Marks a transport-level failure (the response body itself failed) as
/// ERROR, without a description: there is no code to name, and the error
/// text is never recorded. A non-OK gRPC code also reaches this hook, but
/// [`RecordTrailersOnly`] has already recorded it. Emits no event.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordFailure;

impl OnFailure<GrpcFailureClass> for RecordFailure {
    fn on_failure(&mut self, failure: GrpcFailureClass, _latency: Duration, span: &Span) {
        if let GrpcFailureClass::Error(_) = failure {
            span.record("otel.status_code", "error");
        }
    }
}

/// The tower-http layer behind [`ServerLayer`].
pub(crate) type InnerLayer = TraceLayer<
    GrpcMakeClassifier,
    MakeServerSpan,
    (),
    RecordTrailersOnly,
    (),
    RecordEos,
    RecordFailure,
>;

/// The response body the traced service produces around tonic's.
pub(crate) type TracedBody =
    ResponseBody<tonic::body::Body, GrpcEosErrorsAsFailures, (), RecordEos, RecordFailure>;

/// Re-boxes the traced body as tonic's, so the traced and untraced services
/// share one response type (which [`optional_server_layer`]'s `Either` needs).
pub(crate) type BoxBody = fn(Response<TracedBody>) -> Response<tonic::body::Body>;

fn box_body(response: Response<TracedBody>) -> Response<tonic::body::Body> {
    response.map(tonic::body::Body::new)
}

/// The SERVER span layer, for `tonic::transport::Server::layer`. Public only
/// because [`server_layer`] returns it; build it with that function.
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct ServerLayer {
    inner: InnerLayer,
}

impl<S> Layer<S> for ServerLayer {
    type Service = MapResponse<
        Trace<
            S,
            GrpcMakeClassifier,
            MakeServerSpan,
            (),
            RecordTrailersOnly,
            (),
            RecordEos,
            RecordFailure,
        >,
        BoxBody,
    >;

    fn layer(&self, inner: S) -> Self::Service {
        MapResponse::new(self.inner.layer(inner), box_body as BoxBody)
    }
}

/// The SERVER span layer. tower-http emits no event of its own:
/// `on_request` and `on_body_chunk` are `()`, and the response, end-of-stream
/// and failure hooks only record span fields.
pub fn server_layer() -> ServerLayer {
    ServerLayer {
        inner: TraceLayer::new_for_grpc()
            .make_span_with(MakeServerSpan)
            .on_request(())
            .on_response(RecordTrailersOnly)
            .on_body_chunk(())
            .on_eos(RecordEos)
            .on_failure(RecordFailure),
    }
}

/// [`server_layer`] when `telemetry` is active, else the identity layer, so an
/// inactive runtime serves exactly as it does without the feature.
pub fn optional_server_layer(telemetry: &Telemetry) -> Either<ServerLayer, Identity> {
    tower::util::option_layer(telemetry.is_enabled().then(server_layer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classifier_matches_the_otel_grpc_server_error_set() {
        for code in 0..=16 {
            let expected = matches!(code, 2 | 4 | 12 | 13 | 14 | 15);
            assert_eq!(is_server_error(code), expected, "code {code}");
            assert!(code_name(code).is_some(), "code {code}");
        }
        assert!(!is_server_error(-1));
        assert!(!is_server_error(17));
        assert_eq!(code_name(17), None);
        assert_eq!(code_name(4), Some("DEADLINE_EXCEEDED"));
        assert_eq!(code_name(12), Some("UNIMPLEMENTED"));
    }

    /// `METHODS` is exactly the generated service's method set.
    #[test]
    fn methods_match_the_generated_service() {
        let generated = include_str!(concat!(env!("OUT_DIR"), "/macp.v1.rs"));
        let mut found: Vec<&str> = generated
            .match_indices(&format!("\"{PATH_PREFIX}"))
            .map(|(i, m)| {
                let rest = &generated[i + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        found.sort_unstable();
        found.dedup();
        assert_eq!(found, METHODS.to_vec());
        assert_eq!(SERVICE_NAME, "macp.v1.MACPRuntimeService");
        assert_eq!(PATH_PREFIX, format!("/{SERVICE_NAME}/"));
    }

    #[test]
    fn only_service_paths_get_a_method() {
        assert_eq!(method_of("/macp.v1.MACPRuntimeService/Send"), Some("Send"));
        assert_eq!(
            method_of("/macp.v1.MACPRuntimeService/NoSuchMethod"),
            Some(OTHER_METHOD)
        );
        assert_eq!(method_of("/grpc.health.v1.Health/Check"), None);
        assert_eq!(
            method_of("/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"),
            None
        );
        assert_eq!(method_of("/macp.v1.MACPRuntimeServiceX/Send"), None);
    }

    #[test]
    fn header_extractor_reads_metadata_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", "00-abc-def-01".parse().unwrap());
        headers.append("tracestate", "a=1".parse().unwrap());
        headers.append("tracestate", "b=2".parse().unwrap());
        let ex = HeaderExtractor(&headers);
        assert_eq!(ex.get("TraceParent"), Some("00-abc-def-01"));
        assert_eq!(ex.get_all("tracestate"), Some(vec!["a=1", "b=2"]));
        assert_eq!(ex.get_all("missing"), None);
        let mut keys = ex.keys();
        keys.sort_unstable();
        assert_eq!(keys, vec!["traceparent", "tracestate"]);
    }

    #[test]
    fn inactive_telemetry_gives_the_identity_layer() {
        assert!(matches!(
            optional_server_layer(&Telemetry::disabled()),
            Either::Right(_)
        ));
        assert!(matches!(
            optional_server_layer(&Telemetry::enabled()),
            Either::Left(_)
        ));
    }
}
