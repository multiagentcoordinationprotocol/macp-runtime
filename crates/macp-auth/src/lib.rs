//! `macp-auth` — MACP authentication and the request security layer.
//!
//! Holds the [`security`] layer (caller-identity derivation, rate limiting, and
//! payload-size limits) and the pluggable auth [`auth`] resolver chain with its
//! static-bearer and JWT-bearer implementations, and the [`env`](mod@env) registry of
//! every `MACP_*` environment variable the workspace reads. The heavier auth dependencies
//! (`jsonwebtoken`, `reqwest`) are confined to this crate so the kernel and
//! library consumers don't pull them in.

pub mod auth;
pub mod env;
pub mod security;
