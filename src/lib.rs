pub mod pb {
    // Message types come from the transport-free `macp-pb` crate; the tonic
    // service stubs (generated here via `.extern_path`) are merged in so the
    // historical `macp_runtime::pb::*` surface is preserved exactly.
    pub use macp_pb::pb::*;
    tonic::include_proto!("macp.v1");
}

pub use macp_pb::{decision_pb, handoff_pb, multi_round_pb, proposal_pb, quorum_pb, task_pb};

// The base vocabulary crate, re-exported whole. `macp-runtime`'s public
// signatures already traffic in macp-core types, but four of the five lower
// crates were reachable through this root and macp-core was not — so items
// with no historical `macp_runtime::*` path (`commitment_hash`,
// `mode::MessageContext`) could only be named by taking a second dependency.
// Not aliased to `core`: that shadows the `core` extern prelude crate-wide.
// The `error`/`session` shims below stay as the historical short paths.
pub use macp_core;

pub mod error;
pub mod metrics;
pub mod replay;
pub mod runtime;
pub mod session;
pub mod stream_bus;

// The mode layer and registry now live in `macp-modes`; the default governance
// policy engine in `macp-policy`. Re-exported so existing `crate::mode`,
// `crate::mode_registry`, and `crate::policy::*` paths (and the equivalent
// downstream `macp_runtime::*` paths) resolve unchanged.
pub use macp_modes::{mode, mode_registry};
pub use macp_policy as policy;

// The persistence layer (append-only log, session registry, storage backends)
// now lives in `macp-storage`. Re-exported so `macp_runtime::{log_store,
// registry, storage}` paths resolve unchanged.
pub use macp_storage::{log_store, registry, storage};

pub mod extensions;
// Crate-private: the ListSessions page-token codec is a transport detail of
// `server.rs`, deliberately not part of the published API surface.
mod pagination;
pub mod policy_engine;
pub mod server;

// Authentication and the request security layer now live in `macp-auth`.
// Re-exported so `crate::{auth, security}` and downstream
// `macp_runtime::{auth, security}` paths resolve unchanged.
pub use macp_auth::{auth, security};
