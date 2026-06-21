//! Generated MACP protobuf message types.
//!
//! This crate contains the prost-generated message structs for the MACP wire
//! format and all standards-track mode payloads. It is deliberately
//! transport-free: it does **not** pull in `tonic`. The gRPC service stubs for
//! `macp.v1` live in the `macp-runtime` crate (generated via `.extern_path()`
//! against the message types defined here).

pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/macp.v1.rs"));
}

pub mod decision_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.decision.v1.rs"));
}

pub mod proposal_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.proposal.v1.rs"));
}

pub mod task_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.task.v1.rs"));
}

pub mod handoff_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.handoff.v1.rs"));
}

pub mod quorum_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.quorum.v1.rs"));
}
