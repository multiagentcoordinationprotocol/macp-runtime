//! Canonical JSON mapping of MACP messages (RFC-MACP-0001 §10).
//!
//! The JSON form of a MACP message is its proto3 JSON mapping with the five
//! fixed deviations of §10.8: original field names only (§10.8(1)), 64-bit
//! integers as JSON numbers (§10.9), default values emitted (§10.10), three
//! structural transforms (§10.11), and unrecognized members ignored (§10.6).
//!
//! The codec walks the protobuf descriptors of all nine MACP protos, embedded
//! at build time by a descriptor-only `protoc` run, and works on
//! `prost_reflect::DynamicMessage` values:
//!
//! - **decoding** (JSON to message) follows proto3 JSON wherever §10.8 lists no
//!   deviation: every integer type accepts a JSON number, a quoted decimal
//!   string and exponent notation when the value is integral and in range
//!   (never rounded, §10.9); `float`/`double` accept numbers, quoted numbers
//!   and `"NaN"`/`"Infinity"`/`"-Infinity"`; enums accept names or integers,
//!   an unknown name leaving the field at its default (§10.4); `bytes` accept
//!   the standard and URL-safe alphabets, padded or not (§10.3); `null` is
//!   absent (§10.10);
//! - **encoding** (message to JSON) is canonical: original names, every field
//!   without presence emitted with its default, presence fields only when set,
//!   64-bit integers quoted only beyond 2^53−1, enum names, standard padded
//!   base64;
//! - the **structural transforms** of §10.11: `Envelope.timestamp` (RFC 3339)
//!   for `timestamp_unix_ms`, `Envelope.payload`/`payload_b64` with the
//!   payload selection of §10.12, and `PolicyDescriptor.rules` as an inlined
//!   JSON object;
//! - the **deterministic conversion** of a JSON payload to protobuf bytes
//!   (§10.13), written over the descriptors rather than delegated to a
//!   protobuf library, whose deterministic modes the RFC does not trust.
//!
//! Request bodies are parsed by a small parser that keeps each number's
//! source text, so integer fields are judged on the digits and never on a
//! rounded `f64` (§10.9). Duplicate JSON object keys keep the last
//! occurrence, as `serde_json` does. Timestamps use `chrono`, an existing
//! root dependency.
//!
//! This module is transport-free: it imports no `tonic`, `hyper` or server
//! items (a test enforces this), so it can be extracted later.

mod canonical;
mod decode;
mod encode;
mod payload;
mod pool;

use macp_pb::pb::Envelope;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::Value;

#[doc(hidden)]
pub use canonical::canonical_bytes;
#[doc(hidden)]
pub use decode::decode_message;
#[doc(hidden)]
pub use encode::encode_message;
#[doc(hidden)]
pub use payload::payload_descriptor;
#[doc(hidden)]
pub use pool::{descriptor_pool, method_descriptor, SERVICE_NAME};

/// Why a JSON body could not be converted.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsonError {
    /// The named RPC is not a method of `macp.v1.MACPRuntimeService`.
    UnknownMethod(String),
    /// The body is not valid JSON for the request message: a request-level
    /// `INVALID_REQUEST` (RFC-MACP-0006 §4.5).
    InvalidRequest(String),
    /// The body is a well-formed request, but an Envelope in it cannot be
    /// accepted as JSON (its payload or timestamp, §10.11/§10.12): reported in
    /// band as `INVALID_ENVELOPE`, with the Envelope's identifiers.
    InvalidEnvelope {
        reason: String,
        session_id: String,
        message_id: String,
    },
}

impl JsonError {
    /// The MACP error code this failure is reported with.
    pub fn code(&self) -> &'static str {
        match self {
            JsonError::UnknownMethod(_) | JsonError::InvalidRequest(_) => "INVALID_REQUEST",
            JsonError::InvalidEnvelope { .. } => "INVALID_ENVELOPE",
        }
    }

    /// The human-readable reason.
    pub fn reason(&self) -> &str {
        match self {
            JsonError::UnknownMethod(m) => m,
            JsonError::InvalidRequest(r) => r,
            JsonError::InvalidEnvelope { reason, .. } => reason,
        }
    }
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JsonError::UnknownMethod(m) => write!(f, "unknown method {m}"),
            other => write!(f, "{}: {}", other.code(), other.reason()),
        }
    }
}

impl std::error::Error for JsonError {}

/// Decode an HTTP request body (UTF-8 JSON) into the request message of the
/// service method `method` (for example `"Send"`).
#[doc(hidden)]
pub fn decode_request(method: &str, body: &[u8]) -> Result<DynamicMessage, JsonError> {
    let desc = method_descriptor(method)
        .ok_or_else(|| JsonError::UnknownMethod(decode::shown(method)))?
        .input();
    // From the text, not a `serde_json::Value`, so number text survives for
    // the exact integer rules of §10.9.
    decode::decode_message_text(&desc, body)
}

/// Encode a response message as its canonical JSON text.
#[doc(hidden)]
pub fn encode_response(message: &DynamicMessage) -> Vec<u8> {
    // Serializing a `serde_json::Value` cannot fail: its map keys are strings.
    serde_json::to_vec(&encode_message(message)).unwrap_or_default()
}

/// Decode one JSON Envelope (§10.1, §10.2, §10.11, §10.12) into the
/// generated type.
#[doc(hidden)]
pub fn envelope_from_json(value: &Value) -> Result<Envelope, JsonError> {
    let desc = pool::envelope_descriptor();
    let message = decode_message(&desc, value)?;
    message
        .transcode_to::<Envelope>()
        .map_err(|e| JsonError::InvalidRequest(format!("envelope does not transcode: {e}")))
}

/// The canonical JSON form of an Envelope. Its payload is a JSON object when
/// `(mode, message_type)` has a payload schema and the bytes round-trip
/// losslessly through the canonical encoder; otherwise `payload_b64`.
#[doc(hidden)]
pub fn envelope_to_json(envelope: &Envelope) -> Value {
    let mut message = DynamicMessage::new(pool::envelope_descriptor());
    // An `Envelope` is always a valid `macp.v1.Envelope`: transcoding between
    // the generated type and its own descriptor cannot fail.
    if message.transcode_from(envelope).is_err() {
        return Value::Null;
    }
    encode_message(&message)
}

/// The deterministic protobuf bytes (§10.13) of the JSON form of the message
/// `desc`. Unrecognized members never reach the bytes.
#[doc(hidden)]
pub fn canonical_payload_bytes(
    desc: &MessageDescriptor,
    value: &Value,
) -> Result<Vec<u8>, JsonError> {
    let message = decode_message(desc, value)?;
    Ok(canonical_bytes(&message))
}

#[cfg(test)]
mod tests;
