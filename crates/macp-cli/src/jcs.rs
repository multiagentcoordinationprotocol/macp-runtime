//! RFC 8785 JSON Canonicalization Scheme (JCS), over `serde_json::Value`.
//!
//! `macp_core`'s commitment-hash canonicalizer covers only the Section 3
//! projection (strings, one boolean, one nested object). A receipt body
//! (RFC-MACP-0013 A.2) also holds integers and arrays, so it needs the rest:
//! members sorted by UTF-16 code units (§3.2.3), strings escaped as §3.2.2.2
//! says, and numbers in the ECMAScript form of §3.2.2.3.

use serde_json::Value;

/// A number JCS cannot represent (NaN or an infinity never parse from JSON,
/// but a caller-built `Value` may hold one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotCanonicalizable;

impl std::fmt::Display for NotCanonicalizable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("value has no RFC 8785 representation")
    }
}

impl std::error::Error for NotCanonicalizable {}

/// The canonical UTF-8 bytes of `value`.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>, NotCanonicalizable> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out.into_bytes())
}

fn write_value(value: &Value, out: &mut String) -> Result<(), NotCanonicalizable> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                // An integer beyond 2^53 is still printed exactly, as the
                // number a JSON parser holding it as an integer would emit.
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&es_number(n.as_f64().ok_or(NotCanonicalizable)?)?);
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(members) => {
            let mut members: Vec<_> = members.iter().collect();
            members.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, (key, item)) in members.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// ECMAScript `Number.prototype.toString` for a finite double: the shortest
/// round-tripping digits, laid out as ECMA-262 §6.1.6.1.20 says.
fn es_number(f: f64) -> Result<String, NotCanonicalizable> {
    if !f.is_finite() {
        return Err(NotCanonicalizable);
    }
    if f == 0.0 {
        return Ok("0".into());
    }
    let sign = if f < 0.0 { "-" } else { "" };
    // `{:e}` is the shortest round-trip form: "d[.ddd]e<exp>".
    let sci = format!("{:e}", f.abs());
    let (mantissa, exp) = sci.split_once('e').ok_or(NotCanonicalizable)?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().map_err(|_| NotCanonicalizable)? + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e = if e >= 0 {
            format!("+{e}")
        } else {
            e.to_string()
        };
        match digits.split_at(1) {
            (d, "") => format!("{d}e{e}"),
            (d, rest) => format!("{d}.{rest}e{e}"),
        }
    };
    Ok(format!("{sign}{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jcs(v: Value) -> String {
        String::from_utf8(canonicalize(&v).unwrap()).unwrap()
    }

    #[test]
    fn members_sort_by_utf16_code_units() {
        // RFC 8785 §3.2.3's example: U+20AC sorts before U+1F600 in UTF-8
        // order too, but U+FB33 sorts after U+1F600's surrogate pair in UTF-16.
        assert_eq!(
            jcs(json!({"\u{fb33}": 1, "\u{1f600}": 2, "a": 3, "\u{20ac}": 4})),
            "{\"a\":3,\"\u{20ac}\":4,\"\u{1f600}\":2,\"\u{fb33}\":1}"
        );
    }

    #[test]
    fn strings_escape_only_what_jcs_requires() {
        assert_eq!(
            jcs(json!("\u{0}\u{8}\t\n\u{c}\r\u{1f}\"\\/\u{7f}\u{2028}é")),
            "\"\\u0000\\b\\t\\n\\f\\r\\u001f\\\"\\\\/\u{7f}\u{2028}é\""
        );
    }

    #[test]
    fn numbers_follow_ecmascript() {
        // RFC 8785 Appendix B.
        for (f, want) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-1.5, "-1.5"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789.0e-12, "0.000123456789"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (333333333.3333333, "333333333.3333333"),
            (295147905179352830000.0, "295147905179352830000"),
        ] {
            assert_eq!(es_number(f).unwrap(), want, "{f:e}");
        }
        assert_eq!(jcs(json!([1, -2, 3.5, true, null])), "[1,-2,3.5,true,null]");
    }
}
