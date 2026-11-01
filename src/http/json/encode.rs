//! `DynamicMessage` to canonical JSON (RFC-MACP-0001 §10.1-§10.11).
//!
//! Original field names (§10.8(1)); every field without presence emitted with
//! its default, presence fields only when set (§10.10); 64-bit integers as
//! JSON numbers, quoted beyond ±(2^53−1) (§10.9); enum names, or the integer
//! for a value the enum does not name (§10.4); bytes as standard padded
//! base64 (§10.3); and the three structural transforms reversed (§10.11).

use base64::Engine as _;
use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value as PbValue,
};
use serde_json::{Map, Number, Value};

use super::canonical::canonical_bytes;
use super::payload::payload_descriptor;
use super::pool::{is_envelope, is_policy_descriptor};

/// 2^53−1, the largest magnitude a 64-bit value is emitted as a bare number.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The canonical JSON form of `message`.
pub fn encode_message(message: &DynamicMessage) -> Value {
    let desc = message.descriptor();
    let mut obj = Map::new();
    for field in desc.fields() {
        if field.supports_presence() && !message.has_field(&field) {
            continue;
        }
        let name = field.name();
        if is_envelope(&desc) && (name == "timestamp_unix_ms" || name == "payload") {
            continue;
        }
        let value = message.get_field(&field);
        let json = if is_policy_descriptor(&desc) && name == "rules" {
            encode_rules(value.as_str().unwrap_or_default())
        } else {
            encode_field(&field, &value)
        };
        obj.insert(name.to_string(), json);
    }
    if is_envelope(&desc) {
        encode_envelope_transforms(message, &mut obj);
    }
    Value::Object(obj)
}

fn encode_field(field: &FieldDescriptor, value: &PbValue) -> Value {
    let kind = field.kind();
    match value {
        PbValue::List(items) => {
            Value::Array(items.iter().map(|v| encode_single(&kind, v)).collect())
        }
        PbValue::Map(map) => {
            let value_kind = match &kind {
                Kind::Message(entry) => entry.map_entry_value_field().kind(),
                _ => return Value::Object(Map::new()),
            };
            Value::Object(
                map.iter()
                    .map(|(k, v)| (map_key_string(k), encode_single(&value_kind, v)))
                    .collect(),
            )
        }
        single => encode_single(&kind, single),
    }
}

fn map_key_string(key: &MapKey) -> String {
    match key {
        MapKey::Bool(b) => b.to_string(),
        MapKey::I32(n) => n.to_string(),
        MapKey::I64(n) => n.to_string(),
        MapKey::U32(n) => n.to_string(),
        MapKey::U64(n) => n.to_string(),
        MapKey::String(s) => s.clone(),
    }
}

fn encode_single(kind: &Kind, value: &PbValue) -> Value {
    match value {
        PbValue::Bool(b) => Value::Bool(*b),
        PbValue::I32(n) => Value::from(*n),
        PbValue::U32(n) => Value::from(*n),
        PbValue::I64(n) => {
            if n.unsigned_abs() > MAX_SAFE_INTEGER {
                Value::String(n.to_string())
            } else {
                Value::from(*n)
            }
        }
        PbValue::U64(n) => {
            if *n > MAX_SAFE_INTEGER {
                Value::String(n.to_string())
            } else {
                Value::from(*n)
            }
        }
        PbValue::F64(f) => encode_float(*f),
        // The shortest decimal that round-trips the `f32`, not its widened
        // `f64` expansion (0.1f32 is `0.1`, not `0.10000000149011612`).
        PbValue::F32(f) => {
            if f.is_finite() {
                encode_float(f.to_string().parse::<f64>().unwrap_or(f64::from(*f)))
            } else {
                encode_float(f64::from(*f))
            }
        }
        PbValue::String(s) => Value::String(s.clone()),
        PbValue::Bytes(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
        PbValue::EnumNumber(n) => match kind {
            Kind::Enum(e) => match e.get_value(*n) {
                Some(v) => Value::String(v.name().to_string()),
                None => Value::from(*n),
            },
            _ => Value::from(*n),
        },
        PbValue::Message(m) => encode_message(m),
        PbValue::List(_) | PbValue::Map(_) => Value::Null,
    }
}

fn encode_float(f: f64) -> Value {
    if f.is_nan() {
        Value::String("NaN".to_string())
    } else if f == f64::INFINITY {
        Value::String("Infinity".to_string())
    } else if f == f64::NEG_INFINITY {
        Value::String("-Infinity".to_string())
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

/// `PolicyDescriptor.rules` inlined as a JSON object (§10.11(3)); text that
/// is not a JSON object stays the string form.
fn encode_rules(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(parsed @ Value::Object(_)) => parsed,
        _ => Value::String(text.to_string()),
    }
}

/// The Envelope's `timestamp` (§10.11(1)) and `payload`/`payload_b64`
/// (§10.11(2), §10.12).
fn encode_envelope_transforms(message: &DynamicMessage, obj: &mut Map<String, Value>) {
    let ms = message
        .get_field_by_name("timestamp_unix_ms")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    obj.insert(
        "timestamp".to_string(),
        Value::String(format_rfc3339_ms(ms)),
    );

    let text = |name: &str| {
        message
            .get_field_by_name(name)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let bytes = message
        .get_field_by_name("payload")
        .and_then(|v| v.as_bytes().cloned())
        .unwrap_or_default();
    match lossless_payload(&text("mode"), &text("message_type"), &bytes) {
        Some(json) => obj.insert("payload".to_string(), json),
        None => obj.insert(
            "payload_b64".to_string(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        ),
    };
}

/// The payload as JSON when `(mode, message_type)` has a schema and the bytes
/// are exactly the canonical bytes of what they decode to, so converting the
/// JSON back reproduces them (lossless by construction: unknown fields, a
/// non-canonical encoding or undecodable bytes all fall back to
/// `payload_b64`).
fn lossless_payload(mode: &str, message_type: &str, bytes: &[u8]) -> Option<Value> {
    let desc = payload_descriptor(mode, message_type)?;
    let decoded = DynamicMessage::decode(desc, bytes).ok()?;
    if decoded.unknown_fields().next().is_some() || canonical_bytes(&decoded) != bytes {
        return None;
    }
    Some(encode_message(&decoded))
}

/// RFC 3339 in UTC (`Z`), with milliseconds only when non-zero
/// (`2026-03-01T20:24:01Z`, `2026-03-01T20:24:01.020Z`). A value outside the
/// years 0000-9999, which RFC 3339 cannot write, is clamped to that range.
pub(crate) fn format_rfc3339_ms(ms: i64) -> String {
    // 0000-01-01T00:00:00Z and 9999-12-31T23:59:59.999Z.
    const MIN_MS: i64 = -62_167_219_200_000;
    const MAX_MS: i64 = 253_402_300_799_999;
    let clamped = ms.clamp(MIN_MS, MAX_MS);
    chrono::DateTime::from_timestamp_millis(clamped)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
        .unwrap_or_default()
}
