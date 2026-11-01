//! The HTTP listener: accept loop, connection serving, routing and the
//! transport statuses of RFC-MACP-0006 §4.1, §4.2 and §4.5.
//!
//! - **Accept loop.** A semaphore sized by `MACP_HTTP_MAX_CONNECTIONS` is
//!   acquired before each `accept`, so at the bound new connections wait in
//!   the kernel backlog instead of being accepted and dropped. Each connection
//!   holds its permit until it closes.
//! - **Connections.** Every phase before a request is bounded, so a silent
//!   or slow client cannot hold a connection permit: the TLS handshake (10 s);
//!   then the protocol choice, which on TLS is the negotiated ALPN (`h2` ⇒
//!   HTTP/2, anything else ⇒ HTTP/1.1; HTTP/2 over TLS requires ALPN, RFC
//!   9113 §3.2) and in plaintext is a sniff of the first bytes against the
//!   HTTP/2 preface under a 10 s deadline (the same deadline bounds the
//!   preface after an `h2` TLS handshake); then, on HTTP/1.1, the request-head
//!   read timeout (10 s, also between keep-alive requests) and, on HTTP/2,
//!   keep-alive PINGs (every 30 s, 20 s to answer), which also close a client
//!   that sent the preface and then nothing.
//! - **Shutdown.** The listener stops accepting when the shared shutdown
//!   signal fires, closes its socket, and asks every connection to finish
//!   gracefully; [`HttpListener::serve`] returns when all have closed. Open
//!   SSE streams end when the drain starts (a lost connection to the client,
//!   which resumes elsewhere with `Last-Event-ID`), so they do not hold the
//!   drain open; the process-wide drain deadline in `main` still force-exits
//!   any connection that never closes.
//! - **Routing.** `{prefix}/macp/v1/{Method}`, where `{prefix}` is the path of
//!   `MACP_HTTP_PUBLIC_URL`, and also `/macp/v1/{Method}`, so a proxy that
//!   strips the prefix and one that does not both work. The method set is the
//!   service descriptor's (the descriptor pool of [`super::json`]), so it
//!   tracks the proto with no list to maintain here.
//! - **Request timeout.** `MACP_REQUEST_TIMEOUT_SECS` bounds producing the
//!   response *head* of an RPC, body collection included (the deadline starts
//!   when the request head is complete, so a body trickled in slowly times
//!   out), exactly as tonic's timeout bounds a gRPC call; a streaming body
//!   (SSE) flows after the head and is therefore not subject to it. On
//!   expiry the answer is `500 INTERNAL_ERROR` "request timed out" with the
//!   §4.5 body: no registry code names a deadline, and a `Send` may already
//!   have been applied, which a retry with the same `message_id` resolves
//!   (§4.8).
//! - **RPCs.** `dispatch`: authentication from the headers before
//!   the body, the body cap, the canonical JSON decode and the `MacpServer`
//!   handler.

use std::convert::Infallible;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderValue, ALLOW, CONTENT_TYPE};
use http::{Method, Request, Response, StatusCode};
use http_body_util::combinators::UnsyncBoxBody;
#[cfg(test)]
use http_body_util::{BodyExt, Empty};
use hyper::body::Body as HttpBody;
use hyper::server::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use prost_reflect::MethodDescriptor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_rustls::TlsAcceptor;

use super::config::HttpConfig;
use super::errors::{empty, RequestError};
use super::{dispatch, tls};
use crate::server::MacpServer;

/// Every response body of the listener. Unsync so a streaming body (SSE) can
/// hold a `!Sync` stream.
pub type Body = UnsyncBoxBody<Bytes, Infallible>;

/// HTTP/1.1: a client must finish sending the request head within this.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Plaintext: the first bytes must settle HTTP/1.1 vs HTTP/2 within this.
const PROTOCOL_SNIFF_TIMEOUT: Duration = Duration::from_secs(10);
/// The HTTP/2 client connection preface (RFC 9113 §3.4).
const H2_PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
/// HTTP/2 bounds (RFC-MACP-0004 §7 DoS defences).
const H2_MAX_HEADER_LIST_SIZE: u32 = 16 * 1024;
/// Rapid-reset (CVE-2023-44487) bound: streams reset by the client before the
/// server accepted them. hyper's own default, set explicitly.
const H2_MAX_PENDING_ACCEPT_RESET_STREAMS: usize = 20;
const H2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Per-stream send buffer cap; hyper's default (400 KiB), set explicitly.
const H2_MAX_SEND_BUF_SIZE: usize = 400 * 1024;
/// Back-off after a failed `accept` (for example EMFILE), so a persistent
/// error does not spin the loop.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);

/// The RFC-MACP-0005 §7.1 well-known manifest path, at the origin root.
const WELL_KNOWN_PATH: &str = "/.well-known/macp.json";
/// The fixed part of every RPC path (RFC-MACP-0006 §4.1).
const RPC_PATH: &str = "/macp/v1/";

/// Per-listener state shared by every connection.
pub(crate) struct State {
    prefix: String,
    max_body_bytes: usize,
    request_timeout: Duration,
    /// The same server gRPC serves (shared runtime, security layer and rate
    /// buckets).
    server: MacpServer,
    /// SSE idle interval before a `: keepalive` comment (RFC-MACP-0006 §4.3:
    /// 30 s); shortened only by tests.
    sse_keepalive: Duration,
    /// Set to `true` when the listener starts draining: every open SSE
    /// stream ends at once (see [`super::sse`]).
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl State {
    pub(crate) fn from_config(config: &HttpConfig, server: MacpServer) -> Self {
        Self::new(
            config.prefix.clone(),
            config.max_body_bytes,
            config.request_timeout,
            server,
        )
    }

    fn new(
        prefix: String,
        max_body_bytes: usize,
        request_timeout: Duration,
        server: MacpServer,
    ) -> Self {
        Self {
            prefix,
            max_body_bytes,
            request_timeout,
            server,
            sse_keepalive: super::sse::KEEPALIVE_INTERVAL,
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }

    /// What an SSE stream opened now needs from the listener.
    fn sse_context(&self) -> super::sse::Context {
        super::sse::Context::new(self.sse_keepalive, self.shutdown.subscribe())
    }
}

/// The deadlines of the phases before and between requests.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timings {
    tls_handshake: Duration,
    sniff: Duration,
    header_read: Duration,
    h2_keep_alive_interval: Duration,
    h2_keep_alive_timeout: Duration,
}

impl Timings {
    const PRODUCTION: Self = Self {
        tls_handshake: tls::HANDSHAKE_TIMEOUT,
        sniff: PROTOCOL_SNIFF_TIMEOUT,
        header_read: HEADER_READ_TIMEOUT,
        h2_keep_alive_interval: H2_KEEP_ALIVE_INTERVAL,
        h2_keep_alive_timeout: H2_KEEP_ALIVE_TIMEOUT,
    };
}

/// The two protocol builders, configured with the listener's bounds.
pub(crate) struct Builders {
    h1: http1::Builder,
    h2: http2::Builder<TokioExecutor>,
    timings: Timings,
}

impl Builders {
    fn new(max_concurrent_streams: u32, timings: Timings) -> Self {
        let mut h1 = http1::Builder::new();
        h1.timer(TokioTimer::new())
            .header_read_timeout(timings.header_read);
        let mut h2 = http2::Builder::new(TokioExecutor::new());
        h2.timer(TokioTimer::new())
            .max_concurrent_streams(max_concurrent_streams)
            .max_header_list_size(H2_MAX_HEADER_LIST_SIZE)
            .max_pending_accept_reset_streams(H2_MAX_PENDING_ACCEPT_RESET_STREAMS)
            .keep_alive_interval(timings.h2_keep_alive_interval)
            .keep_alive_timeout(timings.h2_keep_alive_timeout)
            .max_send_buf_size(H2_MAX_SEND_BUF_SIZE);
        Self { h1, h2, timings }
    }
}

/// The protocol a connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Proto {
    Http1,
    Http2,
}

/// A bound, not yet serving, HTTP listener.
pub struct HttpListener {
    listener: TcpListener,
    tls: Option<TlsAcceptor>,
    builders: Arc<Builders>,
    state: Arc<State>,
    max_connections: usize,
}

impl HttpListener {
    /// Load the TLS identity (if any) and bind the socket. Every failure is
    /// fatal at startup: unlike the metrics endpoint, a configured HTTP
    /// listener that cannot serve must not leave a half-started runtime.
    /// `server` is the `MacpServer` the gRPC listener serves (a clone, so
    /// both share one runtime, security layer and set of rate buckets).
    pub async fn bind(config: &HttpConfig, server: MacpServer) -> Result<Self, String> {
        let tls = match &config.tls {
            Some(paths) => Some(tls::acceptor(&paths.cert, &paths.key)?),
            None => None,
        };
        let listener = TcpListener::bind(config.bind_addr)
            .await
            .map_err(|e| format!("MACP_HTTP_BIND_ADDR: cannot bind {}: {e}", config.bind_addr))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("MACP_HTTP_BIND_ADDR: {e}"))?;
        tracing::info!(%addr, tls = tls.is_some(), "HTTP listener bound (RFC-MACP-0006 §4)");
        if tls.is_none() {
            tracing::warn!(
                "HTTP listener starting without TLS because MACP_ALLOW_INSECURE=1; this is not \
                 RFC-compliant"
            );
        }
        Ok(Self {
            listener,
            tls,
            builders: Arc::new(Builders::new(
                config.max_concurrent_streams,
                Timings::PRODUCTION,
            )),
            state: Arc::new(State::from_config(config, server)),
            max_connections: config.max_connections,
        })
    }

    /// The bound address.
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.listener.local_addr()
    }

    /// Test support: the SSE keepalive interval (RFC-MACP-0006 §4.3's 30 s
    /// by default), shortened so a test can observe the comment. Not
    /// configurable in production; no environment variable reads it.
    #[doc(hidden)]
    ///
    /// The state is shared only by [`HttpListener::serve`], which consumes
    /// the listener, so the setting always applies; were that ever to change,
    /// this panics rather than silently keeping the default.
    pub fn with_sse_keepalive(mut self, interval: Duration) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("the listener state is unshared until `serve`")
            .sse_keepalive = interval;
        self
    }

    #[cfg(test)]
    fn with_timings(mut self, timings: Timings) -> Self {
        let streams = tests::H2_TEST_STREAMS;
        self.builders = Arc::new(Builders::new(streams, timings));
        self
    }

    /// Accept and serve until `shutdown` resolves, then drain: stop
    /// accepting, close the socket, shut every connection down gracefully,
    /// and return once all of them have closed.
    pub async fn serve(self, shutdown: impl Future<Output = ()>) {
        let Self {
            listener,
            tls,
            builders,
            state,
            max_connections,
        } = self;
        let addr = listener.local_addr().ok();
        let permits = Arc::new(Semaphore::new(max_connections));
        let graceful = GracefulShutdown::new();
        tokio::pin!(shutdown);
        loop {
            let permit = tokio::select! {
                biased;
                _ = &mut shutdown => break,
                permit = permits.clone().acquire_owned() => {
                    permit.expect("the connection semaphore is never closed")
                }
            };
            let accepted = tokio::select! {
                biased;
                _ = &mut shutdown => break,
                accepted = listener.accept() => accepted,
            };
            let (stream, _peer) = match accepted {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "HTTP listener accept failed");
                    tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                    continue;
                }
            };
            let _ = stream.set_nodelay(true);
            // Subscribed here, before any handshake, so a connection still
            // in its TLS handshake when shutdown fires is shut down too.
            let watcher = graceful.watcher();
            tokio::spawn(serve_connection(
                stream,
                tls.clone(),
                Arc::clone(&builders),
                Arc::clone(&state),
                watcher,
                permit,
            ));
        }
        drop(listener);
        if let Some(addr) = addr {
            tracing::info!(%addr, "HTTP listener closed; draining connections");
        }
        // SSE streams have no natural end, so the graceful drain would wait
        // for them until the drain deadline; they end now instead, as a lost
        // connection (no `event: error`), which a subscriber resumes with
        // `Last-Event-ID` (RFC-MACP-0006 §4.3).
        state.shutdown.send_replace(true);
        graceful.shutdown().await;
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    tls: Option<TlsAcceptor>,
    builders: Arc<Builders>,
    state: Arc<State>,
    watcher: Watcher,
    _permit: OwnedSemaphorePermit,
) {
    let t = builders.timings;
    match tls {
        None => match tokio::time::timeout(t.sniff, sniff(stream)).await {
            Ok(Ok((proto, io))) => serve_io(io, proto, &builders, state, watcher).await,
            Ok(Err(e)) => tracing::debug!(error = %e, "HTTP connection closed before a request"),
            Err(_) => tracing::debug!("HTTP connection sent no protocol bytes in time; dropped"),
        },
        Some(acceptor) => {
            match tokio::time::timeout(t.tls_handshake, acceptor.accept(stream)).await {
                // ALPN picks the protocol; HTTP/1.1 is then bounded by the
                // head-read timeout. hyper's HTTP/2 handshake waits for the
                // preface with no deadline (PINGs start only after it), so
                // the preface is read here under the sniff deadline first.
                Ok(Ok(tls_stream)) => match tls_stream.get_ref().1.alpn_protocol() {
                    Some(b"h2") => match tokio::time::timeout(t.sniff, sniff(tls_stream)).await {
                        Ok(Ok((_, io))) => {
                            serve_io(io, Proto::Http2, &builders, state, watcher).await
                        }
                        Ok(Err(e)) => {
                            tracing::debug!(error = %e, "HTTP/2 connection closed before a request")
                        }
                        Err(_) => tracing::debug!("HTTP/2 client sent no preface in time; dropped"),
                    },
                    _ => serve_io(tls_stream, Proto::Http1, &builders, state, watcher).await,
                },
                Ok(Err(e)) => tracing::debug!(error = %e, "HTTP listener TLS handshake failed"),
                Err(_) => tracing::debug!("HTTP listener TLS handshake timed out"),
            }
        }
    }
}

/// Read just enough of a plaintext connection to tell the HTTP/2 preface
/// from anything else (HTTP/1.1), and hand back a stream that replays those
/// bytes. The caller bounds it with a deadline.
pub(crate) async fn sniff<I>(mut io: I) -> std::io::Result<(Proto, Prefixed<I>)>
where
    I: AsyncRead + Unpin,
{
    let mut seen = Vec::with_capacity(H2_PREFACE.len());
    let mut chunk = [0u8; 24];
    let proto = loop {
        let want = H2_PREFACE.len() - seen.len();
        let n = io.read(&mut chunk[..want]).await?;
        if n == 0 {
            if seen.is_empty() {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            break Proto::Http1;
        }
        seen.extend_from_slice(&chunk[..n]);
        if seen[..] != H2_PREFACE[..seen.len()] {
            break Proto::Http1;
        }
        if seen.len() == H2_PREFACE.len() {
            break Proto::Http2;
        }
    };
    Ok((
        proto,
        Prefixed {
            prefix: seen,
            pos: 0,
            inner: io,
        },
    ))
}

/// A stream that first yields `prefix`, then reads from `inner`; writes go
/// straight to `inner`.
pub(crate) struct Prefixed<I> {
    prefix: Vec<u8>,
    pos: usize,
    inner: I,
}

impl<I: AsyncRead + Unpin> AsyncRead for Prefixed<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.pos < self.prefix.len() {
            let n = (self.prefix.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.prefix[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for Prefixed<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Serve one established connection with the chosen protocol until it
/// closes.
pub(crate) async fn serve_io<I>(
    io: I,
    proto: Proto,
    builders: &Builders,
    state: Arc<State>,
    watcher: Watcher,
) where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |req| {
        let state = Arc::clone(&state);
        async move { Ok::<_, Infallible>(handle(&state, req).await) }
    });
    let io = TokioIo::new(io);
    let result = match proto {
        Proto::Http1 => {
            watcher
                .watch(builders.h1.serve_connection(io, service))
                .await
        }
        Proto::Http2 => {
            watcher
                .watch(builders.h2.serve_connection(io, service))
                .await
        }
    };
    if let Err(e) = result {
        tracing::debug!(error = %e, "HTTP connection ended with an error");
    }
}

/// Where a request path leads.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Route {
    /// `{prefix}/macp/v1/{Method}` or `/macp/v1/{Method}` naming an RPC.
    Rpc(MethodDescriptor),
    /// `/.well-known/macp.json` (RFC-MACP-0005 §7.1).
    WellKnown,
    /// Anything else, including an RPC path naming no RPC.
    NotFound,
}

/// Route a request path (no query). The method segment must be the RPC name
/// exactly: no trailing slash, no percent-encoding, case-sensitive.
pub(crate) fn route(path: &str, prefix: &str) -> Route {
    if path == WELL_KNOWN_PATH {
        return Route::WellKnown;
    }
    let after_prefix = (!prefix.is_empty())
        .then(|| path.strip_prefix(prefix))
        .flatten()
        .and_then(|rest| rest.strip_prefix(RPC_PATH));
    let name = after_prefix.or_else(|| path.strip_prefix(RPC_PATH));
    match name.and_then(super::json::method_descriptor) {
        Some(method) => Route::Rpc(method),
        None => Route::NotFound,
    }
}

/// RFC-MACP-0006 §4.2: the media type compared case-insensitively, and a
/// `charset` parameter, if present, must be `utf-8`. Exactly one
/// `Content-Type` header must be present.
pub(crate) fn is_json_content_type<'a>(mut values: impl Iterator<Item = &'a HeaderValue>) -> bool {
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(parts) = split_params(value) else {
        return false;
    };
    let mut parts = parts.into_iter();
    let media = parts.next().unwrap_or_default().trim();
    if !media.eq_ignore_ascii_case("application/json") {
        return false;
    }
    for param in parts {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        let Some((name, raw)) = param.split_once('=') else {
            return false;
        };
        if name.trim().eq_ignore_ascii_case("charset")
            && !unquote(raw.trim()).eq_ignore_ascii_case("utf-8")
        {
            return false;
        }
    }
    true
}

/// Split a header value on `;` outside quoted strings (RFC 9110 §5.6.4,
/// with `\` quoted-pairs). `None` for an unterminated quoted string.
pub(crate) fn split_params(value: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (i, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&value[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    parts.push(&value[start..]);
    Some(parts)
}

/// A parameter value, with a quoted string's quotes and quoted-pairs removed.
pub(crate) fn unquote(raw: &str) -> String {
    match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        None => raw.to_string(),
        Some(inner) => {
            let mut out = String::with_capacity(inner.len());
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                out.push(if c == '\\' {
                    chars.next().unwrap_or(c)
                } else {
                    c
                });
            }
            out
        }
    }
}

/// Answer one request.
pub(crate) async fn handle<B>(state: &State, req: Request<B>) -> Response<Body>
where
    B: HttpBody<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    // CORS preflight belongs to P7 (RFC-MACP-0006 §4.10); until then OPTIONS
    // is answered like an unserved path.
    if req.method() == Method::OPTIONS {
        return empty(StatusCode::NOT_FOUND);
    }
    match route(req.uri().path(), &state.prefix) {
        // Discovery belongs to P8 (RFC-MACP-0005 §7.1); placeholder until then.
        Route::WellKnown => empty(StatusCode::NOT_FOUND),
        Route::NotFound => empty(StatusCode::NOT_FOUND),
        Route::Rpc(_) if req.method() != Method::POST => {
            let mut resp = empty(StatusCode::METHOD_NOT_ALLOWED);
            resp.headers_mut()
                .insert(ALLOW, HeaderValue::from_static("POST"));
            resp
        }
        Route::Rpc(_) if !is_json_content_type(req.headers().get_all(CONTENT_TYPE).iter()) => {
            empty(StatusCode::UNSUPPORTED_MEDIA_TYPE)
        }
        Route::Rpc(method) => {
            let serve = dispatch::serve(
                &state.server,
                state.max_body_bytes,
                method,
                req,
                state.sse_context(),
            );
            match tokio::time::timeout(state.request_timeout, serve).await {
                Ok(resp) => resp,
                // No registry code names a deadline; INTERNAL_ERROR with the
                // §4.5 body (a Send may have been applied: §4.8 retry).
                Err(_) => RequestError::new("INTERNAL_ERROR", "request timed out").into_response(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;

    const METHODS: usize = 27;
    /// `max_concurrent_streams` of the in-process listener tests.
    pub(super) const H2_TEST_STREAMS: u32 = 8;

    fn state(prefix: &str) -> State {
        let runtime = Arc::new(crate::runtime::Runtime::new(
            Arc::new(crate::storage::MemoryBackend),
            Arc::new(crate::registry::SessionRegistry::new()),
            Arc::new(crate::log_store::LogStore::new()),
        ));
        State::new(
            prefix.into(),
            16,
            Duration::from_secs(5),
            MacpServer::new(runtime, crate::security::SecurityLayer::dev_mode()),
        )
    }

    fn name(r: Route) -> Option<String> {
        match r {
            Route::Rpc(m) => Some(m.name().to_string()),
            _ => None,
        }
    }

    #[test]
    fn every_service_method_routes() {
        let service = crate::http::json::descriptor_pool()
            .get_service_by_name(crate::http::json::SERVICE_NAME)
            .unwrap();
        assert_eq!(service.methods().len(), METHODS);
        for m in service.methods() {
            let path = format!("/macp/v1/{}", m.name());
            assert_eq!(name(route(&path, "")).as_deref(), Some(m.name()));
            let prefixed = format!("/api/macp/v1/{}", m.name());
            assert_eq!(name(route(&prefixed, "/api")).as_deref(), Some(m.name()));
            // Served without the prefix as well.
            assert_eq!(name(route(&path, "/api")).as_deref(), Some(m.name()));
        }
    }

    #[test]
    fn route_edges() {
        for path in [
            "/macp/v1/Nope",
            "/macp/v1/Send/",
            "/macp/v1/send",
            "/macp/v1/%53end",
            "/macp/v1/",
            "/macp/v1",
            "/macp/v2/Send",
            "/",
            "",
            "/api/Send",
            "/other/macp/v1/Send",
            "//macp/v1/Send",
            "/.well-known/macp.json/",
        ] {
            assert_eq!(route(path, "/api"), Route::NotFound, "{path}");
        }
        assert_eq!(route("/.well-known/macp.json", ""), Route::WellKnown);
        // At the origin root whatever the prefix is.
        assert_eq!(route("/.well-known/macp.json", "/api"), Route::WellKnown);
        assert_eq!(route("/api/.well-known/macp.json", "/api"), Route::NotFound);
        // A prefix that is itself /macp/v1.
        assert_eq!(
            name(route("/macp/v1/macp/v1/Send", "/macp/v1")).as_deref(),
            Some("Send")
        );
        assert_eq!(
            name(route("/macp/v1/Send", "/macp/v1")).as_deref(),
            Some("Send")
        );
        // The prefix matches whole segments only through RPC_PATH.
        assert_eq!(route("/apix/macp/v1/Send", "/api"), Route::NotFound);
    }

    fn ct(values: &[&str]) -> bool {
        let values: Vec<HeaderValue> = values
            .iter()
            .map(|v| HeaderValue::from_str(v).unwrap())
            .collect();
        is_json_content_type(values.iter())
    }

    #[test]
    fn media_type_parsing() {
        for ok in [
            "application/json",
            "Application/JSON",
            "application/json; charset=utf-8",
            "application/json;charset=UTF-8",
            "application/json; charset=\"utf-8\"",
            "application/json ; charset = utf-8 ",
            "application/json; foo=bar",
            "application/json;",
            "application/json; x=\"a;b\"",
            "application/json; x=\"a;b\"; charset=utf-8",
            "application/json; x=\"q\\\"; still quoted\"; charset=utf-8",
            "application/json; charset=\"u\\tf-8\"",
        ] {
            assert!(ct(&[ok]), "{ok}");
        }
        for bad in [
            "",
            "text/plain",
            "application/macp-envelope+json",
            "application/json-seq",
            "application/jsonx",
            "application/json; charset=latin1",
            "application/json; charset=utf8",
            "application/json; charset=utf-8; charset=latin1",
            "application/json; charset",
            "application/*",
            "*/*",
            "application/json; x=\"a;b\"; charset=latin1",
            "application/json; x=\"unterminated; charset=utf-8",
            "application/json; x=\"a;charset=latin1\"x; charset=latin1",
        ] {
            assert!(!ct(&[bad]), "{bad}");
        }
        assert!(!ct(&[]), "missing");
        assert!(!ct(&["application/json", "application/json"]), "duplicate");
        let latin1 = HeaderValue::from_bytes(b"application/json; x=\xe9").unwrap();
        assert!(!is_json_content_type(std::iter::once(&latin1)));
    }

    fn req(
        method: Method,
        path: &str,
        ct: Option<&str>,
        body: &'static str,
    ) -> Request<Full<Bytes>> {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(ct) = ct {
            b = b.header(CONTENT_TYPE, ct);
        }
        b.body(Full::new(Bytes::from_static(body.as_bytes())))
            .unwrap()
    }

    async fn status_of(r: Request<Full<Bytes>>) -> (StatusCode, http::HeaderMap, Bytes) {
        let resp = handle(&state("/api"), r).await;
        let (parts, body) = resp.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        (parts.status, parts.headers, body)
    }

    #[tokio::test]
    async fn transport_statuses() {
        let json = Some("application/json");
        let (s, h, b) = status_of(req(Method::GET, "/macp/v1/Send", json, "")).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h.get(ALLOW).unwrap(), "POST");
        assert!(b.is_empty());
        for m in [Method::PUT, Method::HEAD, Method::DELETE, Method::PATCH] {
            let (s, h, _) = status_of(req(m.clone(), "/api/macp/v1/GetSession", json, "")).await;
            assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED, "{m}");
            assert_eq!(h.get(ALLOW).unwrap(), "POST");
        }
        // An unknown method is 404 whatever the HTTP method.
        for m in [Method::POST, Method::GET] {
            let (s, h, b) = status_of(req(m, "/macp/v1/Nope", json, "{}")).await;
            assert_eq!(s, StatusCode::NOT_FOUND);
            assert!(h.get(ALLOW).is_none());
            assert!(b.is_empty());
        }
        let (s, _, b) =
            status_of(req(Method::POST, "/macp/v1/Send", Some("text/plain"), "{}")).await;
        assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(b.is_empty());
        let (s, _, _) = status_of(req(Method::POST, "/macp/v1/Send", None, "{}")).await;
        assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let (s, _, _) = status_of(req(
            Method::POST,
            "/macp/v1/Send",
            Some("application/json; charset=latin1"),
            "{}",
        ))
        .await;
        assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        // An RPC is served (P5): authentication precedes the body.
        let (s, h, _) = status_of(req(Method::POST, "/api/macp/v1/Send", json, "{}")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(h.get("www-authenticate").unwrap(), "Bearer");
        let (s, _, _) = status_of(req(Method::POST, "/api/macp/v1/ListModes", json, "{}")).await;
        assert_eq!(s, StatusCode::OK);
        // A streaming RPC authenticates like a unary one (no Accept header
        // admits the stream); CORS and discovery until P7/P8.
        let (s, _, _) = status_of(req(Method::POST, "/macp/v1/WatchSignals", json, "{}")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        let (s, _, _) = status_of(req(Method::OPTIONS, "/macp/v1/Send", None, "")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = status_of(req(Method::GET, "/.well-known/macp.json", None, "")).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn body_cap() {
        let json = Some("application/json");
        // At the cap: read, then refused as JSON. Over it: 413 before decoding.
        let (s, _, _) = status_of(req(
            Method::POST,
            "/macp/v1/ListModes",
            json,
            "0123456789abcdef",
        ))
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, h, _) = status_of(req(
            Method::POST,
            "/macp/v1/ListModes",
            json,
            "0123456789abcdefX",
        ))
        .await;
        assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(h.get("macp-error-code").unwrap(), "PAYLOAD_TOO_LARGE");
    }

    #[tokio::test]
    async fn request_timeout_covers_body_collection() {
        // The body never arrives, so only the timeout can answer.
        let st = State {
            request_timeout: Duration::from_millis(50),
            ..state("")
        };
        let (_tx, body) =
            tokio::sync::mpsc::channel::<Result<hyper::body::Frame<Bytes>, Infallible>>(1);
        let body =
            http_body_util::StreamBody::new(tokio_stream::wrappers::ReceiverStream::new(body));
        let r = Request::builder()
            .method(Method::POST)
            .uri("/macp/v1/ListModes")
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        let resp = handle(&st, r).await;
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            resp.headers().get("macp-error-code").unwrap(),
            "INTERNAL_ERROR"
        );
    }

    /// The connection path in process: HTTP/1.1 and HTTP/2 (prior knowledge)
    /// over a duplex pipe, through the plaintext sniff and the real builders.
    #[tokio::test]
    async fn serves_http1_and_http2_over_duplex() {
        use hyper::client::conn::{http1, http2};
        let builders = Arc::new(Builders::new(8, Timings::PRODUCTION));
        let graceful = GracefulShutdown::new();
        for h2 in [false, true] {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let st = Arc::new(state(""));
            let watcher = graceful.watcher();
            let b = Arc::clone(&builders);
            tokio::spawn(async move {
                let (proto, io) = sniff(server_io).await.unwrap();
                assert_eq!(proto == Proto::Http2, h2);
                serve_io(io, proto, &b, st, watcher).await
            });
            let request = Request::builder()
                .method(Method::GET)
                .uri("/macp/v1/Send")
                .body(Empty::<Bytes>::new())
                .unwrap();
            let resp = if h2 {
                let (mut send, conn) =
                    http2::handshake(TokioExecutor::new(), TokioIo::new(client_io))
                        .await
                        .unwrap();
                tokio::spawn(conn);
                send.send_request(request).await.unwrap()
            } else {
                let (mut send, conn) = http1::handshake(TokioIo::new(client_io)).await.unwrap();
                tokio::spawn(conn);
                send.send_request(request).await.unwrap()
            };
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED, "h2={h2}");
            let expected = if h2 {
                http::Version::HTTP_2
            } else {
                http::Version::HTTP_11
            };
            assert_eq!(resp.version(), expected);
        }
    }

    #[tokio::test]
    async fn sniff_replays_what_it_read() {
        for (input, proto) in [
            (&b"GET / HTTP/1.1\r\n\r\n"[..], Proto::Http1),
            (&b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\nrest"[..], Proto::Http2),
            (&b"PRI * HTTP/1.1"[..], Proto::Http1),
            (&b"P"[..], Proto::Http1), // EOF after a partial match
        ] {
            let (got, mut io) = sniff(input).await.unwrap();
            assert_eq!(got, proto);
            let mut all = Vec::new();
            io.read_to_end(&mut all).await.unwrap();
            assert_eq!(all, input);
        }
        assert!(sniff(&b""[..]).await.is_err());
    }

    // ---- pre-request deadlines (a silent client cannot hold a permit) -----

    /// Small deadlines so the tests are fast; the production values are 10 s
    /// (sniff, head, handshake) and 30 s + 20 s (HTTP/2 PING).
    const FAST: Timings = Timings {
        tls_handshake: Duration::from_millis(300),
        sniff: Duration::from_millis(300),
        header_read: Duration::from_millis(300),
        h2_keep_alive_interval: Duration::from_millis(150),
        h2_keep_alive_timeout: Duration::from_millis(150),
    };
    /// Generous bound on "dropped within the deadline" for a loaded CI box.
    const DROPPED_WITHIN: Duration = Duration::from_secs(5);

    struct Running {
        addr: std::net::SocketAddr,
        cert: Option<rustls_pki::CertificateDer<'static>>,
        _dir: tempfile::TempDir,
    }

    use tokio_rustls::rustls::pki_types as rustls_pki;

    /// A real listener on loopback with `MACP_HTTP_MAX_CONNECTIONS=1`.
    async fn run_listener(tls: bool) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let mut vars = vec![
            ("MACP_HTTP_BIND_ADDR".to_string(), "127.0.0.1:0".to_string()),
            ("MACP_HTTP_MAX_CONNECTIONS".into(), "1".into()),
            ("MACP_ALLOW_INSECURE".into(), "1".into()),
        ];
        let mut cert = None;
        if tls {
            let ck = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let (c, k) = (dir.path().join("c.pem"), dir.path().join("k.pem"));
            std::fs::write(&c, ck.cert.pem()).unwrap();
            std::fs::write(&k, ck.key_pair.serialize_pem()).unwrap();
            vars.push(("MACP_TLS_CERT_PATH".into(), c.display().to_string()));
            vars.push(("MACP_TLS_KEY_PATH".into(), k.display().to_string()));
            cert = Some(ck.cert.der().clone());
        }
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        let config = HttpConfig::from_env(&move |k| map.get(k).cloned())
            .unwrap()
            .unwrap();
        let listener = HttpListener::bind(&config, state("").server)
            .await
            .unwrap()
            .with_timings(FAST);
        let addr = listener.local_addr().unwrap();
        tokio::spawn(listener.serve(std::future::pending()));
        Running {
            addr,
            cert,
            _dir: dir,
        }
    }

    fn connector(
        cert: &rustls_pki::CertificateDer<'static>,
        alpn: &[&[u8]],
    ) -> tokio_rustls::TlsConnector {
        use tokio_rustls::rustls;
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
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

    /// Wait until the server closes `io` (EOF or reset), bounded.
    async fn closed_by_server<I: AsyncRead + Unpin>(io: &mut I) {
        tokio::time::timeout(DROPPED_WITHIN, async {
            let mut buf = [0u8; 1024];
            loop {
                match io.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {} // SETTINGS, PING, a 408...: keep reading
                }
            }
        })
        .await
        .expect("the server kept a silent connection open past its deadline");
    }

    /// The listener's single permit is free again: a new client is served.
    async fn next_client_served(addr: std::net::SocketAddr) {
        use hyper::client::conn::http1;
        tokio::time::timeout(DROPPED_WITHIN, async {
            let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (mut send, conn) = http1::handshake(TokioIo::new(tcp)).await.unwrap();
            tokio::spawn(conn);
            let resp = send
                .send_request(
                    Request::builder()
                        .uri("/macp/v1/Send")
                        .header("host", "x")
                        .body(Empty::<Bytes>::new())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        })
        .await
        .expect("the second client was not served");
    }

    /// Plaintext: a client that sends nothing, half a preface, a partial
    /// HTTP/1.1 head, or the whole preface and then nothing, is dropped
    /// within its deadline; meanwhile it holds the only permit, and the next
    /// client is served once it is gone.
    #[tokio::test]
    async fn silent_plaintext_clients_are_dropped() {
        use tokio::io::AsyncWriteExt;
        let rt = run_listener(false).await;
        for sent in [
            &b""[..],
            &H2_PREFACE[..10],
            &b"GET /macp/v1/Send HTTP/1.1\r\n"[..],
            &H2_PREFACE[..],
        ] {
            let mut silent = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
            silent.write_all(sent).await.unwrap();
            closed_by_server(&mut silent).await;
            next_client_served(rt.addr).await;
        }
        // An idle HTTP/1.1 keep-alive connection after a request is bounded
        // by the head-read timeout as well.
        let mut idle = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
        idle.write_all(b"GET /macp/v1/Send HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        closed_by_server(&mut idle).await;
        next_client_served(rt.addr).await;
    }

    /// The permit really is held while the silent client waits: a second
    /// client is not served until the first is dropped.
    #[tokio::test]
    async fn a_silent_client_holds_the_permit_only_until_its_deadline() {
        let rt = run_listener(false).await;
        let started = std::time::Instant::now();
        let mut silent = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
        // Give the accept loop the permit first.
        let next = tokio::spawn(next_client_served(rt.addr));
        closed_by_server(&mut silent).await;
        next.await.unwrap();
        assert!(started.elapsed() >= FAST.sniff, "{:?}", started.elapsed());
    }

    /// TLS: a silent client during the handshake, and after a handshake that
    /// negotiated `http/1.1`, `h2` or no ALPN at all, is dropped within its
    /// deadline, and the next client is then served.
    #[tokio::test]
    async fn silent_tls_clients_are_dropped() {
        let rt = run_listener(true).await;
        let cert = rt.cert.clone().unwrap();
        let mut raw = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
        closed_by_server(&mut raw).await;
        next_client_served_tls(&rt, &cert).await;
        for (alpn, negotiated) in [
            (&[&b"http/1.1"[..]][..], Some(&b"http/1.1"[..])),
            (&[&b"h2"[..]][..], Some(&b"h2"[..])),
            (&[][..], None),
        ] {
            let tcp = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
            let name = rustls_pki::ServerName::try_from("localhost").unwrap();
            let mut tls = connector(&cert, alpn).connect(name, tcp).await.unwrap();
            assert_eq!(tls.get_ref().1.alpn_protocol(), negotiated);
            closed_by_server(&mut tls).await;
            next_client_served_tls(&rt, &cert).await;
        }
        // h2: the whole preface, then nothing (bounded by the PING timeout).
        let tcp = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
        let name = rustls_pki::ServerName::try_from("localhost").unwrap();
        let mut tls = connector(&cert, &[b"h2"]).connect(name, tcp).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut tls, H2_PREFACE)
            .await
            .unwrap();
        closed_by_server(&mut tls).await;
        next_client_served_tls(&rt, &cert).await;
    }

    async fn next_client_served_tls(rt: &Running, cert: &rustls_pki::CertificateDer<'static>) {
        use hyper::client::conn::http1;
        tokio::time::timeout(DROPPED_WITHIN, async {
            let tcp = tokio::net::TcpStream::connect(rt.addr).await.unwrap();
            let name = rustls_pki::ServerName::try_from("localhost").unwrap();
            let tls = connector(cert, &[b"http/1.1"])
                .connect(name, tcp)
                .await
                .unwrap();
            let (mut send, conn) = http1::handshake(TokioIo::new(tls)).await.unwrap();
            tokio::spawn(conn);
            let resp = send
                .send_request(
                    Request::builder()
                        .uri("/macp/v1/Send")
                        .header("host", "x")
                        .body(Empty::<Bytes>::new())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        })
        .await
        .expect("the next TLS client was not served");
    }
}
