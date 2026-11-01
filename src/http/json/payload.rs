//! Payload selection (RFC-MACP-0001 §10.12): which payload message a JSON
//! `payload` is, chosen by the Envelope's `(mode, message_type)` pair.
//!
//! - A Core message type (`Signal`, `Progress`, `SessionStart`,
//!   `SessionCancel`, `SessionSuspend`, `SessionResume`) selects
//!   `macp.v1.{T}Payload` whatever the mode; `Commitment` selects
//!   `macp.v1.CommitmentPayload`.
//! - Any other type selects `{package}.{message_type}Payload` in the package of
//!   the Mode named by `mode`: `macp.mode.<m>.v1` maps to
//!   `macp.modes.<m>.v1`, and the built-in extension `ext.multi_round.v1` to
//!   `macp.modes.multi_round.v1`.
//!
//! Anything else (a dynamically registered extension Mode, an unknown
//! `message_type`) has no schema: a JSON `payload` for it is refused with
//! `INVALID_ENVELOPE`, and only `payload_b64` can carry it.

use prost_reflect::MessageDescriptor;

use super::pool::descriptor_pool;

/// The Core message types whose payload is in `core.proto` whatever the mode.
pub(crate) const CORE_MESSAGE_TYPES: [&str; 6] = [
    "Signal",
    "Progress",
    "SessionStart",
    "SessionCancel",
    "SessionSuspend",
    "SessionResume",
];

/// The payload message for `(mode, message_type)`, or `None` when this
/// runtime has no schema for the pair.
pub fn payload_descriptor(mode: &str, message_type: &str) -> Option<MessageDescriptor> {
    if !is_identifier(message_type) {
        return None;
    }
    let full_name = if CORE_MESSAGE_TYPES.contains(&message_type) || message_type == "Commitment" {
        format!("macp.v1.{message_type}Payload")
    } else {
        format!("{}.{message_type}Payload", mode_package(mode)?)
    };
    descriptor_pool().get_message_by_name(&full_name)
}

/// The protobuf package holding the payload messages of `mode`.
fn mode_package(mode: &str) -> Option<String> {
    if mode == "ext.multi_round.v1" {
        return Some("macp.modes.multi_round.v1".to_string());
    }
    let name = mode.strip_prefix("macp.mode.")?.strip_suffix(".v1")?;
    is_identifier(name).then(|| format!("macp.modes.{name}.v1"))
}

/// A single protobuf identifier segment, so a crafted `mode` or
/// `message_type` (with dots) cannot select a message outside the table.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
