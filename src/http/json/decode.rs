//! JSON to `DynamicMessage` (RFC-MACP-0001 §10.3-§10.12).
//!
//! Keys match the original `.proto` field name only: a lowerCamelCase
//! spelling is an unrecognized member and is ignored like any other
//! (§10.8(1), §10.6). `null` is the same as absent (§10.10). Numbers follow
//! the proto3 JSON mapping, which §10.8 leaves unchanged apart from the 64-bit
//! rules of §10.9; see `integer` and `float` below.

use std::collections::{BTreeMap, HashMap};

use base64::Engine as _;
use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MapKey, MessageDescriptor, ReflectMessage,
    Value as PbValue,
};
use serde_json::Value;

use super::canonical::{canonical_bytes, is_negative_zero};
use super::payload::payload_descriptor;
use super::pool::{is_envelope, is_policy_descriptor};
use super::JsonError;

/// Messages nest at most this deep (none of MACP's messages is recursive
/// today; the cap keeps a future recursive one from being a stack hazard).
pub(crate) const MAX_DEPTH: usize = 64;

/// 2^53, the largest magnitude at which every integer is an exact `f64`.
const F64_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// Decode the JSON form of the message `desc` from a `serde_json` value.
///
/// A `serde_json::Value` has already lost the text of any number that is not
/// an integer literal, so for an integer field such a number is accepted only
/// when integral and below 2^53 in magnitude. Decode request bodies with
/// `decode_message_text`, which keeps every number's source text and so never
/// rounds (§10.9).
pub fn decode_message(
    desc: &MessageDescriptor,
    value: &Value,
) -> Result<DynamicMessage, JsonError> {
    decode_object(desc, &Json::from(value), "", 0)
}

/// Decode the JSON text `body` (UTF-8) as the message `desc`. Every number
/// keeps its source text, so an integer field refuses any non-integral
/// literal however close to an integer (`1.00000000000000001`), as the proto3
/// JSON mapping does (§10.8, §10.9).
pub fn decode_message_text(
    desc: &MessageDescriptor,
    body: &[u8],
) -> Result<DynamicMessage, JsonError> {
    let json = parse_json(body)
        .map_err(|e| JsonError::InvalidRequest(format!("request body is not valid JSON: {e}")))?;
    decode_object(desc, &json, "", 0)
}

fn invalid(path: &str, what: impl std::fmt::Display) -> JsonError {
    if path.is_empty() {
        JsonError::InvalidRequest(what.to_string())
    } else {
        JsonError::InvalidRequest(format!("{path}: {what}"))
    }
}

fn child(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

fn decode_object(
    desc: &MessageDescriptor,
    value: &Json,
    path: &str,
    depth: usize,
) -> Result<DynamicMessage, JsonError> {
    if depth >= MAX_DEPTH {
        return Err(invalid(
            path,
            format!("messages nest deeper than {MAX_DEPTH}"),
        ));
    }
    let Json::Object(obj) = value else {
        return Err(invalid(
            path,
            format!("expected a JSON object for {}", desc.full_name()),
        ));
    };
    if is_envelope(desc) {
        return decode_envelope(desc, obj, path, depth);
    }
    let mut message = DynamicMessage::new(desc.clone());
    decode_fields(&mut message, obj, path, depth, &[])?;
    Ok(message)
}

/// Decode every recognized member of `obj` into `message`. Keys in `skip` are
/// handled by the caller (the Envelope's transformed fields).
fn decode_fields(
    message: &mut DynamicMessage,
    obj: &BTreeMap<String, Json>,
    path: &str,
    depth: usize,
    skip: &[&str],
) -> Result<(), JsonError> {
    let desc = message.descriptor();
    let mut oneofs_seen: HashMap<String, String> = HashMap::new();
    for (key, value) in obj {
        if skip.contains(&key.as_str()) || value.is_null() {
            continue;
        }
        // Original names only: `get_field_by_name`, never the json_name.
        let Some(field) = desc.get_field_by_name(key) else {
            continue;
        };
        let field_path = child(path, key);
        if let Some(oneof) = field.containing_oneof().filter(|o| !o.is_synthetic()) {
            if let Some(other) = oneofs_seen.insert(oneof.name().to_string(), key.clone()) {
                return Err(invalid(
                    path,
                    format!(
                        "members {other} and {key} of oneof {} are both set",
                        oneof.name()
                    ),
                ));
            }
        }
        let decoded = if is_policy_descriptor(&desc) && key == "rules" {
            Some(PbValue::String(decode_rules(value, &field_path)?))
        } else {
            decode_field(&field, value, &field_path, depth)?
        };
        // `None`: an unknown enum name on a singular field leaves it unset
        // (§10.4).
        if let Some(decoded) = decoded {
            store(message, &field, decoded);
        }
    }
    Ok(())
}

/// Set `field`, except that a field without presence at its default is
/// cleared rather than stored, so a decoded message equals the same message
/// decoded from protobuf bytes (`-0.0` compares equal to the default but is
/// not one, §10.13(2)).
fn store(message: &mut DynamicMessage, field: &FieldDescriptor, value: PbValue) {
    if field.supports_presence() || !value.is_default_for_field(field) || is_negative_zero(&value) {
        message.set_field(field, value);
    } else {
        message.clear_field(field);
    }
}

fn store_by_name(message: &mut DynamicMessage, name: &str, value: PbValue) {
    if let Some(field) = message.descriptor().get_field_by_name(name) {
        store(message, &field, value);
    }
}

/// `Ok(None)` is an unknown enum name (§10.4): a singular field stays unset,
/// a repeated element or map entry is dropped.
fn decode_field(
    field: &FieldDescriptor,
    value: &Json,
    path: &str,
    depth: usize,
) -> Result<Option<PbValue>, JsonError> {
    if field.is_map() {
        let Json::Object(obj) = value else {
            return Err(invalid(path, "expected a JSON object for a map field"));
        };
        let entry = match field.kind() {
            Kind::Message(m) => m,
            _ => return Err(invalid(path, "map field without an entry message")),
        };
        let key_field = entry.map_entry_key_field();
        let value_field = entry.map_entry_value_field();
        let mut map = HashMap::with_capacity(obj.len());
        for (k, v) in obj {
            let entry_path = child(path, &shown(k));
            if v.is_null() {
                return Err(invalid(&entry_path, "null is not a valid map value"));
            }
            let key = decode_map_key(&key_field.kind(), k, &entry_path)?;
            // A map value with an unknown enum name is dropped like a
            // repeated element (§10.4, §10.6).
            if let Some(val) = decode_single(&value_field.kind(), v, &entry_path, depth)? {
                map.insert(key, val);
            }
        }
        Ok(Some(PbValue::Map(map)))
    } else if field.is_list() {
        let Json::Array(items) = value else {
            return Err(invalid(path, "expected a JSON array for a repeated field"));
        };
        let mut list = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let item_path = format!("{path}[{i}]");
            if item.is_null() {
                return Err(invalid(&item_path, "null is not a valid repeated element"));
            }
            // An unknown enum name inside a repeated field drops the element
            // (§10.4).
            if let Some(v) = decode_single(&field.kind(), item, &item_path, depth)? {
                list.push(v);
            }
        }
        Ok(Some(PbValue::List(list)))
    } else {
        decode_single(&field.kind(), value, path, depth)
    }
}

fn decode_single(
    kind: &Kind,
    value: &Json,
    path: &str,
    depth: usize,
) -> Result<Option<PbValue>, JsonError> {
    Ok(Some(match kind {
        Kind::Double => PbValue::F64(float(value, false).map_err(|e| invalid(path, e))?),
        Kind::Float => PbValue::F32(float(value, true).map_err(|e| invalid(path, e))? as f32),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => PbValue::I32(
            integer(value, i32::MIN as i128, i32::MAX as i128).map_err(|e| invalid(path, e))?
                as i32,
        ),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => PbValue::I64(
            integer(value, i64::MIN as i128, i64::MAX as i128).map_err(|e| invalid(path, e))?
                as i64,
        ),
        Kind::Uint32 | Kind::Fixed32 => {
            PbValue::U32(integer(value, 0, u32::MAX as i128).map_err(|e| invalid(path, e))? as u32)
        }
        Kind::Uint64 | Kind::Fixed64 => {
            PbValue::U64(integer(value, 0, u64::MAX as i128).map_err(|e| invalid(path, e))? as u64)
        }
        Kind::Bool => match value {
            Json::Bool(b) => PbValue::Bool(*b),
            _ => return Err(invalid(path, "expected true or false")),
        },
        Kind::String => match value {
            Json::String(s) => PbValue::String(s.clone()),
            _ => return Err(invalid(path, "expected a JSON string")),
        },
        Kind::Bytes => match value {
            Json::String(s) => {
                PbValue::Bytes(decode_base64(s).map_err(|e| invalid(path, e))?.into())
            }
            _ => return Err(invalid(path, "expected a base64 JSON string")),
        },
        Kind::Enum(e) => match value {
            Json::String(name) => match e.get_value_by_name(name) {
                Some(v) => PbValue::EnumNumber(v.number()),
                None => return Ok(None),
            },
            // An open proto3 enum keeps any int32, named or not.
            Json::Number(_) => PbValue::EnumNumber(
                integer(value, i32::MIN as i128, i32::MAX as i128).map_err(|e| invalid(path, e))?
                    as i32,
            ),
            _ => return Err(invalid(path, "expected an enum name or number")),
        },
        Kind::Message(m) => PbValue::Message(decode_object(m, value, path, depth + 1)?),
    }))
}

fn decode_map_key(kind: &Kind, key: &str, path: &str) -> Result<MapKey, JsonError> {
    let as_int = |lo: i128, hi: i128| {
        parse_decimal_integer(key)
            .and_then(|n| in_range(n, lo, hi))
            .map_err(|e| invalid(path, format!("map key: {e}")))
    };
    Ok(match kind {
        Kind::String => MapKey::String(key.to_string()),
        Kind::Bool => match key {
            "true" => MapKey::Bool(true),
            "false" => MapKey::Bool(false),
            _ => return Err(invalid(path, "map key: expected \"true\" or \"false\"")),
        },
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            MapKey::I32(as_int(i32::MIN as i128, i32::MAX as i128)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            MapKey::I64(as_int(i64::MIN as i128, i64::MAX as i128)? as i64)
        }
        Kind::Uint32 | Kind::Fixed32 => MapKey::U32(as_int(0, u32::MAX as i128)? as u32),
        Kind::Uint64 | Kind::Fixed64 => MapKey::U64(as_int(0, u64::MAX as i128)? as u64),
        _ => return Err(invalid(path, "unsupported map key type")),
    })
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

/// An integer field's JSON value: a JSON number, or a quoted decimal string
/// (§10.9), in plain or exponent notation, accepted only when the value is
/// integral and within `[lo, hi]`. Never rounded: number text is parsed
/// exactly, so a non-integral literal is refused however close it is to an
/// integer. A number that reached the codec as an `f64` (a `serde_json`
/// value) has lost its text, so it is accepted only when integral and below
/// 2^53 in magnitude, where every integer is exact.
pub(crate) fn integer(value: &Json, lo: i128, hi: i128) -> Result<i128, String> {
    let n = match value {
        Json::Number(Num::Text(t)) | Json::String(t) => parse_decimal_integer(t)?,
        Json::Number(Num::Float(f)) => {
            if !f.is_finite() || f.fract() != 0.0 {
                return Err(format!("{} is not an integer", shown(&f.to_string())));
            }
            if f.abs() >= F64_EXACT_INT {
                return Err(format!(
                    "{f} cannot be represented exactly; quote it as a decimal string"
                ));
            }
            *f as i128
        }
        _ => return Err("expected a JSON number or a quoted decimal string".to_string()),
    };
    in_range(n, lo, hi)
}

fn in_range(n: i128, lo: i128, hi: i128) -> Result<i128, String> {
    if n < lo || n > hi {
        Err(format!("{n} is out of range [{lo}, {hi}]"))
    } else {
        Ok(n)
    }
}

/// Exact parse of a JSON-number-shaped decimal string (`"900000"`, `"-12"`,
/// `"1.5e2"`, `"1e3"`) to an integer; a non-integral value is an error.
pub(crate) fn parse_decimal_integer(s: &str) -> Result<i128, String> {
    let bad = || format!("{} is not a decimal integer", shown(s));
    if !is_json_number(s) {
        return Err(bad());
    }
    let (negative, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (mantissa, exponent) = match rest.find(['e', 'E']) {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, "0"),
    };
    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    let mut digits: String = format!("{int_part}{frac_part}")
        .trim_start_matches('0')
        .to_string();
    if digits.is_empty() {
        return Ok(0);
    }
    // Exponents beyond a few hundred cannot yield an in-range integer.
    let exponent: i64 = exponent
        .trim_start_matches('+')
        .parse::<i64>()
        .ok()
        .filter(|e| e.abs() <= 1000)
        .ok_or_else(|| format!("{} is out of range", shown(s)))?;
    let mut scale = exponent - frac_part.len() as i64;
    while digits.ends_with('0') {
        digits.pop();
        scale += 1;
    }
    if scale < 0 {
        return Err(format!("{} is not an integer", shown(s)));
    }
    if digits.len() as i64 + scale > 39 {
        return Err(format!("{} is out of range", shown(s)));
    }
    let mut n: i128 = digits
        .parse()
        .map_err(|_| format!("{} is out of range", shown(s)))?;
    for _ in 0..scale {
        n = n
            .checked_mul(10)
            .ok_or_else(|| format!("{} is out of range", shown(s)))?;
    }
    Ok(if negative { -n } else { n })
}

/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`, the JSON number grammar.
pub(crate) fn is_json_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'-') {
        i += 1;
    }
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(c) if c.is_ascii_digit() => {
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

/// A `float`/`double` field's JSON value: a JSON number, a quoted number, or
/// one of `"NaN"`, `"Infinity"`, `"-Infinity"` (§10.8). A finite value beyond
/// the type's range is an error.
pub(crate) fn float(value: &Json, single: bool) -> Result<f64, String> {
    let f = match value {
        Json::Number(Num::Float(f)) => *f,
        Json::Number(Num::Text(t)) => parse_float_text(t)?,
        Json::String(s) => match s.as_str() {
            "NaN" => return Ok(f64::NAN),
            "Infinity" => return Ok(f64::INFINITY),
            "-Infinity" => return Ok(f64::NEG_INFINITY),
            s if is_json_number(s) => parse_float_text(s)?,
            s => return Err(format!("{} is not a number", shown(s))),
        },
        _ => return Err("expected a JSON number or a quoted number".to_string()),
    };
    if !f.is_finite() || (single && f.abs() > f32::MAX as f64) {
        return Err(format!("{} is out of range", shown(&f.to_string())));
    }
    Ok(f)
}

fn parse_float_text(s: &str) -> Result<f64, String> {
    s.parse::<f64>().map_err(|e| format!("{}: {e}", shown(s)))
}

/// `s` for an error message, cut to 64 characters so a huge input does not
/// produce a huge reason.
pub(crate) fn shown(s: &str) -> String {
    const MAX: usize = 64;
    match s.char_indices().nth(MAX) {
        Some((cut, _)) => format!("{:?}…", &s[..cut]),
        None => format!("{s:?}"),
    }
}

/// Standard or URL-safe base64, padded or not (§10.3).
pub(crate) fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    use base64::engine::general_purpose::{
        STANDARD_NO_PAD_INDIFFERENT, URL_SAFE_NO_PAD_INDIFFERENT,
    };
    STANDARD_NO_PAD_INDIFFERENT
        .decode(s)
        .or_else(|_| URL_SAFE_NO_PAD_INDIFFERENT.decode(s))
        .map_err(|e| format!("not valid base64: {e}"))
}

// ---------------------------------------------------------------------------
// Structural transforms (§10.11)
// ---------------------------------------------------------------------------

/// `PolicyDescriptor.rules`: the inlined JSON object, or the string form
/// holding its JSON text (§10.11(3)). Both become the same compact JSON text,
/// so the two forms decode to the same descriptor. A string that is not JSON
/// is kept verbatim (the policy registry judges it).
fn decode_rules(value: &Json, path: &str) -> Result<String, JsonError> {
    match value {
        Json::Object(_) => Ok(value.to_value().map_err(|e| invalid(path, e))?.to_string()),
        Json::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(parsed @ Value::Object(_)) => Ok(parsed.to_string()),
            _ => Ok(s.clone()),
        },
        _ => Err(invalid(
            path,
            "rules must be a JSON object or its JSON text as a string",
        )),
    }
}

/// The Envelope (§10.1, §10.2, §10.11, §10.12). Every plain field decodes as
/// usual; `timestamp` replaces `timestamp_unix_ms` (whose own name is not a
/// JSON member), and exactly one of `payload`/`payload_b64` carries the
/// payload. Failures in those two transforms are Envelope-level decisions,
/// reported in band as `INVALID_ENVELOPE`.
fn decode_envelope(
    desc: &MessageDescriptor,
    obj: &BTreeMap<String, Json>,
    path: &str,
    depth: usize,
) -> Result<DynamicMessage, JsonError> {
    let mut message = DynamicMessage::new(desc.clone());
    decode_fields(
        &mut message,
        obj,
        path,
        depth,
        &["timestamp", "timestamp_unix_ms", "payload", "payload_b64"],
    )?;
    let text = |name: &str| {
        message
            .get_field_by_name(name)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let (mode, message_type) = (text("mode"), text("message_type"));
    let (session_id, message_id) = (text("session_id"), text("message_id"));
    let in_band = |reason: String| JsonError::InvalidEnvelope {
        reason,
        session_id: session_id.clone(),
        message_id: message_id.clone(),
    };

    match obj.get("timestamp").filter(|v| !v.is_null()) {
        None => {}
        Some(Json::String(s)) => {
            let ms = parse_rfc3339_ms(s).map_err(|e| in_band(format!("timestamp: {e}")))?;
            store_by_name(&mut message, "timestamp_unix_ms", PbValue::I64(ms));
        }
        Some(_) => {
            return Err(in_band(
                "timestamp: expected an RFC 3339 string".to_string(),
            ))
        }
    }

    let json_payload = obj.get("payload").filter(|v| !v.is_null());
    let b64_payload = obj.get("payload_b64").filter(|v| !v.is_null());
    let bytes = match (json_payload, b64_payload) {
        (Some(_), Some(_)) => {
            return Err(in_band(
                "an Envelope carries exactly one of payload and payload_b64, not both".to_string(),
            ))
        }
        (None, None) => {
            return Err(in_band(
                "an Envelope carries exactly one of payload and payload_b64; neither is present"
                    .to_string(),
            ))
        }
        (None, Some(Json::String(s))) => {
            decode_base64(s).map_err(|e| in_band(format!("payload_b64: {e}")))?
        }
        (None, Some(_)) => {
            return Err(in_band("payload_b64: expected a base64 string".to_string()))
        }
        (Some(payload), None) => {
            let Some(payload_desc) = payload_descriptor(&mode, &message_type) else {
                return Err(in_band(format!(
                    "no payload schema for mode {} and message_type {}; send payload_b64",
                    shown(&mode),
                    shown(&message_type)
                )));
            };
            if !matches!(payload, Json::Object(_)) {
                return Err(in_band("payload: expected a JSON object".to_string()));
            }
            let decoded = decode_object(&payload_desc, payload, &child(path, "payload"), depth + 1)
                .map_err(|e| in_band(e.reason().to_string()))?;
            canonical_bytes(&decoded)
        }
    };
    store_by_name(&mut message, "payload", PbValue::Bytes(bytes.into()));
    Ok(message)
}

/// RFC 3339 with any offset and fractional seconds, to milliseconds since the
/// Unix epoch, truncating digits below the millisecond (§10.11(1)).
pub(crate) fn parse_rfc3339_ms(s: &str) -> Result<i64, String> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| format!("{} is not RFC 3339: {e}", shown(s)))?;
    let secs = dt.timestamp();
    let millis = i64::from(dt.timestamp_subsec_millis());
    secs.checked_mul(1000)
        .and_then(|ms| ms.checked_add(millis))
        .ok_or_else(|| format!("{} is out of range", shown(s)))
}

/// Test access to the depth cap, which no MACP message is deep enough to hit.
#[cfg(test)]
pub(crate) fn decode_message_at_depth(
    desc: &MessageDescriptor,
    value: &Value,
    depth: usize,
) -> Result<DynamicMessage, JsonError> {
    decode_object(desc, &Json::from(value), "", depth)
}

// ---------------------------------------------------------------------------
// JSON text with number text preserved
// ---------------------------------------------------------------------------

/// A parsed JSON value whose numbers keep their source text, so integrality
/// and range are judged on the digits, never on a rounded `f64`. Objects keep
/// the last of duplicate keys, as `serde_json` does.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(Num),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Num {
    /// A JSON number token exactly as written (or an integer's decimal form).
    Text(String),
    /// A number that reached the codec as an `f64` (from a `serde_json`
    /// value), whose text is gone.
    Float(f64),
}

impl Json {
    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// Back to a `serde_json` value (for `rules` text).
    pub(crate) fn to_value(&self) -> Result<Value, String> {
        Ok(match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Number(Num::Text(t)) => {
                serde_json::from_str::<Value>(t).map_err(|e| format!("{}: {e}", shown(t)))?
            }
            Json::Number(Num::Float(f)) => {
                serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number)
            }
            Json::String(s) => Value::String(s.clone()),
            Json::Array(a) => Value::Array(a.iter().map(Json::to_value).collect::<Result<_, _>>()?),
            Json::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, v)| Ok((k.clone(), v.to_value()?)))
                    .collect::<Result<_, String>>()?,
            ),
        })
    }
}

impl From<&Value> for Json {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Json::Null,
            Value::Bool(b) => Json::Bool(*b),
            Value::Number(n) => Json::Number(match (n.as_i64(), n.as_u64(), n.as_f64()) {
                (Some(i), _, _) => Num::Text(i.to_string()),
                (_, Some(u), _) => Num::Text(u.to_string()),
                (_, _, Some(f)) => Num::Float(f),
                _ => Num::Text(n.to_string()),
            }),
            Value::String(s) => Json::String(s.clone()),
            Value::Array(a) => Json::Array(a.iter().map(Json::from).collect()),
            Value::Object(o) => {
                Json::Object(o.iter().map(|(k, v)| (k.clone(), Json::from(v))).collect())
            }
        }
    }
}

/// JSON text nests at most this deep (the same bound as `serde_json`'s
/// recursion limit), so a hostile body cannot exhaust the stack.
const MAX_TEXT_DEPTH: usize = 128;

/// Parse RFC 8259 JSON text, keeping number tokens as text. Strings are
/// delegated to `serde_json` once their extent is found, so escapes and
/// surrogates follow it exactly.
pub(crate) fn parse_json(bytes: &[u8]) -> Result<Json, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
    let mut p = Parser { s: text, i: 0 };
    let value = p.value(0)?;
    p.ws();
    if p.i != text.len() {
        return Err(format!("trailing characters at byte {}", p.i));
    }
    Ok(value)
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn err(&self, what: &str) -> String {
        format!("{what} at byte {}", self.i)
    }

    fn expect(&mut self, b: u8) -> Result<(), String> {
        self.ws();
        if self.peek() == Some(b) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", b as char)))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth >= MAX_TEXT_DEPTH {
            return Err(self.err("JSON nests too deeply"));
        }
        self.ws();
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                let mut obj = BTreeMap::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(Json::Object(obj));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.expect(b':')?;
                    let value = self.value(depth + 1)?;
                    obj.insert(key, value);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Object(obj));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Json::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Array(items));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while matches!(
                    self.peek(),
                    Some(b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                ) {
                    self.i += 1;
                }
                let token = &self.s[start..self.i];
                if !is_json_number(token) {
                    self.i = start;
                    return Err(self.err("invalid number"));
                }
                Ok(Json::Number(Num::Text(token.to_string())))
            }
            _ => Err(self.err("expected a JSON value")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(value)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(self.err("expected a string"));
        }
        let start = self.i;
        let bytes = self.s.as_bytes();
        let mut j = start + 1;
        loop {
            match bytes.get(j) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => break,
                Some(b'\\') => j += 2,
                Some(_) => j += 1,
            }
        }
        let token = self
            .s
            .get(start..=j)
            .ok_or_else(|| self.err("unterminated string"))?;
        let parsed = serde_json::from_str::<String>(token).map_err(|e| self.err(&e.to_string()))?;
        self.i = j + 1;
        Ok(parsed)
    }
}
