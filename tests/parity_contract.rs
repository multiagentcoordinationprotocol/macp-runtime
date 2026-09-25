//! Proof-of-concept consumer of `schemas/parity/contract.json` (spec issue
//! #134, macp-runtime issue #176).
//!
//! `schemas/parity/contract.json` is a small, non-normative manifest in the
//! spec repo pinning cross-implementation-agreed values (protocol version,
//! mode-id sets, defaults, error codes, commitment-hash format, Contribute
//! payload encoding) across macp-runtime, macp-sdk-python, and
//! macp-sdk-typescript. `schemas/parity/README.md`'s Versioning section makes
//! `applies_to` a MUST-assert list: a consumer named there is expected to
//! assert every field the section defines, against its own real code -- not
//! reimplement the value, and not merely echo it back.
//!
//! This runner asserts every section naming `"macp-runtime"` in `applies_to`
//! (`protocol`, `modes`, `defaults`, `error_codes`, `commitment_hash`,
//! `contribute_payload`, `contribute_acceptance` -- see `HANDLED` below)
//! against this runtime's real, live code: `macp_core::MACP_VERSION`, the
//! mode-registry constants, the `#[doc(hidden)]` predicates
//! `is_canonical_commitment_hash`/`parse_contribute_value`, `MacpError::
//! error_code()`, and `PolicyRegistry::register`. `retry` and
//! `projection_anomaly` are SDK-only sections (absent from `applies_to` here)
//! and are deliberately not asserted.
//!
//! The manifest file loaded is `tests/parity/contract.json` by default (a
//! byte-identical vendored copy -- see `tests/parity/SOURCE.md`), or the path
//! named by `MACP_PARITY_CONTRACT` when set and non-empty (CI's
//! `conformance-oracle` job points this at the spec-repo checkout's own copy,
//! proving the runtime matches canonical directly, not merely the vendored
//! copy).
//!
//! **Explicitly out of scope:** validating `contract.json`'s own *shape*
//! against `schemas/json/macp-parity-contract.schema.json`. That is the spec
//! repo's `scripts/check-parity-contract.py` and its negative fixtures under
//! `schemas/json/tests/invalid-parity-contract/` -- this runner's job is
//! asserting the manifest's *content* against macp-runtime's real behavior,
//! not re-validating the manifest's JSON shape.

use macp_core::error::MacpError;
use macp_core::session::{
    CURRENT_SEMANTICS_REV, DEFAULT_CONFIGURATION_VERSION, DEFAULT_MODE_VERSION,
};
use macp_core::MACP_VERSION;
use macp_modes::mode::multi_round::parse_contribute_value;
use macp_modes::mode::util::is_canonical_commitment_hash;
use macp_modes::mode::{EXTENSION_MODE_NAMES, STANDARD_MODE_NAMES};
use macp_policy::defaults::DEFAULT_POLICY_ID;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Sections this repo asserts against real code -- every section whose
/// `applies_to` names `"macp-runtime"` must appear here (checked by
/// `every_macp_runtime_section_is_handled`), and every entry here must still
/// exist as a section (checked by `every_handled_section_still_exists`).
const HANDLED: &[&str] = &[
    "protocol",
    "modes",
    "defaults",
    "error_codes",
    "commitment_hash",
    "contribute_payload",
    "contribute_acceptance",
];

/// Every section name the canonical schema currently defines -- `HANDLED`'s
/// seven macp-runtime-relevant entries plus the two SDK-only sections
/// (`retry`, `projection_anomaly`). Used only for the sections-map
/// key-allowlist check in `root_and_sections_have_no_unexpected_keys`; the
/// coverage guards above use `HANDLED`, not this. Conflating the two would
/// make the coverage guard reject the real, correctly-vendored manifest for
/// containing SDK-only sections that legitimately don't name macp-runtime.
const ALL_SECTIONS: &[&str] = &[
    "protocol",
    "modes",
    "defaults",
    "error_codes",
    "commitment_hash",
    "contribute_payload",
    "contribute_acceptance",
    "retry",
    "projection_anomaly",
];

/// The parity-contract manifest to load. Defaults to the vendored copy in
/// `tests/parity/contract.json`; the CI oracle job overrides it with
/// `MACP_PARITY_CONTRACT` (a *file* path, unlike `MACP_CONFORMANCE_FIXTURES_DIR`
/// in `tests/conformance_loader.rs`, which is a directory) to run this same
/// suite against the spec repo's canonical `schemas/parity/contract.json`.
/// Phase 3's `check_dir` byte-compares the vendored copy against canonical, so
/// the two locations cannot drift silently.
fn parity_contract_path() -> PathBuf {
    match std::env::var("MACP_PARITY_CONTRACT") {
        Ok(path) if !path.is_empty() => PathBuf::from(path),
        _ => Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/parity/contract.json"),
    }
}

fn load_contract() -> Value {
    let path = parity_contract_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn section<'a>(contract: &'a Value, name: &str) -> &'a Value {
    &contract["sections"][name]
}

fn str_array<'a>(value: &'a Value, key: &str) -> Vec<&'a str> {
    value[key]
        .as_array()
        .unwrap_or_else(|| panic!("'{key}' must be an array"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("'{key}' entries must be strings"))
        })
        .collect()
}

fn applies_to_macp_runtime(section: &Value) -> bool {
    str_array(section, "applies_to").contains(&"macp-runtime")
}

/// The canonical schema (`schemas/json/macp-parity-contract.schema.json`)
/// gives every object -- root, the `sections` map, and every individual
/// section -- `"additionalProperties": false` paired with
/// `"patternProperties": {"^(_|\\$comment)": {}}`. Serde's
/// `deny_unknown_fields` can't express "unknown key rejected unless it
/// matches this pattern," so the check is manual: a plain prefix check, not a
/// regex -- the upstream pattern is exactly this prefix rule and nothing
/// more.
fn check_no_unexpected_keys(obj: &Map<String, Value>, allowed: &[&str], context: &str) {
    for key in obj.keys() {
        let ok =
            allowed.contains(&key.as_str()) || key.starts_with('_') || key.starts_with("$comment");
        assert!(
            ok,
            "unexpected key '{key}' in {context} (allowed: {allowed:?}, plus keys starting \
             with '_' or '$comment')"
        );
    }
}

// ─── Structural guards ──────────────────────────────────────────────────

#[test]
fn root_and_sections_have_no_unexpected_keys() {
    let contract = load_contract();
    check_no_unexpected_keys(
        contract
            .as_object()
            .expect("contract root must be an object"),
        &["contract_version", "sections"],
        "contract root",
    );
    check_no_unexpected_keys(
        contract["sections"]
            .as_object()
            .expect("sections must be an object"),
        ALL_SECTIONS,
        "sections map",
    );
}

#[test]
fn contract_version_is_1_x() {
    let contract = load_contract();
    let version = contract["contract_version"]
        .as_str()
        .expect("contract_version must be a string");
    assert!(
        version.starts_with("1."),
        "contract_version {version:?} does not start with '1.'"
    );
}

#[test]
fn every_macp_runtime_section_is_handled() {
    let contract = load_contract();
    let sections_map = contract["sections"]
        .as_object()
        .expect("sections must be an object");
    for name in sections_map.keys() {
        let sec = section(&contract, name);
        if applies_to_macp_runtime(sec) {
            assert!(
                HANDLED.contains(&name.as_str()),
                "section '{name}' names macp-runtime in applies_to but has no matching \
                 assertion in tests/parity_contract.rs's HANDLED list"
            );
        }
    }
}

#[test]
fn every_handled_section_still_exists() {
    let contract = load_contract();
    let sections_map = contract["sections"]
        .as_object()
        .expect("sections must be an object");
    for name in HANDLED {
        assert!(
            sections_map.contains_key(*name),
            "HANDLED names section '{name}' but it no longer exists in the manifest"
        );
    }
}

// ─── protocol ───────────────────────────────────────────────────────────

#[test]
fn protocol_macp_version_matches_runtime() {
    let contract = load_contract();
    let protocol = section(&contract, "protocol");
    assert_eq!(
        protocol["macp_version"].as_str().unwrap(),
        MACP_VERSION,
        "manifest's protocol.macp_version does not match macp_core::MACP_VERSION"
    );
}

// ─── modes ──────────────────────────────────────────────────────────────

#[test]
fn modes_standard_and_extension_match_runtime() {
    let contract = load_contract();
    let modes = section(&contract, "modes");

    let standard: HashSet<&str> = str_array(modes, "standard").into_iter().collect();
    let expected_standard: HashSet<&str> = STANDARD_MODE_NAMES.iter().copied().collect();
    assert_eq!(
        standard, expected_standard,
        "manifest's modes.standard does not match macp_modes::mode::STANDARD_MODE_NAMES"
    );

    let extension: HashSet<&str> = str_array(modes, "extension").into_iter().collect();
    let expected_extension: HashSet<&str> = EXTENSION_MODE_NAMES.iter().copied().collect();
    assert_eq!(
        extension, expected_extension,
        "manifest's modes.extension does not match macp_modes::mode::EXTENSION_MODE_NAMES"
    );
}

// ─── defaults ───────────────────────────────────────────────────────────

#[test]
fn defaults_mode_and_configuration_version_match_runtime() {
    let contract = load_contract();
    let defaults = section(&contract, "defaults");
    assert_eq!(
        defaults["mode_version"].as_str().unwrap(),
        DEFAULT_MODE_VERSION
    );
    assert_eq!(
        defaults["configuration_version"].as_str().unwrap(),
        DEFAULT_CONFIGURATION_VERSION
    );
    assert_eq!(
        defaults["policy_version"].as_str().unwrap(),
        DEFAULT_POLICY_ID
    );
}

#[test]
fn defaults_policy_builder_schema_version_matches_documented_value() {
    let contract = load_contract();
    let defaults = section(&contract, "defaults");
    let version = defaults["policy_builder_schema_version"]
        .as_u64()
        .expect("policy_builder_schema_version must be an integer");
    // RFC-MACP-0012 Section 3's SHOULD-recommendation that new policies
    // declare schema_version 3 -- documentation-grade pin, kept alongside
    // the real behavioral assertion below since a literal-to-literal
    // comparison alone has zero detection power on its own.
    assert_eq!(version, 3);
}

#[test]
fn defaults_policy_builder_schema_version_is_accepted_by_the_registry() {
    let contract = load_contract();
    let defaults = section(&contract, "defaults");
    let version = defaults["policy_builder_schema_version"]
        .as_u64()
        .expect("policy_builder_schema_version must be an integer") as u32;

    // A newly authored policy declaring the manifest's recommended
    // schema_version must be accepted by the real registry -- this is the
    // recommendation's actual claim, not just literal equality against a
    // hardcoded `3`.
    let mut probe = macp_policy::defaults::default_policy();
    probe.policy_id = "test.parity-schema-version-probe".to_string();
    probe.schema_version = version;

    let registry = macp_policy::registry::PolicyRegistry::new();
    registry.register(probe).unwrap_or_else(|e| {
        panic!(
            "PolicyRegistry rejected a policy declaring policy_builder_schema_version \
             {version}: {e}"
        )
    });
}

// ─── error_codes ────────────────────────────────────────────────────────

#[test]
fn error_codes_permanent_set_matches_runtime() {
    let contract = load_contract();
    let error_codes = section(&contract, "error_codes");
    let permanent: HashSet<&str> = str_array(error_codes, "permanent").into_iter().collect();
    let deprecated: HashSet<&str> = str_array(error_codes, "deprecated").into_iter().collect();

    // One constructed value per variant, mirroring
    // crates/macp-core/src/error.rs's own error_code_mapping_covers_all_variants
    // test (:86-130) -- neither list is compiler-enforced exhaustive since
    // MacpError is #[non_exhaustive], so a reviewer adding a variant must
    // update both places.
    let variants: Vec<MacpError> = vec![
        MacpError::InvalidMacpVersion,
        MacpError::InvalidEnvelope,
        MacpError::SessionAlreadyExists,
        MacpError::UnknownSession,
        MacpError::SessionNotOpen,
        MacpError::TtlExpired,
        MacpError::InvalidTtl,
        MacpError::UnknownMode,
        MacpError::InvalidModeState,
        MacpError::InvalidPayload,
        MacpError::Forbidden,
        MacpError::Unauthenticated,
        MacpError::DuplicateMessage,
        MacpError::PayloadTooLarge,
        MacpError::RateLimited,
        MacpError::StorageFailed,
        MacpError::InvalidSessionId,
        MacpError::UnknownPolicyVersion,
        MacpError::PolicyDenied {
            reasons: vec!["parity_contract probe".to_string()],
        },
        MacpError::InvalidPolicyDefinition,
    ];
    let produced: HashSet<&str> = variants.iter().map(|e| e.error_code()).collect();

    assert_eq!(
        produced, permanent,
        "runtime-produced error codes do not match the manifest's error_codes.permanent set"
    );
    for code in &deprecated {
        assert!(
            !produced.contains(code),
            "deprecated code '{code}' must not be produced by any current MacpError variant"
        );
    }
}

// ─── commitment_hash ────────────────────────────────────────────────────

#[test]
fn commitment_hash_accept_reject_vectors_match_runtime() {
    let contract = load_contract();
    let sec = section(&contract, "commitment_hash");
    let accept = str_array(sec, "accept");
    let reject = str_array(sec, "reject");

    assert!(!accept.is_empty());
    for s in &accept {
        assert!(
            is_canonical_commitment_hash(s),
            "expected accept, is_canonical_commitment_hash rejected {s:?}"
        );
    }

    assert!(!reject.is_empty());
    for s in &reject {
        assert!(
            !is_canonical_commitment_hash(s),
            "expected reject, is_canonical_commitment_hash accepted {s:?}"
        );
    }
}

// ─── contribute_payload / contribute_acceptance ────────────────────────

#[derive(Debug, Clone, Deserialize)]
struct ContributeVector {
    name: String,
    value: String,
    protobuf_hex: String,
    #[serde(default)]
    legacy_json_hex: Option<String>,
    #[serde(default)]
    decode_only: bool,
}

fn contribute_vectors(contract: &Value) -> Vec<ContributeVector> {
    let sec = section(contract, "contribute_payload");
    serde_json::from_value(sec["vectors"].clone())
        .unwrap_or_else(|e| panic!("parsing contribute_payload.vectors: {e}"))
}

/// Hand-rolled -- no `hex` crate dependency exists anywhere in this
/// workspace's `Cargo.lock` today, and adding one would trigger
/// `integration_tests/Cargo.lock`'s two-lockfile regeneration cost
/// (CLAUDE.md) for a ~10-line prefix/nibble decode. `context` names the
/// vector on a panic, since this is the one place a bug could otherwise
/// surface as a bare, unattributed panic.
fn hex_decode(s: &str, context: &str) -> Vec<u8> {
    assert!(
        s.len().is_multiple_of(2),
        "hex_decode({context}): odd-length hex string {s:?}"
    );
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16).unwrap_or_else(|| {
            panic!(
                "hex_decode({context}): non-hex character {:?} in {s:?}",
                pair[0] as char
            )
        });
        let lo = (pair[1] as char).to_digit(16).unwrap_or_else(|| {
            panic!(
                "hex_decode({context}): non-hex character {:?} in {s:?}",
                pair[1] as char
            )
        });
        out.push(((hi << 4) | lo) as u8);
    }
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn hex_helpers_self_test() {
    assert_eq!(hex_encode(&[0x0a, 0xff]), "0aff");
    assert_eq!(hex_decode("0aff", "self-test"), vec![0x0a, 0xff]);
}

#[test]
#[should_panic(expected = "odd-length")]
fn hex_decode_rejects_odd_length_input() {
    hex_decode("abc", "test");
}

#[test]
#[should_panic(expected = "non-hex character")]
fn hex_decode_rejects_non_hex_character() {
    hex_decode("zz", "test");
}

#[test]
fn contribute_payload_vectors_round_trip_through_the_real_codec() {
    let contract = load_contract();
    let vectors = contribute_vectors(&contract);
    assert_eq!(
        vectors.len(),
        4,
        "expected exactly 4 contribute_payload vectors, found {}",
        vectors.len()
    );

    for vector in &vectors {
        // Decode: protobuf_hex must decode, via the real parse_contribute_value,
        // to `value`.
        let protobuf_bytes = hex_decode(&vector.protobuf_hex, &vector.name);
        let decoded = parse_contribute_value(&protobuf_bytes, CURRENT_SEMANTICS_REV)
            .unwrap_or_else(|e| panic!("vector {}: protobuf decode failed: {e:?}", vector.name));
        assert_eq!(
            decoded, vector.value,
            "vector {}: protobuf decode mismatch",
            vector.name
        );

        if let Some(legacy_hex) = &vector.legacy_json_hex {
            let legacy_bytes = hex_decode(legacy_hex, &vector.name);
            let decoded_legacy = parse_contribute_value(&legacy_bytes, CURRENT_SEMANTICS_REV)
                .unwrap_or_else(|e| {
                    panic!("vector {}: legacy_json decode failed: {e:?}", vector.name)
                });
            assert_eq!(
                decoded_legacy, vector.value,
                "vector {}: legacy_json decode mismatch",
                vector.name
            );
        }

        if !vector.decode_only {
            // Round-trip: build the real ContributePayload, encode it via the
            // real prost-generated encoder, and assert a byte-exact match
            // against protobuf_hex.
            let payload = macp_pb::multi_round_pb::ContributePayload {
                value: vector.value.clone(),
            };
            let mut encoded = Vec::new();
            prost::Message::encode(&payload, &mut encoded)
                .unwrap_or_else(|e| panic!("vector {}: encode failed: {e}", vector.name));
            assert_eq!(
                hex_encode(&encoded),
                vector.protobuf_hex,
                "vector {}: encode round-trip mismatch",
                vector.name
            );
        }
    }
}

#[test]
fn contribute_payload_first_byte_markers_match_vectors() {
    let contract = load_contract();
    let sec = section(&contract, "contribute_payload");
    let protobuf_marker = sec["first_byte"]["protobuf"].as_str().unwrap();
    let legacy_marker = sec["first_byte"]["legacy_json"].as_str().unwrap();
    assert_eq!(protobuf_marker, "0x0a");
    assert_eq!(legacy_marker, "0x7b");

    for vector in contribute_vectors(&contract) {
        assert!(
            vector.protobuf_hex.starts_with("0a"),
            "vector {}: protobuf_hex does not start with the declared first byte 0x0a",
            vector.name
        );
        if let Some(legacy_hex) = &vector.legacy_json_hex {
            assert!(
                legacy_hex.starts_with("7b"),
                "vector {}: legacy_json_hex does not start with the declared first byte 0x7b",
                vector.name
            );
        }
    }
}

#[test]
fn contribute_acceptance_empty_payload_is_rejected() {
    let contract = load_contract();
    let sec = section(&contract, "contribute_acceptance");
    assert_eq!(sec["empty_payload"].as_str().unwrap(), "reject");

    let result = parse_contribute_value(&[], CURRENT_SEMANTICS_REV);
    assert!(
        result.is_err(),
        "parse_contribute_value(&[]) must be rejected per the manifest's \
         contribute_acceptance.empty_payload: reject"
    );
}
