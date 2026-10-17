//! RFC-MACP-0013 commitment-hash conformance runner.
//!
//! Reads every vector file under `tests/conformance/cmt-hash/` (except
//! `vector-schema.json`). The schema has two shapes:
//!
//! - a **compute** vector (§4) — builds the `macp_pb::pb::CommitmentPayload`,
//!   calls the real `macp_core::commitment_hash::commitment_hash`, asserts the
//!   pinned `hash`, and asserts that verifying that hash against the payload's
//!   wire encoding is *match* (the schema's stated self-check);
//! - a **verify** vector (§6) — calls the real
//!   `macp_core::commitment_hash::verify_commitment_hash` on `wire_hex` and
//!   `claimed_hash` and asserts the pinned `expected_outcome`.
//!
//! This is a small, purpose-built runner -- deliberately separate from
//! `tests/conformance_loader.rs`, which loads session-transcript fixtures
//! (a different shape) via its own macro/loader. Hash vectors are not
//! session transcripts and are not taught to that loader.

use macp_core::commitment_hash::{verify_commitment_hash, VerifyOutcome};
use macp_pb::pb::{CommitmentPayload, CommitmentRef};
use prost::Message;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Both vector shapes; `vector-schema.json`'s `oneOf` makes them disjoint,
/// and [`Vector::kind`] refuses a file that is neither or both.
#[derive(Debug, Clone, Deserialize)]
struct Vector {
    name: String,
    // Compute vectors.
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    payload: Option<VectorPayload>,
    #[serde(default)]
    must_differ_from: Option<String>,
    // Verify vectors.
    #[serde(default)]
    wire_hex: Option<String>,
    #[serde(default)]
    claimed_hash: Option<String>,
    #[serde(default)]
    expected_outcome: Option<String>,
}

enum Kind<'a> {
    Compute {
        hash: &'a str,
        payload: &'a VectorPayload,
    },
    Verify {
        wire: Vec<u8>,
        claimed: &'a str,
        expected: &'a str,
    },
}

impl Vector {
    fn kind(&self) -> Kind<'_> {
        match (
            &self.hash,
            &self.payload,
            &self.wire_hex,
            &self.claimed_hash,
            &self.expected_outcome,
        ) {
            (Some(hash), Some(payload), None, None, None) => Kind::Compute { hash, payload },
            (None, None, Some(wire), Some(claimed), Some(expected)) => Kind::Verify {
                wire: hex(wire),
                claimed,
                expected,
            },
            _ => panic!(
                "vector {} is neither a compute nor a verify vector",
                self.name
            ),
        }
    }
}

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd-length hex {s:?}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("lowercase hex"))
        .collect()
}

/// The schema's `expected_outcome` enum, from the real verifier's outcome.
fn outcome_name(outcome: &VerifyOutcome) -> &'static str {
    match outcome {
        VerifyOutcome::Match => "match",
        VerifyOutcome::Mismatch { .. } => "mismatch",
        VerifyOutcome::CannotVerify(_) => "cannot_verify",
        // `#[non_exhaustive]`: an outcome the schema has no name for can
        // never equal an `expected_outcome`, so it fails the assertion.
        _ => "<outside the schema enum>",
    }
}

#[derive(Debug, Clone, Deserialize)]
struct VectorPayload {
    commitment_id: String,
    action: String,
    authority_scope: String,
    reason: String,
    mode_version: String,
    policy_version: String,
    configuration_version: String,
    outcome_positive: bool,
    // Critical: must be `Option`, not a defaulted-present struct, so that a
    // JSON payload with no "supersedes" key deserializes to `None` (RFC-MACP-0013
    // §3 rule 3's unset-vs-empty distinction), while a payload with
    // "supersedes": {"session_id": "", "commitment_hash": ""} deserializes to
    // `Some(VectorSupersedes { .. })` with empty strings inside. Vector 003
    // (no key) vs vector 004 (key present, both sub-fields "") is exactly the
    // pair this distinction has to get right.
    #[serde(default)]
    supersedes: Option<VectorSupersedes>,
}

#[derive(Debug, Clone, Deserialize)]
struct VectorSupersedes {
    session_id: String,
    commitment_hash: String,
}

impl From<VectorPayload> for CommitmentPayload {
    fn from(v: VectorPayload) -> Self {
        CommitmentPayload {
            commitment_id: v.commitment_id,
            action: v.action,
            authority_scope: v.authority_scope,
            reason: v.reason,
            mode_version: v.mode_version,
            policy_version: v.policy_version,
            configuration_version: v.configuration_version,
            outcome_positive: v.outcome_positive,
            supersedes: v.supersedes.map(|s| CommitmentRef {
                session_id: s.session_id,
                commitment_hash: s.commitment_hash,
            }),
        }
    }
}

fn vectors_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance/cmt-hash")
}

fn load_vectors() -> Vec<Vector> {
    let dir = vectors_dir();
    let mut vectors = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("vector-schema.json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let vector: Vector = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
        vectors.push(vector);
    }
    vectors
}

/// Spec `59ae57d`: 001–005 compute, 006–010 verify. A count change means the
/// vendored set moved — re-check this runner before bumping these.
const COMPUTE_VECTORS: usize = 5;
const VERIFY_VECTORS: usize = 5;

#[test]
fn cmt_hash_vectors_match_real_implementation() {
    let vectors = load_vectors();
    assert_eq!(
        vectors.len(),
        COMPUTE_VECTORS + VERIFY_VECTORS,
        "expected exactly {} commitment-hash conformance vectors, found {}",
        COMPUTE_VECTORS + VERIFY_VECTORS,
        vectors.len()
    );

    let hashes_by_name: HashMap<String, String> = vectors
        .iter()
        .filter_map(|v| Some((v.name.clone(), v.hash.clone()?)))
        .collect();

    let (mut computed_n, mut verified_n) = (0usize, 0usize);
    for vector in &vectors {
        match vector.kind() {
            Kind::Compute { hash, payload } => {
                let payload: CommitmentPayload = payload.clone().into();
                let computed = macp_core::commitment_hash::commitment_hash(&payload);
                assert_eq!(
                    computed, hash,
                    "vector {} hash mismatch: computed {computed}, expected {hash}",
                    vector.name
                );
                let outcome = verify_commitment_hash(&payload.encode_to_vec(), hash);
                assert!(
                    outcome.is_match(),
                    "vector {}: its own hash must verify as match, got {outcome:?}",
                    vector.name
                );
                if let Some(other_name) = &vector.must_differ_from {
                    let other_hash = hashes_by_name.get(other_name).unwrap_or_else(|| {
                        panic!("must_differ_from target {other_name:?} not found")
                    });
                    assert_ne!(
                        hash, other_hash,
                        "vector {} must_differ_from {} but hashes are equal",
                        vector.name, other_name
                    );
                }
                computed_n += 1;
            }
            Kind::Verify {
                wire,
                claimed,
                expected,
            } => {
                let outcome = verify_commitment_hash(&wire, claimed);
                assert_eq!(
                    outcome_name(&outcome),
                    expected,
                    "vector {}: verifier returned {outcome:?}",
                    vector.name
                );
                verified_n += 1;
            }
        }
    }

    eprintln!("cmt_hash_vectors: executed {computed_n} compute + {verified_n} verify vectors");
    assert_eq!((computed_n, verified_n), (COMPUTE_VECTORS, VERIFY_VECTORS));
}

/// Explicit deserialization-shape check (see the comment on
/// `VectorPayload::supersedes` above): vector 003 has no `supersedes` key at
/// all and must deserialize to `None`; vector 004 has `supersedes` present
/// with both sub-fields `""` and must deserialize to `Some(..)` with empty
/// strings inside. A deserialization bug that coerces both to the same shape
/// could otherwise hide behind two lucky-matching final hashes.
#[test]
fn supersedes_option_is_none_vs_some_not_collapsed() {
    let dir = vectors_dir();

    let raw_003 = std::fs::read_to_string(dir.join("cmt_hash_003_all_empty.json")).unwrap();
    let vector_003: Vector = serde_json::from_str(&raw_003).unwrap();
    assert!(
        vector_003.payload.as_ref().unwrap().supersedes.is_none(),
        "cmt_hash_003_all_empty: expected supersedes to deserialize to None (key absent), got {:?}",
        vector_003.payload.as_ref().unwrap().supersedes
    );

    let raw_004 = std::fs::read_to_string(dir.join("cmt_hash_004_empty_supersedes.json")).unwrap();
    let vector_004: Vector = serde_json::from_str(&raw_004).unwrap();
    match &vector_004.payload.as_ref().unwrap().supersedes {
        Some(s) => {
            assert_eq!(s.session_id, "");
            assert_eq!(s.commitment_hash, "");
        }
        None => panic!(
            "cmt_hash_004_empty_supersedes: expected supersedes to deserialize to Some(..) (key present with empty sub-fields), got None"
        ),
    }

    // And, as a sanity check that this distinction actually matters end to end:
    // the two vectors' pinned hashes must differ.
    assert_ne!(vector_003.hash, vector_004.hash);
}
