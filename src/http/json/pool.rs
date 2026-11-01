//! The descriptor pool: all nine MACP protos (`macp.v1` plus the six
//! `macp.modes.*.v1` payload packages), compiled to a `FileDescriptorSet` by
//! the descriptor-only `protoc` run in `build.rs`.

use std::sync::OnceLock;

use prost_reflect::{DescriptorPool, MessageDescriptor, MethodDescriptor};

/// The fully-qualified name of the MACP gRPC service.
pub const SERVICE_NAME: &str = "macp.v1.MACPRuntimeService";

const ENVELOPE: &str = "macp.v1.Envelope";

static DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/macp_json_descriptor.bin"));

/// The process-wide descriptor pool, decoded once.
pub fn descriptor_pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        // The bytes are protoc's own output for the pinned protos, embedded at
        // build time; a decode failure is a build defect, not an input error.
        DescriptorPool::decode(DESCRIPTOR_SET)
            .expect("build.rs wrote an invalid macp_json_descriptor.bin")
    })
}

/// The service method named `name` (for example `"Send"`).
pub fn method_descriptor(name: &str) -> Option<MethodDescriptor> {
    descriptor_pool()
        .get_service_by_name(SERVICE_NAME)?
        .methods()
        .find(|m| m.name() == name)
}

pub(crate) fn envelope_descriptor() -> MessageDescriptor {
    message(ENVELOPE)
}

pub(crate) fn message(full_name: &str) -> MessageDescriptor {
    descriptor_pool()
        .get_message_by_name(full_name)
        .unwrap_or_else(|| panic!("{full_name} is missing from the MACP descriptor pool"))
}

pub(crate) fn is_envelope(desc: &MessageDescriptor) -> bool {
    desc.full_name() == ENVELOPE
}

pub(crate) fn is_policy_descriptor(desc: &MessageDescriptor) -> bool {
    desc.full_name() == "macp.v1.PolicyDescriptor"
}
