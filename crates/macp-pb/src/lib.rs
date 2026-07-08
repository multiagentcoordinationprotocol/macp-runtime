//! Generated MACP protobuf message types.
//!
//! This crate contains the prost-generated message structs for the MACP wire
//! format and all standards-track mode payloads. It is deliberately
//! transport-free: it does **not** pull in `tonic`. The gRPC service stubs for
//! `macp.v1` live in the `macp-runtime` crate (generated via `.extern_path()`
//! against the message types defined here).

// Doc comments in these modules are derived from the .proto source comments
// and can contain text rustdoc reads as malformed markup (e.g. `<hex>`), so
// rustdoc lints are suppressed on the generated code — there is nothing to fix
// in output we don't hand-write.
#[allow(rustdoc::all)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/macp.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod decision_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.decision.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod proposal_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.proposal.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod task_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.task.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod handoff_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.handoff.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod quorum_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.quorum.v1.rs"));
}

#[allow(rustdoc::all)]
pub mod multi_round_pb {
    include!(concat!(env!("OUT_DIR"), "/macp.modes.multi_round.v1.rs"));
}
