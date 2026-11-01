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

pub mod json;
