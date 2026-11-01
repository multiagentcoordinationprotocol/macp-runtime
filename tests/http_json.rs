//! The canonical JSON codec (RFC-MACP-0001 §10), `http-listener` feature.
//!
//! Acceptance tests of plan P3: the vendored `tests/http/examples` round-trip,
//! canonical bytes against `prost` (equal, and different exactly where §10.13
//! says), the number rules, a decode→encode→decode fixpoint over a hand-rolled
//! corpus (`tests/http_json/corpus/`) for every request/response type of the
//! service, the payload-selection table pinned against `macp-conform`, and
//! the codec's isolation from the transport.
//!
//! Regenerate the corpus only on purpose:
//! `MACP_REGEN_GOLDEN=1 cargo test -p macp-runtime --features http-listener --test http_json`.
#![cfg(feature = "http-listener")]

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use macp_runtime::http::json::{
    canonical_bytes, canonical_payload_bytes, decode_message, decode_request, descriptor_pool,
    encode_message, encode_response, envelope_from_json, envelope_to_json, method_descriptor,
    payload_descriptor, JsonError, SERVICE_NAME,
};
use macp_runtime::pb::{self, CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::{decision_pb, handoff_pb, multi_round_pb, proposal_pb, quorum_pb, task_pb};
use prost::Message;
use prost_reflect::{DynamicMessage, Kind, MessageDescriptor, Value as PbValue};
use serde_json::{json, Map, Value};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn msg(name: &str) -> MessageDescriptor {
    descriptor_pool()
        .get_message_by_name(name)
        .unwrap_or_else(|| panic!("{name} not in pool"))
}

fn service_methods() -> Vec<prost_reflect::MethodDescriptor> {
    descriptor_pool()
        .get_service_by_name(SERVICE_NAME)
        .expect("service in pool")
        .methods()
        .collect()
}

/// Every mode payload message, by package.
const MODE_PAYLOADS: &[(&str, &[&str])] = &[
    ("decision", &["Proposal", "Evaluation", "Objection", "Vote"]),
    (
        "proposal",
        &[
            "Proposal",
            "CounterProposal",
            "Accept",
            "Reject",
            "Withdraw",
        ],
    ),
    (
        "task",
        &[
            "TaskRequest",
            "TaskAccept",
            "TaskReject",
            "TaskUpdate",
            "TaskComplete",
            "TaskFail",
        ],
    ),
    (
        "handoff",
        &[
            "HandoffOffer",
            "HandoffContext",
            "HandoffAccept",
            "HandoffDecline",
        ],
    ),
    (
        "quorum",
        &["ApprovalRequest", "Approve", "Reject", "Abstain"],
    ),
    ("multi_round", &["Contribute"]),
];

const CORE_PAYLOADS: &[&str] = &[
    "Signal",
    "Progress",
    "SessionStart",
    "SessionCancel",
    "SessionSuspend",
    "SessionResume",
    "Commitment",
];

// ---------------------------------------------------------------------------
// Descriptor pool and isolation
// ---------------------------------------------------------------------------

#[test]
fn pool_holds_the_service_and_every_payload_message() {
    assert_eq!(service_methods().len(), 27, "MACPRuntimeService RPC count");
    assert!(method_descriptor("Send").is_some());
    assert!(method_descriptor("send").is_none());
    for (pkg, types) in MODE_PAYLOADS {
        for t in *types {
            msg(&format!("macp.modes.{pkg}.v1.{t}Payload"));
        }
    }
    for t in CORE_PAYLOADS {
        msg(&format!("macp.v1.{t}Payload"));
    }
    // ...and nothing else: every `*Payload` in a mode package is listed.
    let listed: BTreeSet<String> = MODE_PAYLOADS
        .iter()
        .flat_map(|(pkg, types)| {
            types
                .iter()
                .map(move |t| format!("macp.modes.{pkg}.v1.{t}Payload"))
        })
        .collect();
    let in_pool: BTreeSet<String> = descriptor_pool()
        .all_messages()
        .filter(|m| m.package_name().starts_with("macp.modes."))
        .map(|m| m.full_name().to_string())
        .collect();
    assert_eq!(listed, in_pool);
}

/// The canonical encoder's packed-repeated, zigzag (`sint*`) and fixed-width
/// paths are exercised by no MACP message today; this pins that, so a schema
/// that starts using them fails here and must add a prost-equality case for
/// them to `canonical_bytes_equal_prost_for_every_payload_type_without_maps_or_nan`.
#[test]
fn no_macp_message_uses_packed_zigzag_or_fixed_encodings() {
    let mut offenders = Vec::new();
    for m in descriptor_pool().all_messages() {
        if !m.package_name().starts_with("macp.") {
            continue;
        }
        for f in m.fields() {
            let kind = f.kind();
            let special = matches!(
                kind,
                Kind::Sint32
                    | Kind::Sint64
                    | Kind::Fixed32
                    | Kind::Fixed64
                    | Kind::Sfixed32
                    | Kind::Sfixed64
            );
            let packed =
                f.is_list() && !matches!(kind, Kind::String | Kind::Bytes | Kind::Message(_));
            if special || packed {
                offenders.push(f.full_name().to_string());
            }
        }
    }
    assert!(offenders.is_empty(), "untested encodings: {offenders:?}");
}

/// The codec may import `serde_json`, `base64`, `prost`, `prost-reflect` and
/// `chrono` (an existing root dependency, for §10.11(1) timestamps); it must
/// never reach the transport (tonic, hyper) or the gRPC server.
#[test]
fn codec_imports_no_transport_or_server_items() {
    let dir = root().join("src/http/json");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            for (i, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                for banned in ["tonic", "hyper", "crate::server", "super::super::server"] {
                    assert!(
                        !code.contains(banned),
                        "{}:{}: the JSON codec must not use `{banned}`: {line}",
                        path.display(),
                        i + 1
                    );
                }
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 6,
        "expected the codec's six modules, found {checked}"
    );
}

// ---------------------------------------------------------------------------
// Vendored examples (tests/http/examples)
// ---------------------------------------------------------------------------

fn is_default_json(v: &Value) -> bool {
    match v {
        Value::String(s) => s.is_empty() || s.ends_with("_UNSPECIFIED"),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Bool(b) => !b,
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Null => false,
    }
}

/// `encoded` equals `example`, except for keys `example` omits whose value in
/// `encoded` is the proto3 default (the encoder emits every default, §10.10).
fn equal_modulo_omitted_defaults(encoded: &Value, example: &Value, path: &str) {
    match (encoded, example) {
        (Value::Object(e), Value::Object(x)) => {
            for (k, xv) in x {
                let ev = e
                    .get(k)
                    .unwrap_or_else(|| panic!("{path}.{k}: missing from {encoded}"));
                equal_modulo_omitted_defaults(ev, xv, &format!("{path}.{k}"));
            }
            for (k, ev) in e {
                if !x.contains_key(k) {
                    assert!(is_default_json(ev), "{path}.{k}: extra non-default {ev}");
                }
            }
        }
        (Value::Array(e), Value::Array(x)) => {
            assert_eq!(e.len(), x.len(), "{path}");
            for (i, (ev, xv)) in e.iter().zip(x).enumerate() {
                equal_modulo_omitted_defaults(ev, xv, &format!("{path}[{i}]"));
            }
        }
        (Value::Number(a), Value::Number(b)) => assert_eq!(a.as_f64(), b.as_f64(), "{path}"),
        _ => assert_eq!(encoded, example, "{path}"),
    }
}

fn round_trip(desc: &MessageDescriptor, example: &Value, path: &str) {
    let decoded = decode_message(desc, example).unwrap_or_else(|e| panic!("{path}: {e}"));
    let encoded = encode_message(&decoded);
    equal_modulo_omitted_defaults(&encoded, example, path);
}

#[test]
fn vendored_examples_round_trip() {
    let mut round_tripped = 0;
    for name in [
        "send",
        "errors",
        "stream_session_subscribe",
        "watch_signals",
    ] {
        let path = root().join(format!("tests/http/examples/{name}.json"));
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (i, ex) in doc["exchanges"].as_array().unwrap().iter().enumerate() {
            let at = format!("{name}#{i}");
            let method = ex["request"]["url"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap();
            let Some(method) = method_descriptor(method) else {
                continue; // the transport-404 example names no RPC
            };
            let response = &ex["response"];
            let status = response["status"].as_u64().unwrap();
            let body = &ex["request"]["body"];
            if status == 200 {
                round_trip(&method.input(), body, &format!("{at} request"));
                // Every Envelope in a request converts to the generated type.
                if let Some(env) = body.get("envelope") {
                    envelope_from_json(env).unwrap_or_else(|e| panic!("{at}: {e}"));
                }
                round_tripped += 1;
            } else if body.get("envelope").is_none() && method.name() == "Send" {
                // The bare-Envelope body: decodes, but carries no envelope.
                let m = decode_request("Send", body.to_string().as_bytes()).unwrap();
                assert!(!m.has_field_by_name("envelope"), "{at}");
            }
            if let Some(body) = response.get("body").filter(|b| !b.is_null()) {
                if let Some(err) = body.get("error") {
                    round_trip(&msg("macp.v1.MACPError"), err, &format!("{at} error body"));
                } else {
                    round_trip(&method.output(), body, &format!("{at} response"));
                }
                round_tripped += 1;
            }
            for (j, event) in response["events"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(data) = event.get("data") {
                    round_trip(&method.output(), data, &format!("{at} event {j}"));
                    if let Some(env) = data.get("envelope") {
                        // The output side picks the same payload form back.
                        let back = envelope_to_json(&envelope_from_json(env).unwrap());
                        equal_modulo_omitted_defaults(&back, env, &format!("{at} event {j}"));
                    }
                    round_tripped += 1;
                }
            }
        }
    }
    assert!(
        round_tripped >= 15,
        "only {round_tripped} example bodies checked"
    );
}

// ---------------------------------------------------------------------------
// Synthetic values (also the generator of the hand-rolled corpus)
// ---------------------------------------------------------------------------

/// A maximal value: every field set to a non-default value. `maps` and
/// `oneof_member` pick what to include; nested messages recurse.
fn maximal(
    desc: &MessageDescriptor,
    maps: bool,
    oneof_member: Option<&str>,
    depth: usize,
) -> Value {
    if desc.full_name() == "macp.v1.Envelope" {
        return json!({
            "macp_version": "1.0",
            "mode": "macp.mode.decision.v1",
            "message_type": "Vote",
            "message_id": "01HV0Q2D2E4F6G8H0J2K4M6N8P",
            "session_id": "01HV0Q2A8W0Y2XQ5N1R8Y2K9B3",
            "sender": "agent://finance",
            "timestamp": "2026-03-01T20:25:10.250Z",
            "payload": {"proposal_id": "p-1", "vote": "APPROVE", "reason": "Within budget ✓"}
        });
    }
    let mut obj = Map::new();
    for field in desc.fields() {
        // One member per real oneof: the requested one, else the first.
        if let Some(oneof) = field.containing_oneof().filter(|o| !o.is_synthetic()) {
            let requested = oneof_member.filter(|m| oneof.fields().any(|f| f.name() == *m));
            let pick = match requested {
                Some(m) => m == field.name(),
                None => oneof.fields().next().map(|f| f.number()) == Some(field.number()),
            };
            if !pick {
                continue;
            }
        }
        if field.is_map() && !maps {
            continue;
        }
        if desc.full_name() == "macp.v1.PolicyDescriptor" && field.name() == "rules" {
            obj.insert(
                "rules".into(),
                json!({"voting": {"algorithm": "majority", "threshold": 0.5}}),
            );
            continue;
        }
        let kind = field.kind();
        let value = if field.is_map() {
            let Kind::Message(entry) = &kind else {
                unreachable!()
            };
            let vk = entry.map_entry_value_field().kind();
            json!({"k1": scalar(&vk, field.name(), depth), "k2": scalar(&vk, field.name(), depth)})
        } else if field.is_list() {
            json!([
                scalar(&kind, field.name(), depth),
                scalar(&kind, field.name(), depth)
            ])
        } else {
            scalar(&kind, field.name(), depth)
        };
        obj.insert(field.name().to_string(), value);
    }
    Value::Object(obj)
}

fn scalar(kind: &Kind, name: &str, depth: usize) -> Value {
    match kind {
        Kind::String => json!(format!("{name}-✓")),
        Kind::Bool => json!(true),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => json!(-7),
        Kind::Uint32 | Kind::Fixed32 => json!(7),
        // Above 2^53: quoted by the encoder (§10.9).
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => json!(9_007_199_254_740_993_i64),
        Kind::Uint64 | Kind::Fixed64 => json!(u64::MAX),
        Kind::Double => json!(0.25),
        Kind::Float => json!(0.5),
        Kind::Bytes => json!("AAECAwQ="),
        Kind::Enum(e) => json!(e.values().last().unwrap().name()),
        Kind::Message(m) => {
            if depth > 6 {
                json!({})
            } else {
                maximal(m, true, None, depth + 1)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical bytes against prost (§10.13)
// ---------------------------------------------------------------------------

fn prost_bytes(name: &str, m: &DynamicMessage) -> Vec<u8> {
    macro_rules! table {
        ($($full:literal => $ty:ty),* $(,)?) => {
            match name {
                $($full => m.transcode_to::<$ty>().unwrap().encode_to_vec(),)*
                other => panic!("no generated type registered for {other}"),
            }
        };
    }
    table! {
        "macp.v1.SignalPayload" => pb::SignalPayload,
        "macp.v1.ProgressPayload" => pb::ProgressPayload,
        "macp.v1.SessionStartPayload" => pb::SessionStartPayload,
        "macp.v1.SessionCancelPayload" => pb::SessionCancelPayload,
        "macp.v1.SessionSuspendPayload" => pb::SessionSuspendPayload,
        "macp.v1.SessionResumePayload" => pb::SessionResumePayload,
        "macp.v1.CommitmentPayload" => pb::CommitmentPayload,
        "macp.modes.decision.v1.ProposalPayload" => decision_pb::ProposalPayload,
        "macp.modes.decision.v1.EvaluationPayload" => decision_pb::EvaluationPayload,
        "macp.modes.decision.v1.ObjectionPayload" => decision_pb::ObjectionPayload,
        "macp.modes.decision.v1.VotePayload" => decision_pb::VotePayload,
        "macp.modes.proposal.v1.ProposalPayload" => proposal_pb::ProposalPayload,
        "macp.modes.proposal.v1.CounterProposalPayload" => proposal_pb::CounterProposalPayload,
        "macp.modes.proposal.v1.AcceptPayload" => proposal_pb::AcceptPayload,
        "macp.modes.proposal.v1.RejectPayload" => proposal_pb::RejectPayload,
        "macp.modes.proposal.v1.WithdrawPayload" => proposal_pb::WithdrawPayload,
        "macp.modes.task.v1.TaskRequestPayload" => task_pb::TaskRequestPayload,
        "macp.modes.task.v1.TaskAcceptPayload" => task_pb::TaskAcceptPayload,
        "macp.modes.task.v1.TaskRejectPayload" => task_pb::TaskRejectPayload,
        "macp.modes.task.v1.TaskUpdatePayload" => task_pb::TaskUpdatePayload,
        "macp.modes.task.v1.TaskCompletePayload" => task_pb::TaskCompletePayload,
        "macp.modes.task.v1.TaskFailPayload" => task_pb::TaskFailPayload,
        "macp.modes.handoff.v1.HandoffOfferPayload" => handoff_pb::HandoffOfferPayload,
        "macp.modes.handoff.v1.HandoffContextPayload" => handoff_pb::HandoffContextPayload,
        "macp.modes.handoff.v1.HandoffAcceptPayload" => handoff_pb::HandoffAcceptPayload,
        "macp.modes.handoff.v1.HandoffDeclinePayload" => handoff_pb::HandoffDeclinePayload,
        "macp.modes.quorum.v1.ApprovalRequestPayload" => quorum_pb::ApprovalRequestPayload,
        "macp.modes.quorum.v1.ApprovePayload" => quorum_pb::ApprovePayload,
        "macp.modes.quorum.v1.RejectPayload" => quorum_pb::RejectPayload,
        "macp.modes.quorum.v1.AbstainPayload" => quorum_pb::AbstainPayload,
        "macp.modes.multi_round.v1.ContributePayload" => multi_round_pb::ContributePayload,
    }
}

fn all_payload_names() -> Vec<String> {
    CORE_PAYLOADS
        .iter()
        .map(|t| format!("macp.v1.{t}Payload"))
        .chain(MODE_PAYLOADS.iter().flat_map(|(pkg, types)| {
            types
                .iter()
                .map(move |t| format!("macp.modes.{pkg}.v1.{t}Payload"))
        }))
        .collect()
}

#[test]
fn canonical_bytes_equal_prost_for_every_payload_type_without_maps_or_nan() {
    for name in all_payload_names() {
        let desc = msg(&name);
        for value in [maximal(&desc, false, None, 0), json!({})] {
            let ours = canonical_payload_bytes(&desc, &value).unwrap();
            let decoded = decode_message(&desc, &value).unwrap();
            assert_eq!(ours, prost_bytes(&name, &decoded), "{name}: {value}");
        }
    }
}

#[test]
fn canonical_bytes_equal_hand_built_prost_values() {
    let start = SessionStartPayload {
        intent: "Select a vendor".into(),
        participants: vec!["agent://a".into(), "agent://b".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: "policy.default".into(),
        ttl_ms: 900_000,
        roots: vec![pb::Root {
            uri: "file:///r".into(),
            name: String::new(),
        }],
        context_id: "ctx".into(),
        extensions: HashMap::new(),
        max_suspend_ms: 60_000,
    };
    let json_start = json!({
        "intent": "Select a vendor", "participants": ["agent://a", "agent://b"],
        "mode_version": "1.0.0", "configuration_version": "cfg-1",
        "policy_version": "policy.default", "ttl_ms": "900000",
        "roots": [{"uri": "file:///r"}], "context_id": "ctx", "max_suspend_ms": 6e4,
        "maxSuspendMs": 1, "unknown": {"x": 1}
    });
    assert_eq!(
        canonical_payload_bytes(&msg("macp.v1.SessionStartPayload"), &json_start).unwrap(),
        start.encode_to_vec()
    );
    let commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        outcome_positive: true,
        supersedes: Some(pb::CommitmentRef::default()),
        ..Default::default()
    };
    // A set-but-default message field (explicit presence) is emitted.
    assert_eq!(
        canonical_payload_bytes(
            &msg("macp.v1.CommitmentPayload"),
            &json!({"commitment_id": "c1", "action": "decision.selected", "outcome_positive": true, "supersedes": {}})
        )
        .unwrap(),
        commitment.encode_to_vec()
    );
    let approval = quorum_pb::ApprovalRequestPayload {
        request_id: "r".into(),
        details: vec![0xfb, 0xff],
        required_approvals: 3,
        ..Default::default()
    };
    assert_eq!(
        canonical_payload_bytes(
            &msg("macp.modes.quorum.v1.ApprovalRequestPayload"),
            &json!({"request_id": "r", "details": "-_8", "required_approvals": "3"})
        )
        .unwrap(),
        approval.encode_to_vec()
    );
}

#[test]
fn canonical_bytes_differ_from_prost_exactly_where_10_13_says() {
    let start = msg("macp.v1.SessionStartPayload");

    // Map entry with a default key and value: both are written (rule 5);
    // prost writes an empty entry.
    let ours = canonical_payload_bytes(&start, &json!({"extensions": {"": ""}})).unwrap();
    assert_eq!(ours, vec![0x4a, 0x04, 0x0a, 0x00, 0x12, 0x00]);
    let prost = SessionStartPayload {
        extensions: HashMap::from([(String::new(), Vec::new())]),
        ..Default::default()
    }
    .encode_to_vec();
    assert_eq!(prost, vec![0x4a, 0x00]);

    // Map keys by UTF-8 bytes: U+FFFF (EF BF BF) before U+10000 (F0 90 80
    // 80). UTF-16 code-unit order (FFFF vs D800 DC00) is the reverse.
    let ours = canonical_payload_bytes(
        &start,
        &json!({"extensions": {"\u{10000}": "AQ==", "\u{FFFF}": "Ag=="}}),
    )
    .unwrap();
    let first_key_at = 4; // tag 0x4a, len, tag 0x0a, key len
    assert_eq!(&ours[first_key_at..first_key_at + 3], "\u{FFFF}".as_bytes());
    assert!("\u{FFFF}".encode_utf16().next() > "\u{10000}".encode_utf16().next());
    // Deterministic whatever the JSON member order.
    let reordered = canonical_payload_bytes(
        &start,
        &json!({"extensions": {"\u{FFFF}": "Ag==", "\u{10000}": "AQ=="}}),
    )
    .unwrap();
    assert_eq!(ours, reordered);

    // NaN: the canonical quiet NaN (rule 6), whatever NaN the value holds.
    let eval = msg("macp.modes.decision.v1.EvaluationPayload");
    let mut m = DynamicMessage::new(eval.clone());
    m.set_field_by_name(
        "confidence",
        PbValue::F64(f64::from_bits(0xFFF8_0000_0000_0001)),
    );
    let ours = canonical_bytes(&m);
    assert_eq!(
        ours,
        [&[0x19][..], &0x7FF8_0000_0000_0000_u64.to_le_bytes()].concat()
    );
    let prost = decision_pb::EvaluationPayload {
        confidence: f64::from_bits(0xFFF8_0000_0000_0001),
        ..Default::default()
    }
    .encode_to_vec();
    assert_ne!(ours, prost);
    assert_eq!(
        canonical_payload_bytes(&eval, &json!({"confidence": "NaN"})).unwrap(),
        ours
    );

    // -0.0 is not the default (rule 2); prost compares with == and drops it.
    let ours = canonical_payload_bytes(&eval, &json!({"confidence": -0.0})).unwrap();
    assert_eq!(
        ours,
        [&[0x19][..], &(-0.0f64).to_bits().to_le_bytes()].concat()
    );
    let prost = decision_pb::EvaluationPayload {
        confidence: -0.0,
        ..Default::default()
    }
    .encode_to_vec();
    assert!(prost.is_empty());
    assert_eq!(
        encode_message(&decode_message(&eval, &json!({"confidence": -0.0})).unwrap())["confidence"],
        json!(-0.0)
    );
}

// ---------------------------------------------------------------------------
// Number rules and names (§10.8, §10.9)
// ---------------------------------------------------------------------------

fn get(m: &DynamicMessage, name: &str) -> PbValue {
    m.get_field_by_name(name).unwrap().into_owned()
}

#[test]
fn number_and_name_rules() {
    let start = msg("macp.v1.SessionStartPayload");
    let list = msg("macp.v1.ListSessionsRequest");
    let eval = msg("macp.modes.decision.v1.EvaluationPayload");

    // `sessionId` instead of `session_id` leaves the field empty.
    let env = envelope_from_json(&json!({"sessionId": "s1", "payload_b64": ""})).unwrap();
    assert_eq!(env.session_id, "");
    // Unknown enum name leaves the default.
    let ack = decode_message(
        &msg("macp.v1.Ack"),
        &json!({"session_state": "SESSION_STATE_LATER"}),
    )
    .unwrap();
    assert_eq!(get(&ack, "session_state"), PbValue::EnumNumber(0));
    // "900000" = 900000.
    let m = decode_message(&start, &json!({"ttl_ms": "900000"})).unwrap();
    assert_eq!(get(&m, "ttl_ms"), PbValue::I64(900_000));
    // 1e3, "1e3" and 1000.0 decode to 1000 for int32 and int64.
    for v in [json!(1e3), json!("1e3"), json!(1000.0)] {
        let m = decode_message(&list, &json!({"page_size": v})).unwrap();
        assert_eq!(get(&m, "page_size"), PbValue::I32(1000), "int32 {v}");
        let m = decode_message(&start, &json!({"ttl_ms": v})).unwrap();
        assert_eq!(get(&m, "ttl_ms"), PbValue::I64(1000), "int64 {v}");
    }
    // "2.5" and 2.5e0 decode for a double.
    for v in [json!("2.5"), json!(2.5e0)] {
        let m = decode_message(&eval, &json!({"confidence": v})).unwrap();
        assert_eq!(get(&m, "confidence"), PbValue::F64(2.5), "{v}");
    }
    // 1.5 for an int is an error, never rounded.
    assert!(decode_message(&list, &json!({"page_size": 1.5})).is_err());
    assert!(decode_message(&start, &json!({"ttl_ms": "1.5"})).is_err());
    // 2^53 encodes quoted; 2^53-1 does not.
    let m = decode_message(&start, &json!({"ttl_ms": 9_007_199_254_740_992_i64})).unwrap();
    assert_eq!(encode_message(&m)["ttl_ms"], json!("9007199254740992"));
    let m = decode_message(&start, &json!({"ttl_ms": "9007199254740991"})).unwrap();
    assert_eq!(
        encode_message(&m)["ttl_ms"],
        json!(9_007_199_254_740_991_i64)
    );
    // The encoder never writes an exponent for an integer.
    let m = decode_message(&start, &json!({"ttl_ms": 1e15, "max_suspend_ms": "2e3"})).unwrap();
    let text = String::from_utf8(encode_response(&m)).unwrap();
    assert!(text.contains("\"ttl_ms\":1000000000000000"), "{text}");
    assert!(text.contains("\"max_suspend_ms\":2000"), "{text}");
    let bytes = text.as_bytes();
    assert!(
        !bytes
            .windows(2)
            .any(|w| w[0].is_ascii_digit() && matches!(w[1], b'e' | b'E')),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// Corpus fixpoint: decode → encode → decode, every request/response type
// ---------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    root().join("tests/http_json/corpus")
}

/// The corpus cases for one message type: maximal (no maps, then with maps),
/// one per extra oneof member, and all-defaults.
fn generate_cases(desc: &MessageDescriptor) -> Value {
    let mut cases = vec![
        json!({"name": "maximal", "value": maximal(desc, true, None, 0)}),
        // An Envelope must carry one payload form (§10.2): the empty one.
        if desc.full_name() == "macp.v1.Envelope" {
            json!({"name": "defaults", "value": {"payload_b64": ""}})
        } else {
            json!({"name": "defaults", "value": {}})
        },
    ];
    for oneof in desc.oneofs().filter(|o| !o.is_synthetic()) {
        for member in oneof.fields().skip(1) {
            cases.push(json!({
                "name": format!("oneof {}", member.name()),
                "value": maximal(desc, true, Some(member.name()), 0),
            }));
        }
    }
    json!({"type": desc.full_name(), "cases": cases})
}

fn corpus_types() -> BTreeSet<String> {
    let mut types: BTreeSet<String> = service_methods()
        .iter()
        .flat_map(|m| {
            [
                m.input().full_name().to_string(),
                m.output().full_name().to_string(),
            ]
        })
        .collect();
    types.extend(all_payload_names());
    types.insert("macp.v1.Envelope".into());
    types.insert("macp.v1.MACPError".into());
    types
}

fn assert_fixpoint(desc: &MessageDescriptor, value: &Value, at: &str) {
    let m1 = decode_message(desc, value).unwrap_or_else(|e| panic!("{at}: {e}"));
    let e1 = encode_message(&m1);
    let m2 = decode_message(desc, &e1).unwrap_or_else(|e| panic!("{at}: re-decode: {e}\n{e1}"));
    assert_eq!(m1, m2, "{at}: decode(encode(m)) != m");
    let e2 = encode_message(&m2);
    assert_eq!(e1, e2, "{at}: encode is not stable");
    // And the wire form agrees with prost-reflect's own encoding round trip.
    let wire = m1.encode_to_vec();
    assert_eq!(
        DynamicMessage::decode(desc.clone(), wire.as_slice()).unwrap(),
        m1,
        "{at}: wire"
    );
}

#[test]
fn corpus_is_a_fixpoint_for_every_service_message_type() {
    let regen = std::env::var("MACP_REGEN_GOLDEN").as_deref() == Ok("1");
    if regen {
        std::fs::create_dir_all(corpus_dir()).unwrap();
    }
    let types = corpus_types();
    for name in &types {
        let desc = msg(name);
        let file = corpus_dir().join(format!("{name}.json"));
        if regen {
            let mut text = serde_json::to_string_pretty(&generate_cases(&desc)).unwrap();
            text.push('\n');
            std::fs::write(&file, text).unwrap();
        }
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap_or_else(|e| {
                panic!(
                    "{name}: no corpus at {} ({e}); regenerate with MACP_REGEN_GOLDEN=1",
                    file.display()
                )
            }))
            .unwrap();
        assert_eq!(doc["type"], json!(name));
        let cases = doc["cases"].as_array().unwrap();
        assert!(
            cases.len() >= 2,
            "{name}: needs a maximal and an all-defaults case"
        );
        for case in cases {
            assert_fixpoint(&desc, &case["value"], &format!("{name} {}", case["name"]));
        }
    }
    // No stray corpus file for a type that is no longer in the service.
    for entry in std::fs::read_dir(corpus_dir()).unwrap() {
        let file = entry.unwrap().file_name().into_string().unwrap();
        let stem = file.strip_suffix(".json").unwrap_or(&file);
        assert!(types.contains(stem), "stray corpus file {file}");
    }
}

#[test]
fn vendored_examples_are_a_fixpoint() {
    for name in [
        "send",
        "errors",
        "stream_session_subscribe",
        "watch_signals",
    ] {
        let doc: Value = serde_json::from_str(
            &std::fs::read_to_string(root().join(format!("tests/http/examples/{name}.json")))
                .unwrap(),
        )
        .unwrap();
        for (i, ex) in doc["exchanges"].as_array().unwrap().iter().enumerate() {
            let method = ex["request"]["url"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap();
            let Some(method) = method_descriptor(method) else {
                continue;
            };
            let at = format!("{name}#{i}");
            assert_fixpoint(&method.input(), &ex["request"]["body"], &at);
            for event in ex["response"]["events"].as_array().into_iter().flatten() {
                if let Some(data) = event.get("data") {
                    assert_fixpoint(&method.output(), data, &at);
                }
            }
            if let Some(body) = ex["response"].get("body").filter(|b| b.is_object()) {
                assert_fixpoint(&method.output(), body, &at);
            }
        }
    }
}

/// The conformance fixtures' envelopes: each payload encoded by
/// `macp-conform`, carried as `payload_b64`, converts to a JSON `payload`
/// that maps back to the very same bytes, and is a fixpoint.
#[test]
fn conformance_fixture_envelopes_are_a_fixpoint() {
    use base64::Engine as _;
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let mut checked = 0;
    let mut files: Vec<PathBuf> = std::fs::read_dir(root().join("tests/conformance"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    for path in files {
        let fx: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let Some(mode) = fx["mode"].as_str() else {
            continue;
        };
        let start = macp_conform::encode::encode_payload(
            "macp.v1.SessionStartPayload",
            "SessionStart",
            &fx,
        )
        .unwrap();
        let mut messages = vec![("SessionStart".to_string(), fx["initiator"].clone(), start)];
        for m in fx["messages"].as_array().into_iter().flatten() {
            let (Some(pt), Some(mt)) = (m["payload_type"].as_str(), m["message_type"].as_str())
            else {
                continue;
            };
            let Ok(bytes) = macp_conform::encode::encode_payload(pt, mt, &m["payload"]) else {
                continue;
            };
            messages.push((mt.to_string(), m["sender"].clone(), bytes));
        }
        for (i, (mt, sender, bytes)) in messages.into_iter().enumerate() {
            let at = format!("{} #{i} {mt}", path.display());
            let env = json!({
                "macp_version": "1.0", "mode": mode, "message_type": mt,
                "message_id": format!("m{i}"), "session_id": "01HV0Q2A8W0Y2XQ5N1R8Y2K9B3",
                "sender": sender, "timestamp": "2026-03-01T20:24:01Z", "payload_b64": b64(&bytes),
            });
            assert_fixpoint(&msg("macp.v1.Envelope"), &env, &at);
            // prost's encoding of these values is canonical (no maps, no
            // NaN, no -0.0), so the output side picks the JSON form.
            let out = envelope_to_json(&envelope_from_json(&env).unwrap());
            assert!(
                out.get("payload").is_some(),
                "{at}: expected a JSON payload, got {out}"
            );
            assert_eq!(envelope_from_json(&out).unwrap().payload, bytes, "{at}");
            checked += 1;
        }
    }
    assert!(checked >= 100, "only {checked} fixture envelopes checked");
}

// ---------------------------------------------------------------------------
// Payload selection (§10.12), pinned against macp-conform
// ---------------------------------------------------------------------------

/// The standards-track and built-in extension modes, with the message types
/// each declares a payload for.
const MODE_TABLE: &[(&str, &str)] = &[
    ("macp.mode.decision.v1", "decision"),
    ("macp.mode.proposal.v1", "proposal"),
    ("macp.mode.task.v1", "task"),
    ("macp.mode.handoff.v1", "handoff"),
    ("macp.mode.quorum.v1", "quorum"),
    ("ext.multi_round.v1", "multi_round"),
];

/// Only the scalar fields `macp-conform`'s encoder reads (no bytes, repeated
/// or message fields), so its bytes and ours are comparable.
fn conform_comparable(desc: &MessageDescriptor) -> Value {
    let mut obj = Map::new();
    for f in desc.fields() {
        if f.is_list() || f.is_map() || matches!(f.kind(), Kind::Bytes | Kind::Message(_)) {
            continue;
        }
        let v = match f.kind() {
            Kind::Int64 => json!(1_772_396_641_020_i64),
            Kind::Uint32 => json!(3),
            Kind::Double => json!(0.75),
            Kind::Bool => json!(true),
            _ => json!(format!("{}-v", f.name())),
        };
        obj.insert(f.name().to_string(), v);
    }
    Value::Object(obj)
}

#[test]
fn payload_selection_table_is_pinned_against_macp_conform() {
    let package_types: HashMap<&str, &[&str]> = MODE_PAYLOADS.iter().copied().collect();
    let all_types: BTreeSet<&str> = MODE_PAYLOADS
        .iter()
        .flat_map(|(_, t)| t.iter().copied())
        .collect();
    for (mode, pkg) in MODE_TABLE {
        for mt in all_types
            .iter()
            .copied()
            .chain(["Commitment", "SessionStart", "Signal", "Bogus"])
        {
            let ours = payload_descriptor(mode, mt);
            let expected_mode_type = package_types[pkg].contains(&mt);
            let core = CORE_PAYLOADS.contains(&mt);
            assert_eq!(ours.is_some(), expected_mode_type || core, "{mode} {mt}");
            let Some(desc) = ours else {
                // conform has no message for the pair either.
                assert!(
                    macp_conform::encode::encode_payload(
                        &format!("macp.modes.{pkg}.v1.{mt}Payload"),
                        mt,
                        &json!({})
                    )
                    .is_err(),
                    "{mode} {mt}: conform knows a message we refuse"
                );
                continue;
            };
            let expected_name = if core {
                format!("macp.v1.{mt}Payload")
            } else {
                format!("macp.modes.{pkg}.v1.{mt}Payload")
            };
            assert_eq!(desc.full_name(), expected_name, "{mode} {mt}");
            if mt == "Signal" {
                continue; // conform encodes only the payloads fixtures use
            }
            let value = conform_comparable(&desc);
            let conform = macp_conform::encode::encode_payload(desc.full_name(), mt, &value)
                .unwrap_or_else(|e| panic!("{mode} {mt}: conform refuses {expected_name}: {e}"));
            if mt != "SessionStart" {
                // conform ignores SessionStart's context_id and max_suspend_ms.
                assert_eq!(
                    canonical_payload_bytes(&desc, &value).unwrap(),
                    conform,
                    "{mode} {mt}"
                );
            }
        }
    }
}

#[test]
fn json_payload_for_a_dynamically_registered_ext_mode_is_invalid_envelope() {
    use macp_runtime::mode_registry::ModeRegistry;
    let registry =
        ModeRegistry::build_default(Arc::new(macp_runtime::policy::DefaultPolicyEvaluator));
    let mode = "ext.custom.v1";
    registry
        .register_extension(pb::ModeDescriptor {
            mode: mode.into(),
            mode_version: "1.0.0".into(),
            message_types: vec![
                "SessionStart".into(),
                "Contribute".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        })
        .unwrap();
    assert!(registry.get_mode(mode).is_some(), "registered");

    // `Contribute` is a multi_round message type, but this mode is not
    // `ext.multi_round.v1`: the codec has no schema for the pair.
    let env = json!({
        "macp_version": "1.0", "mode": mode, "message_type": "Contribute",
        "message_id": "m1", "session_id": "s1", "sender": "agent://a",
        "payload": {"value": "x"},
    });
    match envelope_from_json(&env) {
        Err(JsonError::InvalidEnvelope {
            message_id,
            session_id,
            ..
        }) => {
            assert_eq!((message_id.as_str(), session_id.as_str()), ("m1", "s1"));
        }
        other => panic!("expected INVALID_ENVELOPE, got {other:?}"),
    }
    let send = json!({ "envelope": env }).to_string();
    assert_eq!(
        decode_request("Send", send.as_bytes()).unwrap_err().code(),
        "INVALID_ENVELOPE"
    );

    // payload_b64 is always permitted, and the output side picks it.
    let mut b64 = env.clone();
    b64.as_object_mut().unwrap().remove("payload");
    b64["payload_b64"] = json!("CgF4");
    let decoded: Envelope = envelope_from_json(&b64).unwrap();
    assert_eq!(decoded.payload, b"\x0a\x01x");
    assert_eq!(envelope_to_json(&decoded)["payload_b64"], json!("CgF4"));
    // Core types still select their Core payload under the ext mode.
    let mut start = env.clone();
    start["message_type"] = json!("SessionStart");
    start["payload"] = json!({"intent": "i", "ttl_ms": 1000});
    assert!(envelope_from_json(&start).is_ok());
}

#[test]
fn failure_paths() {
    // Non-object payload, both forms, neither form.
    for (extra, why) in [
        (json!({"payload": "text"}), "non-object payload"),
        (json!({"payload": [1]}), "array payload"),
        (json!({"payload": {}, "payload_b64": ""}), "both forms"),
        (json!({}), "neither form"),
    ] {
        let mut env =
            json!({"mode": "macp.mode.decision.v1", "message_type": "Vote", "message_id": "m"});
        env.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let err = envelope_from_json(&env).unwrap_err();
        assert_eq!(err.code(), "INVALID_ENVELOPE", "{why}: {err}");
    }
    // A wrong type inside the payload is in band as well.
    let env = json!({"mode": "macp.mode.decision.v1", "message_type": "Evaluation",
                     "payload": {"confidence": {}}});
    assert_eq!(
        envelope_from_json(&env).unwrap_err().code(),
        "INVALID_ENVELOPE"
    );
    // A wrong type in the Envelope's own fields is a request-level error.
    let env = json!({"mode": 1, "payload_b64": ""});
    assert_eq!(
        envelope_from_json(&env).unwrap_err().code(),
        "INVALID_REQUEST"
    );
    // Not JSON, or not an object.
    assert_eq!(
        decode_request("GetSession", b"nope").unwrap_err().code(),
        "INVALID_REQUEST"
    );
    assert_eq!(
        decode_request("GetSession", b"\"s\"").unwrap_err().code(),
        "INVALID_REQUEST"
    );
    assert!(matches!(
        decode_request("Nope", b"{}"),
        Err(JsonError::UnknownMethod(_))
    ));
    // Duplicate keys: serde_json keeps the last (documented behaviour).
    let m = decode_request("GetSession", br#"{"session_id":"a","session_id":"b"}"#).unwrap();
    assert_eq!(get(&m, "session_id"), PbValue::String("b".into()));
}

#[test]
fn every_service_message_round_trips_through_generated_types() {
    // The HTTP handlers transcode DynamicMessage to the generated prost types;
    // a sample: Send's request and the Envelope.
    let req = decode_request(
        "Send",
        serde_json::to_vec(&json!({"envelope": maximal(&msg("macp.v1.Envelope"), true, None, 0)}))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let send: pb::SendRequest = req.transcode_to().unwrap();
    let env = send.envelope.unwrap();
    assert_eq!(env.timestamp_unix_ms, 1_772_396_710_250);
    assert_eq!(env.message_type, "Vote");
    assert_eq!(
        decision_pb::VotePayload::decode(env.payload.as_slice())
            .unwrap()
            .reason,
        "Within budget ✓"
    );
}
