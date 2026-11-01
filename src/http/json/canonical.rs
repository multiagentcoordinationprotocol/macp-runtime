//! Deterministic protobuf bytes of a message (RFC-MACP-0001 §10.13).
//!
//! Two conforming receivers must produce the same bytes from the same JSON
//! payload, and the RFC does not trust protobuf libraries' deterministic
//! modes to agree, so the bytes are written here, field by field:
//!
//! 1. fields in ascending field-number order;
//! 2. a proto3 default is omitted unless the field has explicit presence and
//!    is set; `-0.0` is not the default and is emitted; elements of repeated
//!    fields and map entries are never omitted;
//! 3. unrecognized members never reach the bytes (a decoded message's unknown
//!    fields are not written);
//! 4. repeated scalar numeric fields are packed;
//! 5. map entries in ascending order of the key's UTF-8 bytes, each always
//!    writing its key (field 1) then its value (field 2), even when default;
//! 6. NaN is the canonical quiet NaN (`0x7FF8000000000000`, `0x7FC00000`).
//!
//! Rule 7 (decode before encoding, so equivalent JSON is identical) is the
//! decoder's. `prost` cannot be used: it skips default map keys and values,
//! and `DynamicMessage` maps are unordered.

use prost::encoding::{encode_key, encode_varint, WireType};
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, MapKey, ReflectMessage, Value};

const CANONICAL_NAN_F64: u64 = 0x7FF8_0000_0000_0000;
const CANONICAL_NAN_F32: u32 = 0x7FC0_0000;

/// The §10.13 bytes of `message`.
pub fn canonical_bytes(message: &DynamicMessage) -> Vec<u8> {
    let mut buf = Vec::new();
    write_message(message, &mut buf);
    buf
}

fn write_message(message: &DynamicMessage, buf: &mut Vec<u8>) {
    let mut fields: Vec<FieldDescriptor> = message.descriptor().fields().collect();
    fields.sort_by_key(FieldDescriptor::number);
    for field in fields {
        let value = message.get_field(&field);
        // `has_field` applies rule 2, but compares with `==`, so it takes a
        // stored `-0.0` for the default; rule 2 says `-0.0` is emitted.
        if message.has_field(&field) || is_negative_zero(&value) {
            write_field(&field, &value, buf);
        }
    }
}

pub(crate) fn is_negative_zero(value: &Value) -> bool {
    match value {
        Value::F64(f) => *f == 0.0 && f.is_sign_negative(),
        Value::F32(f) => *f == 0.0 && f.is_sign_negative(),
        _ => false,
    }
}

fn write_field(field: &FieldDescriptor, value: &Value, buf: &mut Vec<u8>) {
    let number = field.number();
    let kind = field.kind();
    match value {
        Value::Map(map) => {
            let Kind::Message(entry) = &kind else { return };
            let key_kind = entry.map_entry_key_field().kind();
            let value_kind = entry.map_entry_value_field().kind();
            let mut entries: Vec<(&MapKey, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| compare_keys(a.0, b.0));
            for (k, v) in entries {
                let mut body = Vec::new();
                write_single(1, &key_kind, &map_key_value(k), &mut body);
                write_single(2, &value_kind, v, &mut body);
                write_len_delimited(number, &body, buf);
            }
        }
        Value::List(items) => {
            if items.is_empty() {
                return;
            }
            if is_packable(&kind) {
                let mut body = Vec::new();
                for item in items {
                    write_scalar_raw(&kind, item, &mut body);
                }
                write_len_delimited(number, &body, buf);
            } else {
                for item in items {
                    write_single(number, &kind, item, buf);
                }
            }
        }
        // Rule 2 was applied by `write_message`.
        single => write_single(number, &kind, single, buf),
    }
}

/// Map keys in ascending order of their UTF-8 bytes. MACP has only string
/// keys today; integer and bool keys (which have no UTF-8 form in the wire
/// encoding) order numerically.
fn compare_keys(a: &MapKey, b: &MapKey) -> std::cmp::Ordering {
    match (a, b) {
        (MapKey::String(x), MapKey::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => a.cmp(b),
    }
}

fn map_key_value(key: &MapKey) -> Value {
    match key {
        MapKey::Bool(b) => Value::Bool(*b),
        MapKey::I32(n) => Value::I32(*n),
        MapKey::I64(n) => Value::I64(*n),
        MapKey::U32(n) => Value::U32(*n),
        MapKey::U64(n) => Value::U64(*n),
        MapKey::String(s) => Value::String(s.clone()),
    }
}

fn is_packable(kind: &Kind) -> bool {
    !matches!(kind, Kind::String | Kind::Bytes | Kind::Message(_))
}

fn wire_type(kind: &Kind) -> WireType {
    match kind {
        Kind::Double | Kind::Fixed64 | Kind::Sfixed64 => WireType::SixtyFourBit,
        Kind::Float | Kind::Fixed32 | Kind::Sfixed32 => WireType::ThirtyTwoBit,
        Kind::String | Kind::Bytes | Kind::Message(_) => WireType::LengthDelimited,
        _ => WireType::Varint,
    }
}

/// One tagged value, written whatever its value (rule 2 is applied by the
/// caller for singular fields, and does not apply to elements and entries).
fn write_single(number: u32, kind: &Kind, value: &Value, buf: &mut Vec<u8>) {
    match (kind, value) {
        (Kind::Message(_), Value::Message(m)) => {
            let mut body = Vec::new();
            write_message(m, &mut body);
            write_len_delimited(number, &body, buf);
        }
        (Kind::String, Value::String(s)) => write_len_delimited(number, s.as_bytes(), buf),
        (Kind::Bytes, Value::Bytes(b)) => write_len_delimited(number, b, buf),
        _ => {
            encode_key(number, wire_type(kind), buf);
            write_scalar_raw(kind, value, buf);
        }
    }
}

fn write_len_delimited(number: u32, body: &[u8], buf: &mut Vec<u8>) {
    encode_key(number, WireType::LengthDelimited, buf);
    encode_varint(body.len() as u64, buf);
    buf.extend_from_slice(body);
}

/// A scalar's untagged wire form, as proto3 encodes it.
fn write_scalar_raw(kind: &Kind, value: &Value, buf: &mut Vec<u8>) {
    match (kind, value) {
        (Kind::Double, Value::F64(f)) => {
            let bits = if f.is_nan() {
                CANONICAL_NAN_F64
            } else {
                f.to_bits()
            };
            buf.extend_from_slice(&bits.to_le_bytes());
        }
        (Kind::Float, Value::F32(f)) => {
            let bits = if f.is_nan() {
                CANONICAL_NAN_F32
            } else {
                f.to_bits()
            };
            buf.extend_from_slice(&bits.to_le_bytes());
        }
        // int32 and enums are sign-extended to 64 bits, as proto3 does.
        (Kind::Int32, Value::I32(n)) => encode_varint(i64::from(*n) as u64, buf),
        (Kind::Enum(_), Value::EnumNumber(n)) => encode_varint(i64::from(*n) as u64, buf),
        (Kind::Int64, Value::I64(n)) => encode_varint(*n as u64, buf),
        (Kind::Uint32, Value::U32(n)) => encode_varint(u64::from(*n), buf),
        (Kind::Uint64, Value::U64(n)) => encode_varint(*n, buf),
        (Kind::Sint32, Value::I32(n)) => {
            encode_varint(u64::from(((n << 1) ^ (n >> 31)) as u32), buf)
        }
        (Kind::Sint64, Value::I64(n)) => encode_varint(((n << 1) ^ (n >> 63)) as u64, buf),
        (Kind::Fixed32, Value::U32(n)) => buf.extend_from_slice(&n.to_le_bytes()),
        (Kind::Sfixed32, Value::I32(n)) => buf.extend_from_slice(&n.to_le_bytes()),
        (Kind::Fixed64, Value::U64(n)) => buf.extend_from_slice(&n.to_le_bytes()),
        (Kind::Sfixed64, Value::I64(n)) => buf.extend_from_slice(&n.to_le_bytes()),
        (Kind::Bool, Value::Bool(b)) => encode_varint(u64::from(*b), buf),
        // `DynamicMessage::set_field` only stores values valid for the field's
        // kind, so no other pairing reaches here.
        _ => {}
    }
}
