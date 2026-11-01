//! The HTTP SERVER span and `http.server.request.duration` (`otel` and
//! `http-listener` features only).
//!
//! Every request the HTTP binding (RFC-MACP-0006 §4) answers gets one span of
//! kind `Server`, named after the OpenTelemetry HTTP semantic conventions:
//! `{method} {route}` for a matched route (`POST /macp/v1/Send`), the method
//! alone otherwise (an `OPTIONS` preflight, a transport `404`), so a client
//! cannot grow span-name cardinality. A method outside the RFC 9110 set is
//! recorded as `_OTHER` and named `HTTP`. Its parent is the context the client
//! propagated in `traceparent`/`tracestate` (RFC-MACP-0006 §2.1), extracted
//! through the global text-map propagator with the gRPC span's
//! [`HeaderExtractor`]; an absent or malformed `traceparent` makes it a root
//! and is never an error.
//!
//! The span is entered while the handler runs, so a `process` span (which
//! takes `Span::current()` as its parent) is its child, and while the
//! response body is polled; it ends when the body ends or is dropped, so an
//! SSE stream's span lasts as long as the stream. This is the hand-written
//! counterpart of the gRPC span's tower-http `TraceLayer`: the HTTP path has
//! no tower stack.
//!
//! Attributes: `http.request.method`, `http.route` (only for a matched
//! route: the RPC path without the deployment prefix, or the well-known
//! path), `url.scheme`, `network.protocol.version` and
//! `http.response.status_code`. Nothing else: no header value, token, body,
//! payload, query, peer address or client-supplied path is recorded. Status:
//! ERROR, with no description, only for a `5xx`; every other status leaves
//! it UNSET (a `4xx` is the client's outcome, not a runtime failure).
//!
//! The same end point records `http.server.request.duration` (seconds), with
//! `http.request.method`, `http.route` and `http.response.status_code`, when
//! metrics export.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Bytes;
use http::{Request, Response, Version};
use http_body_util::BodyExt;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tracing::field::Empty;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::grpc::HeaderExtractor;
use super::instruments::Instruments;
use super::{Telemetry, TARGET};
use crate::http::server::Body;

/// The token a method outside the RFC 9110 set is recorded as.
pub const OTHER_METHOD: &str = "_OTHER";

/// `network.protocol.version` for `version`, if it is one hyper serves.
fn protocol_version(version: Version) -> Option<&'static str> {
    match version {
        Version::HTTP_10 => Some("1.0"),
        Version::HTTP_11 => Some("1.1"),
        Version::HTTP_2 => Some("2"),
        Version::HTTP_3 => Some("3"),
        _ => None,
    }
}

/// The SERVER span name: `{method} {route}`, or the method alone with no
/// route; `HTTP` stands in for [`OTHER_METHOD`].
pub fn span_name(method: &str, route: Option<&str>) -> String {
    let method = if method == OTHER_METHOD {
        "HTTP"
    } else {
        method
    };
    match route {
        Some(route) => format!("{method} {route}"),
        None => method.to_string(),
    }
}

/// The span and timing of one HTTP request, from its head to the end of its
/// response body.
pub(crate) struct RequestTrace {
    span: Span,
    start: Instant,
    method: &'static str,
    route: Option<String>,
    scheme: &'static str,
    instruments: Option<Arc<Instruments>>,
}

impl RequestTrace {
    /// Start tracing `req`, or `None` when telemetry is inactive. `method` is
    /// the normalized request method, `route` the matched route.
    pub(crate) fn start<B>(
        telemetry: &Telemetry,
        req: &Request<B>,
        method: &'static str,
        route: Option<String>,
        scheme: &'static str,
    ) -> Option<Self> {
        if !telemetry.is_enabled() {
            return None;
        }
        let span = tracing::info_span!(
            target: TARGET,
            "http",
            otel.name = span_name(method, route.as_deref()).as_str(),
            otel.kind = "server",
            otel.status_code = Empty,
            http.request.method = method,
            http.route = Empty,
            url.scheme = scheme,
            network.protocol.version = Empty,
            http.response.status_code = Empty,
            error.type = Empty,
        );
        if let Some(route) = &route {
            span.record("http.route", route.as_str());
        }
        if let Some(version) = protocol_version(req.version()) {
            span.record("network.protocol.version", version);
        }
        // A disabled span (no OpenTelemetry layer, as when only metrics
        // export) cannot take a parent; skip the extraction altogether.
        if !span.is_disabled() {
            // An absent or malformed `traceparent` extracts an empty
            // context, which makes this span a root.
            let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
                propagator.extract(&HeaderExtractor(req.headers()))
            });
            if let Err(e) = span.set_parent(parent) {
                // Not `TARGET`: this event must never reach the
                // OpenTelemetry layer.
                tracing::debug!(error = %e, "HTTP SERVER span kept as a root: parent not set");
            }
        }
        Some(Self {
            span,
            start: Instant::now(),
            method,
            route,
            scheme,
            instruments: telemetry.instruments().cloned(),
        })
    }

    /// The span, to instrument the handler with.
    pub(crate) fn span(&self) -> &Span {
        &self.span
    }

    /// Record `resp`'s status and hand the span to its body, which ends it
    /// (and records the duration) when the body ends or is dropped.
    pub(crate) fn finish(self, resp: Response<Body>) -> Response<Body> {
        let status = resp.status().as_u16();
        self.span
            .record("http.response.status_code", i64::from(status));
        if resp.status().is_server_error() {
            self.span.record("otel.status_code", "error");
            // HTTP semconv: on a 5xx with no more specific error, the
            // status code itself is `error.type` (bounded: 500..=599).
            self.span.record("error.type", resp.status().as_str());
        }
        resp.map(|inner| {
            TracedBody {
                inner,
                span: self.span,
                end: Some(End {
                    start: self.start,
                    method: self.method,
                    route: self.route,
                    scheme: self.scheme,
                    status,
                    instruments: self.instruments,
                }),
            }
            .boxed_unsync()
        })
    }
}

/// What is recorded when the response body ends.
struct End {
    start: Instant,
    method: &'static str,
    route: Option<String>,
    scheme: &'static str,
    status: u16,
    instruments: Option<Arc<Instruments>>,
}

impl End {
    fn record(self) {
        if let Some(instruments) = &self.instruments {
            instruments.record_http_request(
                self.method,
                self.route.as_deref(),
                self.scheme,
                self.status,
                self.start.elapsed().as_secs_f64(),
            );
        }
    }
}

/// A response body that keeps the SERVER span entered while polled and
/// open until it ends or is dropped.
struct TracedBody {
    inner: Body,
    span: Span,
    end: Option<End>,
}

impl TracedBody {
    /// Record the duration once and close the span.
    fn end(&mut self) {
        if let Some(end) = self.end.take() {
            end.record();
            // Dropping the last handle ends the span now, not when the
            // connection drops the (finished) body.
            self.span = Span::none();
        }
    }
}

impl HttpBody for TracedBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        let polled = {
            let _entered = this.span.enter();
            Pin::new(&mut this.inner).poll_frame(cx)
        };
        if matches!(polled, Poll::Ready(None)) {
            this.end();
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for TracedBody {
    fn drop(&mut self) {
        self.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_names_are_bounded() {
        assert_eq!(
            span_name("POST", Some("/macp/v1/Send")),
            "POST /macp/v1/Send"
        );
        assert_eq!(span_name("POST", None), "POST");
        assert_eq!(span_name("OPTIONS", None), "OPTIONS");
        assert_eq!(span_name(OTHER_METHOD, None), "HTTP");
        assert_eq!(
            span_name(OTHER_METHOD, Some("/macp/v1/Send")),
            "HTTP /macp/v1/Send"
        );
    }

    #[test]
    fn protocol_versions() {
        assert_eq!(protocol_version(Version::HTTP_11), Some("1.1"));
        assert_eq!(protocol_version(Version::HTTP_2), Some("2"));
        assert_eq!(protocol_version(Version::HTTP_09), None);
    }
}
