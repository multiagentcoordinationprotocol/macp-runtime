//! The HTTP/JSON transport binding (RFC-MACP-0006 §4), compiled only with the
//! non-default `http-listener` Cargo feature.
//!
//! The binding is a second, opt-in surface over the same `MacpServer` the gRPC
//! binding serves: at runtime the listener starts only when
//! `MACP_HTTP_BIND_ADDR` is set. Its building blocks are crate-internal; this
//! module is public so the documentation of the binding is published, not to
//! offer a stable library API.
//!
//! - [`json`]: the canonical JSON mapping of every MACP message
//!   (RFC-MACP-0001 §10), a transport-free codec.
//! - [`config`]: `MACP_HTTP_*` startup configuration, validated before
//!   anything binds.
//! - [`server`]: the listener (accept loop, HTTP/1.1 + HTTP/2, graceful
//!   shutdown), routing and the transport statuses of §4.1/§4.2/§4.5.
//! - `dispatch`: the RPC table and the unary RPCs (authentication before the
//!   body, the body cap, `Send`'s two layers, §4.4).
//! - `errors`: request-level errors (§4.5) and JSON responses.
//! - `metadata`: the request headers a handler sees as gRPC metadata.
//! - `sse`: the streaming RPCs as server-sent events (§4.3): `Accept`
//!   negotiation, `Last-Event-ID` resume, framing, keepalive and how each
//!   stream ends.
//! - `tls`: the rustls acceptor (ring provider, ALPN `h2`/`http/1.1`).

pub mod config;
mod dispatch;
mod errors;
pub mod json;
mod metadata;
pub mod server;
mod sse;
mod tls;

#[doc(hidden)]
pub use config::HttpConfig;
#[doc(hidden)]
pub use server::HttpListener;
