pub mod pb {
    // Message types come from the transport-free `macp-pb` crate; the tonic
    // service stubs (generated here via `.extern_path`) are merged in so the
    // historical `macp_runtime::pb::*` surface is preserved exactly.
    pub use macp_pb::pb::*;
    tonic::include_proto!("macp.v1");
}

pub use macp_pb::{decision_pb, handoff_pb, proposal_pb, quorum_pb, task_pb};

pub mod error;
pub mod log_store;
pub mod metrics;
pub mod mode;
pub mod mode_registry;
pub mod policy;
pub mod registry;
pub mod replay;
pub mod runtime;
pub mod session;
pub mod storage;
pub mod stream_bus;

pub mod auth;
pub mod extensions;
pub mod security;
