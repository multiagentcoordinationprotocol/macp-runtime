//! Unit tests, one group per §10 rule. The cross-cutting acceptance tests
//! (examples, prost equality, corpus fixpoint, payload table) are in
//! `tests/http_json.rs`.

use prost_reflect::{DynamicMessage, Value as PbValue};
use serde_json::{json, Value};

use super::decode::{
    decode_base64, decode_message_text, float, integer as integer_of, is_json_number,
    parse_decimal_integer, parse_json, parse_rfc3339_ms, shown, Json,
};
use super::encode::format_rfc3339_ms;
use super::pool::message;
use super::*;

fn decode(name: &str, v: Value) -> Result<DynamicMessage, JsonError> {
    decode_message(&message(name), &v)
}

/// `integer` on a `serde_json` value (number text already lost).
fn integer(v: &Value, lo: i128, hi: i128) -> Result<i128, String> {
    integer_of(&Json::from(v), lo, hi)
}

/// `integer` on raw JSON text (number text kept, as for a request body).
fn integer_text(text: &str, lo: i128, hi: i128) -> Result<i128, String> {
    integer_of(&parse_json(text.as_bytes()).unwrap(), lo, hi)
}

fn field(m: &DynamicMessage, name: &str) -> PbValue {
    m.get_field_by_name(name).unwrap().into_owned()
}

// --- numbers (§10.8, §10.9) -------------------------------------------------

#[test]
fn integers_accept_numbers_quoted_and_exponent_forms() {
    let (lo, hi) = (i64::MIN as i128, i64::MAX as i128);
    for v in [
        json!(1000),
        json!("1000"),
        json!(1e3),
        json!("1e3"),
        json!(1000.0),
        json!("1.5e3"),
        json!("10000e-1"),
    ] {
        let expected = if v == json!("1.5e3") { 1500 } else { 1000 };
        assert_eq!(integer(&v, lo, hi), Ok(expected), "{v}");
    }
    assert_eq!(integer(&json!("-0"), lo, hi), Ok(0));
    assert_eq!(
        integer(&json!("9223372036854775807"), lo, hi),
        Ok(i64::MAX as i128)
    );
    assert_eq!(
        integer(&json!(u64::MAX), 0, u64::MAX as i128),
        Ok(u64::MAX as i128)
    );
}

#[test]
fn integers_reject_fractions_out_of_range_and_inexact_values() {
    let (lo, hi) = (i32::MIN as i128, i32::MAX as i128);
    for v in [
        json!(1.5),
        json!("1.5"),
        json!("2.5e0"),
        json!("1e-1"),
        json!(2147483648_i64),
        json!("2147483648"),
        json!(" 1"),
        json!("0x10"),
        json!("+1"),
        json!(""),
        json!(true),
        json!([1]),
    ] {
        assert!(integer(&v, lo, hi).is_err(), "{v} must be refused");
    }
    // An f64 above 2^53 may already have been rounded by the parser.
    assert!(integer(&json!(1.0e17), i64::MIN as i128, i64::MAX as i128).is_err());
    // ...but the same value as an integer literal or quoted is exact.
    assert_eq!(
        integer(
            &json!(100000000000000000_i64),
            i64::MIN as i128,
            i64::MAX as i128
        ),
        Ok(100000000000000000)
    );
    assert_eq!(
        integer(&json!("1e17"), i64::MIN as i128, i64::MAX as i128),
        Ok(100000000000000000)
    );
    assert!(parse_decimal_integer("1e1001").is_err());
    assert!(parse_decimal_integer("1e400").is_err());
    assert_eq!(parse_decimal_integer("0e999999"), Ok(0));
}

#[test]
fn integers_are_judged_on_the_number_text_never_a_rounded_f64() {
    for (lo, hi) in [
        (i32::MIN as i128, i32::MAX as i128),
        (i64::MIN as i128, i64::MAX as i128),
    ] {
        for bad in [
            "9007199254740992.5",
            "1.00000000000000001",
            "\"1.00000000000000001\"",
            "-1.00000000000000001",
            "1e-400",
        ] {
            assert!(integer_text(bad, lo, hi).is_err(), "{bad} must be refused");
        }
        for ok in ["1e3", "\"1e3\"", "1000.0", "1000", "\"1000\"", "10000e-1"] {
            assert_eq!(integer_text(ok, lo, hi), Ok(1000), "{ok}");
        }
    }
    // Exact above 2^53 from text, in every notation.
    let (lo, hi) = (i64::MIN as i128, i64::MAX as i128);
    assert_eq!(
        integer_text("9007199254740993", lo, hi),
        Ok(9007199254740993)
    );
    assert_eq!(
        integer_text("9007199254740993.0", lo, hi),
        Ok(9007199254740993)
    );
    assert_eq!(integer_text("1e17", lo, hi), Ok(100000000000000000));
    // From a `serde_json` f64 the text is gone: 2^53 itself is refused (`>=`).
    assert!(integer(&json!(9007199254740992.0), lo, hi).is_err());
    assert_eq!(
        integer(&json!(9007199254740991.0), lo, hi),
        Ok(9007199254740991)
    );
}

#[test]
fn request_bodies_keep_number_text_end_to_end() {
    let desc = message("macp.v1.SessionStartPayload");
    for bad in [
        r#"{"ttl_ms": 1.00000000000000001}"#,
        r#"{"ttl_ms": 9007199254740992.5}"#,
        r#"{"ttl_ms": "1.00000000000000001"}"#,
    ] {
        let e = decode_message_text(&desc, bad.as_bytes()).unwrap_err();
        assert_eq!(e.code(), "INVALID_REQUEST", "{bad}");
    }
    for ok in [
        r#"{"ttl_ms": 1e3}"#,
        r#"{"ttl_ms": "1e3"}"#,
        r#"{"ttl_ms": 1000.0}"#,
    ] {
        let m = decode_message_text(&desc, ok.as_bytes()).unwrap();
        assert_eq!(field(&m, "ttl_ms"), PbValue::I64(1000), "{ok}");
    }
    // The text parser agrees with serde_json on structure and duplicates.
    let text = r#" {"a": [1, -2.5e3, true, false, null, "\u00e9\n"], "a2": {}, "k": 1, "k": 2} "#;
    let ours = parse_json(text.as_bytes()).unwrap();
    let theirs: Value = serde_json::from_str(text).unwrap();
    assert_eq!(ours.to_value().unwrap(), theirs);
    for bad in [
        "",
        "{",
        "[1,]",
        "{\"a\" 1}",
        "01",
        "1 2",
        "\"\\x\"",
        "tru",
        "{\"a\":1,}",
    ] {
        assert!(parse_json(bad.as_bytes()).is_err(), "{bad:?}");
        assert!(
            serde_json::from_str::<Value>(bad).is_err(),
            "{bad:?} oracle"
        );
    }
    let deep = "[".repeat(200) + &"]".repeat(200);
    assert!(parse_json(deep.as_bytes()).is_err(), "depth cap");
}

#[test]
fn echoed_input_is_truncated_in_reasons() {
    let long = "9".repeat(500) + ".5";
    let reason = integer_text(&format!("\"{long}\""), 0, 10).unwrap_err();
    assert!(reason.len() < 120, "{reason}");
    assert!(reason.contains('…'));
    assert_eq!(shown("abc"), "\"abc\"");
}

#[test]
fn json_number_grammar() {
    for ok in ["0", "-0", "12", "1.5", "1e3", "1E+3", "-1.25e-2"] {
        assert!(is_json_number(ok), "{ok}");
    }
    for bad in ["", "-", "01", "1.", ".5", "1e", "+1", "NaN", "1_000", "1 "] {
        assert!(!is_json_number(bad), "{bad}");
    }
}

#[test]
fn floats_accept_quoted_and_special_values() {
    assert_eq!(float(&Json::from(&json!(2.5)), false), Ok(2.5));
    assert_eq!(float(&Json::from(&json!("2.5")), false), Ok(2.5));
    assert_eq!(float(&Json::from(&json!("2.5e0")), false), Ok(2.5));
    assert_eq!(float(&Json::from(&json!(3)), false), Ok(3.0));
    assert!(float(&Json::from(&json!("NaN")), false).unwrap().is_nan());
    assert_eq!(
        float(&Json::from(&json!("Infinity")), false),
        Ok(f64::INFINITY)
    );
    assert_eq!(
        float(&Json::from(&json!("-Infinity")), false),
        Ok(f64::NEG_INFINITY)
    );
    for bad in [
        json!("nan"),
        json!("inf"),
        json!("1e400"),
        json!(true),
        json!({}),
    ] {
        assert!(float(&Json::from(&bad), false).is_err(), "{bad}");
    }
    assert!(
        float(&Json::from(&json!(1e39)), true).is_err(),
        "beyond f32 range"
    );
}

// --- bytes (§10.3) ------------------------------------------------------------

#[test]
fn bytes_accept_both_alphabets_padded_or_not() {
    let raw = vec![0xfb, 0xff, 0xfe];
    for s in ["+//+", "-__-"] {
        assert_eq!(decode_base64(s).unwrap(), raw, "{s}");
    }
    assert_eq!(decode_base64("AAECAwQ=").unwrap(), vec![0, 1, 2, 3, 4]);
    assert_eq!(decode_base64("AAECAwQ").unwrap(), vec![0, 1, 2, 3, 4]);
    assert!(decode_base64("not base64!").is_err());
}

// --- names, unknown members, null (§10.6, §10.8(1), §10.10) -------------------

#[test]
fn camel_case_keys_are_unknown_members() {
    let m = decode("macp.v1.GetSessionRequest", json!({"sessionId": "s1"})).unwrap();
    assert_eq!(field(&m, "session_id"), PbValue::String(String::new()));
    let m = decode(
        "macp.v1.GetSessionRequest",
        json!({"session_id": "s1", "extra": 1}),
    )
    .unwrap();
    assert_eq!(field(&m, "session_id"), PbValue::String("s1".into()));
}

#[test]
fn null_is_absent() {
    let m = decode("macp.v1.Ack", json!({"message_id": null, "error": null})).unwrap();
    assert!(!m.has_field_by_name("error"));
    assert!(decode("macp.v1.ListModesResponse", json!({"modes": [null]})).is_err());
}

#[test]
fn wrong_json_type_per_scalar_kind_is_invalid_request() {
    let cases = [
        ("macp.v1.GetSessionRequest", json!({"session_id": 5})),
        ("macp.v1.Ack", json!({"ok": "true"})),
        ("macp.v1.Ack", json!({"accepted_at_unix_ms": true})),
        ("macp.v1.Ack", json!({"session_state": 1.5})),
        ("macp.v1.Ack", json!({"session_state": {}})),
        ("macp.v1.Ack", json!({"error": "x"})),
        ("macp.v1.ListSessionsRequest", json!({"page_size": "big"})),
        ("macp.v1.PolicyDescriptor", json!({"schema_version": -1})),
        ("macp.v1.SignalPayload", json!({"data": 7})),
        ("macp.v1.SignalPayload", json!({"confidence": "high"})),
        ("macp.v1.SessionStartPayload", json!({"participants": "a"})),
        ("macp.v1.SessionStartPayload", json!({"extensions": []})),
        (
            "macp.v1.SessionStartPayload",
            json!({"extensions": {"k": "%%"}}),
        ),
        ("macp.v1.PolicyDescriptor", json!({"rules": 3})),
        ("macp.v1.GetSessionRequest", json!([])),
    ];
    for (name, v) in cases {
        let err = decode(name, v.clone()).expect_err(&format!("{name} {v}"));
        assert_eq!(err.code(), "INVALID_REQUEST", "{name} {v}: {err}");
    }
}

#[test]
fn oneof_with_two_members_is_refused() {
    let v = json!({"envelope": {"payload_b64": ""}, "error": {"code": "X"}});
    assert!(decode("macp.v1.StreamSessionResponse", v).is_err());
}

#[test]
fn depth_is_capped() {
    use super::decode::{decode_message_at_depth, MAX_DEPTH};
    let ack = message("macp.v1.Ack");
    let nested = json!({"error": {"code": "X"}});
    assert!(decode_message_at_depth(&ack, &nested, MAX_DEPTH - 2).is_ok());
    let err = decode_message_at_depth(&ack, &nested, MAX_DEPTH - 1).unwrap_err();
    assert_eq!(err.code(), "INVALID_REQUEST");
    assert!(err.reason().contains("nest deeper"), "{err}");
}

// --- enums (§10.4) ---------------------------------------------------------------

#[test]
fn enums_accept_names_and_numbers_and_ignore_unknown_names() {
    let m = decode(
        "macp.v1.Ack",
        json!({"session_state": "SESSION_STATE_OPEN"}),
    )
    .unwrap();
    assert_eq!(field(&m, "session_state"), PbValue::EnumNumber(1));
    let m = decode("macp.v1.Ack", json!({"session_state": 2})).unwrap();
    assert_eq!(field(&m, "session_state"), PbValue::EnumNumber(2));
    let m = decode("macp.v1.Ack", json!({"session_state": 77})).unwrap();
    assert_eq!(field(&m, "session_state"), PbValue::EnumNumber(77));
    let m = decode(
        "macp.v1.Ack",
        json!({"session_state": "SESSION_STATE_NOPE", "ok": true}),
    )
    .unwrap();
    assert_eq!(field(&m, "session_state"), PbValue::EnumNumber(0));
    assert_eq!(field(&m, "ok"), PbValue::Bool(true));
    // Encoding: names, and the integer for an unnamed value.
    assert_eq!(
        encode_message(&decode("macp.v1.Ack", json!({"session_state": 77})).unwrap())
            ["session_state"],
        json!(77)
    );
}

// --- 64-bit encoding (§10.9) -------------------------------------------------------

#[test]
fn sixty_four_bit_values_are_numbers_until_2_pow_53() {
    let enc = |n: i64| {
        encode_message(&decode("macp.v1.Ack", json!({"accepted_at_unix_ms": n})).unwrap())
            ["accepted_at_unix_ms"]
            .clone()
    };
    assert_eq!(enc(9_007_199_254_740_991), json!(9_007_199_254_740_991_i64));
    assert_eq!(enc(9_007_199_254_740_992), json!("9007199254740992"));
    assert_eq!(enc(-9_007_199_254_740_992), json!("-9007199254740992"));
    assert_eq!(enc(1000), json!(1000));
    // Never an exponent for integers.
    let text = String::from_utf8(encode_response(
        &decode("macp.v1.Ack", json!({"accepted_at_unix_ms": "1e15"})).unwrap(),
    ))
    .unwrap();
    assert!(
        text.contains("\"accepted_at_unix_ms\":1000000000000000"),
        "{text}"
    );
}

// --- defaults (§10.7, §10.10) --------------------------------------------------------

#[test]
fn defaults_are_emitted_and_presence_fields_only_when_set() {
    let v = encode_message(&DynamicMessage::new(message("macp.v1.Ack")));
    assert_eq!(
        v,
        json!({"ok": false, "duplicate": false, "message_id": "", "session_id": "",
               "accepted_at_unix_ms": 0, "session_state": "SESSION_STATE_UNSPECIFIED"})
    );
    let v = encode_message(&DynamicMessage::new(message(
        "macp.v1.WhatIfReplayResponse",
    )));
    assert!(v.get("first_divergence").is_none());
    assert!(v.get("recorded").is_none());
    assert_eq!(v["denials"], json!([]));
    let m = decode(
        "macp.v1.WhatIfReplayResponse",
        json!({"first_divergence": 0}),
    )
    .unwrap();
    assert_eq!(encode_message(&m)["first_divergence"], json!(0));
    let v = encode_message(&DynamicMessage::new(message(
        "macp.v1.ExperimentalCapabilities",
    )));
    assert_eq!(v["features"], json!({}));
}

// --- structural transforms (§10.11) ----------------------------------------------------

#[test]
fn timestamp_accepts_any_offset_and_truncates_below_the_millisecond() {
    assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:01Z"), Ok(1000));
    assert_eq!(
        parse_rfc3339_ms("1970-01-01T01:00:01.0019999+01:00"),
        Ok(1001)
    );
    assert_eq!(parse_rfc3339_ms("1969-12-31T23:59:59.5Z"), Ok(-500));
    assert!(parse_rfc3339_ms("yesterday").is_err());
    assert_eq!(format_rfc3339_ms(1_772_396_641_000), "2026-03-01T20:24:01Z");
    assert_eq!(
        format_rfc3339_ms(1_772_396_641_020),
        "2026-03-01T20:24:01.020Z"
    );
    assert_eq!(format_rfc3339_ms(i64::MAX), "9999-12-31T23:59:59.999Z");
    assert_eq!(format_rfc3339_ms(i64::MIN), "0000-01-01T00:00:00Z");
}

#[test]
fn envelope_timestamp_unix_ms_is_not_a_json_member() {
    let env = envelope_from_json(&json!({"timestamp_unix_ms": 5, "payload_b64": ""})).unwrap();
    assert_eq!(env.timestamp_unix_ms, 0);
    let bad =
        envelope_from_json(&json!({"timestamp": "nope", "payload_b64": "", "message_id": "m"}))
            .unwrap_err();
    assert_eq!(bad.code(), "INVALID_ENVELOPE");
}

#[test]
fn rules_string_and_object_decode_to_the_same_descriptor() {
    let obj = decode(
        "macp.v1.PolicyDescriptor",
        json!({"policy_id": "p", "rules": {"b": 1, "a": {"c": [true]}}}),
    )
    .unwrap();
    let text = decode(
        "macp.v1.PolicyDescriptor",
        json!({"policy_id": "p", "rules": "{ \"a\": {\"c\": [true]}, \"b\": 1 }"}),
    )
    .unwrap();
    assert_eq!(obj, text);
    assert_eq!(
        encode_message(&obj)["rules"],
        json!({"a": {"c": [true]}, "b": 1})
    );
    // Text that is not a JSON object stays the string form both ways.
    let raw = decode("macp.v1.PolicyDescriptor", json!({"rules": "not json"})).unwrap();
    assert_eq!(field(&raw, "rules"), PbValue::String("not json".into()));
    assert_eq!(encode_message(&raw)["rules"], json!("not json"));
}

// --- payload selection (§10.2, §10.12) -----------------------------------------------------

#[test]
fn payload_selection_by_pair() {
    let name =
        |mode: &str, mt: &str| payload_descriptor(mode, mt).map(|d| d.full_name().to_string());
    assert_eq!(name("", "Signal").as_deref(), Some("macp.v1.SignalPayload"));
    assert_eq!(
        name("macp.mode.task.v1", "SessionStart").as_deref(),
        Some("macp.v1.SessionStartPayload")
    );
    assert_eq!(
        name("ext.whatever.v1", "Commitment").as_deref(),
        Some("macp.v1.CommitmentPayload")
    );
    assert_eq!(
        name("macp.mode.decision.v1", "Proposal").as_deref(),
        Some("macp.modes.decision.v1.ProposalPayload")
    );
    assert_eq!(
        name("macp.mode.proposal.v1", "Proposal").as_deref(),
        Some("macp.modes.proposal.v1.ProposalPayload")
    );
    assert_eq!(
        name("macp.mode.quorum.v1", "Reject").as_deref(),
        Some("macp.modes.quorum.v1.RejectPayload")
    );
    assert_eq!(
        name("ext.multi_round.v1", "Contribute").as_deref(),
        Some("macp.modes.multi_round.v1.ContributePayload")
    );
    assert_eq!(
        name("macp.mode.multi_round.v1", "Contribute").as_deref(),
        Some("macp.modes.multi_round.v1.ContributePayload")
    );
    for (mode, mt) in [
        ("ext.custom.v1", "Contribute"),
        ("macp.mode.decision.v1", "Contribute"),
        ("macp.mode.decision.v2", "Proposal"),
        ("macp.mode.decision.v1", "Ack"),
        ("macp.mode.decision.v1", ""),
        ("macp.mode.x.y.v1", "Proposal"),
        ("macp.mode.decision.v1", "v1.SessionStart"),
    ] {
        assert_eq!(name(mode, mt), None, "{mode} {mt}");
    }
}

#[test]
fn both_or_neither_payload_forms_are_invalid_envelope() {
    let base = json!({"mode": "", "message_type": "Signal", "message_id": "m1", "session_id": ""});
    let with = |extra: Value| {
        let mut v = base.clone();
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        envelope_from_json(&v)
    };
    for extra in [
        json!({}),
        json!({"payload": null, "payload_b64": null}),
        json!({"payload": {}, "payload_b64": ""}),
        json!({"payload": [], }),
        json!({"payload": "AAEC"}),
        json!({"payload_b64": 5}),
        json!({"payload_b64": "%%%"}),
        json!({"payload": {"confidence": "high"}}),
    ] {
        let err = with(extra.clone()).expect_err(&extra.to_string());
        match err {
            JsonError::InvalidEnvelope { message_id, .. } => {
                assert_eq!(message_id, "m1", "{extra}")
            }
            other => panic!("{extra}: expected INVALID_ENVELOPE, got {other}"),
        }
    }
    assert_eq!(
        with(json!({"payload_b64": ""})).unwrap().payload,
        Vec::<u8>::new()
    );
    assert_eq!(
        with(json!({"payload": {}})).unwrap().payload,
        Vec::<u8>::new()
    );
    assert_eq!(
        with(json!({"payload": null, "payload_b64": "AAE"}))
            .unwrap()
            .payload,
        vec![0, 1]
    );
}

#[test]
fn envelope_output_falls_back_to_b64_unless_lossless() {
    use macp_pb::pb::Envelope;
    let mut env = Envelope {
        mode: "macp.mode.decision.v1".into(),
        message_type: "Vote".into(),
        payload: canonical_payload_bytes(
            &message("macp.modes.decision.v1.VotePayload"),
            &json!({"proposal_id": "p", "vote": "APPROVE"}),
        )
        .unwrap(),
        ..Default::default()
    };
    let v = envelope_to_json(&env);
    assert_eq!(v["payload"]["vote"], json!("APPROVE"));
    assert!(v.get("payload_b64").is_none());
    assert!(v.get("timestamp_unix_ms").is_none());
    // An unknown field (99, varint 1) makes the bytes non-canonical.
    env.payload.extend_from_slice(&[0x98, 0x06, 0x01]);
    let v = envelope_to_json(&env);
    assert!(v.get("payload").is_none());
    assert!(v["payload_b64"].is_string());
    // Out-of-order fields decode fine but are not canonical.
    env.payload = vec![0x12, 0x01, b'v', 0x0a, 0x01, b'p'];
    assert!(envelope_to_json(&env).get("payload_b64").is_some());
    // No schema for the pair.
    env.mode = "ext.custom.v1".into();
    env.payload = vec![];
    assert_eq!(envelope_to_json(&env)["payload_b64"], json!(""));
}

#[test]
fn decode_request_classifies_failures() {
    assert!(matches!(
        decode_request("Nope", b"{}"),
        Err(JsonError::UnknownMethod(_))
    ));
    assert_eq!(
        decode_request("Send", b"{").unwrap_err().code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        decode_request("Send", b"[]").unwrap_err().code(),
        "INVALID_REQUEST"
    );
    let bare =
        decode_request("Send", br#"{"macp_version":"1.0","message_type":"Signal"}"#).unwrap();
    assert!(!bare.has_field_by_name("envelope"));
    // `null` is absent (§10.10): no envelope member.
    let null = decode_request("Send", br#"{"envelope":null}"#).unwrap();
    assert!(!null.has_field_by_name("envelope"));
    // Anything wrong inside the envelope member is in band (RFC-MACP-0006
    // §4.4), with the identifiers copied when they are strings.
    for (body, mid, sid) in [
        (r#"{"envelope":5}"#, "", ""),
        (r#"{"envelope":"x"}"#, "", ""),
        (
            r#"{"envelope":{"mode":1,"message_id":"m1","session_id":"s1"}}"#,
            "m1",
            "s1",
        ),
        (
            r#"{"envelope":{"message_id":7,"session_id":"s1","payload_b64":""}}"#,
            "",
            "s1",
        ),
    ] {
        match decode_request("Send", body.as_bytes()).unwrap_err() {
            JsonError::InvalidEnvelope {
                message_id,
                session_id,
                ..
            } => assert_eq!(
                (message_id.as_str(), session_id.as_str()),
                (mid, sid),
                "{body}"
            ),
            other => panic!("{body}: {other}"),
        }
    }
    // A wrong type outside the envelope member stays a request-level failure.
    assert_eq!(
        decode_request("GetSession", br#"{"session_id":7}"#)
            .unwrap_err()
            .code(),
        "INVALID_REQUEST"
    );
}
