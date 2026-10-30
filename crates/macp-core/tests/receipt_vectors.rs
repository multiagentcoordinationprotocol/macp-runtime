//! The spec's 12 receipt vectors (RFC-MACP-0013 Appendix A.4), vendored
//! byte-identical under the repo's `tests/vectors/receipt/` (see SOURCE.md
//! there), against `macp_core::receipt`: every vector verifies as pinned, and
//! `receipt_010_wire_match` is reproduced byte for byte by `build`.

use std::path::{Path, PathBuf};

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use macp_core::receipt::{
    build, jcs, jws_header, verify, KeySet, ReceiptInputs, ReceiptSigner, TrustedKeys, Verdict,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn vector_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/receipt")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn example_signer(label: &str, kid: &str) -> ReceiptSigner {
    let seed: [u8; 32] = Sha256::digest(label.as_bytes()).into();
    ReceiptSigner::from_seed(&seed, kid).unwrap()
}

#[test]
fn every_receipt_vector_verifies_as_pinned() {
    let mut paths: Vec<_> = std::fs::read_dir(vector_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "json")
                && p.file_name().unwrap() != "vector-schema.json"
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 12, "the spec pins 12 receipt vectors");

    for path in paths {
        let v = read_json(&path);
        let name = v["name"].as_str().unwrap();
        assert_eq!(path.file_stem().unwrap(), name, "name matches the file");
        let trusted: TrustedKeys = serde_json::from_value(v["trusted_keys"].clone()).unwrap();
        let got = serde_json::to_value(verify(&v["receipt"], &trusted)).unwrap();
        assert_eq!(got, v["expected"], "{name}: {}", v["description"]);
    }
}

#[test]
fn trusting_no_key_leaves_a_signed_receipt_integrity_only() {
    let v = read_json(&vector_dir().join("receipt_001_attested.json"));
    let got = serde_json::to_value(verify(&v["receipt"], &TrustedKeys::new())).unwrap();
    assert_eq!(
        got,
        serde_json::json!({"integrity": "match", "attestations": ["unknown_key"], "verdict": "integrity_only"})
    );
}

#[test]
fn build_reproduces_receipt_010_byte_for_byte() {
    let v = read_json(&vector_dir().join("receipt_010_wire_match.json"));
    let want = &v["receipt"];
    let wire = STANDARD
        .decode(want["commitment_wire_b64"].as_str().unwrap())
        .unwrap();
    let signer = example_signer("macp-receipt example key", "example-1");
    assert_eq!(
        signer.public_jwk()["x"],
        v["trusted_keys"]["example-1"],
        "the example key is the vector's"
    );
    let inputs = ReceiptInputs::new(
        want["session_id"].as_str().unwrap(),
        want["mode"].as_str().unwrap(),
        &wire,
        "runtime:example",
        "2026-10-24T12:00:00Z",
    )
    .semantics_rev("macp-runtime", 4);
    let bytes = build(&inputs, Some(&signer)).unwrap();
    let got: Value = serde_json::from_slice(&bytes).unwrap();

    // The emitted text is the JCS of the whole receipt.
    assert_eq!(jcs::canonicalize(&got).unwrap(), bytes);
    // Body (everything A.2 signs) matches the vector's, canonically.
    let body = |r: &Value| {
        let mut r = r.as_object().unwrap().clone();
        r.remove("attestations");
        jcs::canonicalize(&Value::Object(r)).unwrap()
    };
    assert_eq!(body(&got), body(want));
    // Ed25519 is deterministic: the JWS string is the vector's.
    assert_eq!(got["attestations"], want["attestations"]);
    assert_eq!(got["commitment_hash"], want["commitment_hash"]);
    let trusted: TrustedKeys = serde_json::from_value(v["trusted_keys"].clone()).unwrap();
    assert_eq!(verify(&got, &trusted).verdict, Verdict::Attested);
}

#[test]
fn the_protected_header_is_exactly_alg_then_kid() {
    assert_eq!(
        URL_SAFE_NO_PAD.encode(jws_header("example-1")),
        "eyJhbGciOiJFZERTQSIsImtpZCI6ImV4YW1wbGUtMSJ9"
    );
    assert_eq!(jws_header("a\"b"), r#"{"alg":"EdDSA","kid":"a\"b"}"#);
}

#[test]
fn an_unsigned_receipt_is_integrity_only_with_no_attestations() {
    let v = read_json(&vector_dir().join("receipt_010_wire_match.json"));
    let wire = STANDARD
        .decode(v["receipt"]["commitment_wire_b64"].as_str().unwrap())
        .unwrap();
    let bytes = build(
        &ReceiptInputs::new(
            "s",
            "macp.mode.decision.v1",
            &wire,
            "runtime:x",
            "2026-10-24T12:00:00Z",
        ),
        None,
    )
    .unwrap();
    let got: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(got["attestations"], serde_json::json!([]));
    assert!(got.get("transcript").is_none());
    assert!(got.get("semantics_rev").is_none());
    assert_eq!(
        verify(&got, &TrustedKeys::new()).verdict,
        Verdict::IntegrityOnly
    );
}

#[test]
fn a_payload_outside_the_hash_label_gets_no_receipt() {
    // Field 10 (varint) is outside macp-commitment-hash/1's frozen set.
    let wire = [0x50, 0x01];
    assert!(build(
        &ReceiptInputs::new("s", "m", &wire, "runtime:x", "2026-10-24T12:00:00Z"),
        None
    )
    .is_err());
    // A passthrough-style JSON resolution is not a CommitmentPayload either.
    assert!(build(
        &ReceiptInputs::new("s", "m", br#"{"action":"x"}"#, "runtime:x", "t"),
        None
    )
    .is_err());
}

#[test]
fn a_pkcs8_pem_key_loads_and_signs() {
    let pem = std::fs::read_to_string(fixtures().join("test-receipt-key.pem")).unwrap();
    assert!(
        ReceiptSigner::load(&pem, None).is_err(),
        "a PEM needs a kid"
    );
    let signer = ReceiptSigner::load(&pem, Some("pem-1")).unwrap();
    let v = read_json(&vector_dir().join("receipt_010_wire_match.json"));
    let wire = STANDARD
        .decode(v["receipt"]["commitment_wire_b64"].as_str().unwrap())
        .unwrap();
    let got: Value = serde_json::from_slice(
        &build(
            &ReceiptInputs::new(
                "s",
                "macp.mode.decision.v1",
                &wire,
                "runtime:x",
                "2026-10-24T12:00:00Z",
            ),
            Some(&signer),
        )
        .unwrap(),
    )
    .unwrap();
    let mut trusted = TrustedKeys::new();
    macp_core::receipt::add_jwks(
        &KeySet::new().with_active(&signer).unwrap().to_jwks(),
        &mut trusted,
    )
    .unwrap();
    assert_eq!(verify(&got, &trusted).verdict, Verdict::Attested);
}

#[test]
fn the_spec_invalid_key_sets_are_refused() {
    let dir = fixtures().join("invalid-receipt-keys");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let doc = read_json(&path);
        assert!(
            KeySet::retired_from_jwks(&doc).is_err(),
            "{} must be refused: {}",
            path.display(),
            doc["_invalid_because"]
        );
        n += 1;
    }
    assert_eq!(n, 8);
    // The spec's valid example key set loads.
    let ok = serde_json::json!({"keys": [example_signer("macp-receipt example key", "example-1").public_jwk()]});
    assert_eq!(KeySet::retired_from_jwks(&ok).unwrap().len(), 1);
}
