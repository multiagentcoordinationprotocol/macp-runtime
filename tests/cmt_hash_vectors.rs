//! RFC-MACP-0013 commitment-hash conformance runner.
//!
//! Reads every vector file under `tests/conformance/cmt-hash/` (except
//! `vector-schema.json`), builds the corresponding
//! `macp_pb::pb::CommitmentPayload`, calls the real
//! `macp_core::commitment_hash::commitment_hash`, and asserts the result
//! equals the vector's pinned `hash` field.
//!
//! This is a small, purpose-built runner -- deliberately separate from
//! `tests/conformance_loader.rs`, which loads session-transcript fixtures
//! (a different shape) via its own macro/loader. Hash vectors are not
//! session transcripts and are not taught to that loader.

use macp_pb::pb::{CommitmentPayload, CommitmentRef};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
struct Vector {
    name: String,
    hash: String,
    payload: VectorPayload,
    #[serde(default)]
    must_differ_from: Option<String>,
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

#[test]
fn cmt_hash_vectors_match_real_implementation() {
    let vectors = load_vectors();
    assert_eq!(
        vectors.len(),
        5,
        "expected exactly 5 commitment-hash conformance vectors, found {}",
        vectors.len()
    );

    let hashes_by_name: HashMap<String, String> = vectors
        .iter()
        .map(|v| (v.name.clone(), v.hash.clone()))
        .collect();

    let mut executed = 0usize;
    for vector in &vectors {
        let payload: CommitmentPayload = vector.payload.clone().into();

        let computed = macp_core::commitment_hash::commitment_hash(&payload);
        assert_eq!(
            computed, vector.hash,
            "vector {} hash mismatch: computed {computed}, expected {}",
            vector.name, vector.hash
        );
        executed += 1;

        if let Some(other_name) = &vector.must_differ_from {
            let other_hash = hashes_by_name
                .get(other_name)
                .unwrap_or_else(|| panic!("must_differ_from target {other_name:?} not found"));
            assert_ne!(
                &vector.hash, other_hash,
                "vector {} must_differ_from {} but hashes are equal",
                vector.name, other_name
            );
        }
    }

    eprintln!("cmt_hash_vectors: executed {executed} vectors");
    assert_eq!(executed, 5);
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
        vector_003.payload.supersedes.is_none(),
        "cmt_hash_003_all_empty: expected supersedes to deserialize to None (key absent), got {:?}",
        vector_003.payload.supersedes
    );

    let raw_004 = std::fs::read_to_string(dir.join("cmt_hash_004_empty_supersedes.json")).unwrap();
    let vector_004: Vector = serde_json::from_str(&raw_004).unwrap();
    match &vector_004.payload.supersedes {
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
