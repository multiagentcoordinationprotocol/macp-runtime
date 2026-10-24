//! The spec's fixture formats, and loading them from a directory or a pinned
//! release bundle.
//!
//! Two shapes are loaded:
//!
//! - **session fixtures**, `schemas/conformance/*.json` (format:
//!   `schemas/conformance/schema.json` and `README.md` in the spec repo);
//! - **commitment-hash vectors**, `schemas/conformance/cmt-hash/*.json`
//!   (format: `cmt-hash/vector-schema.json`, RFC-MACP-0013).
//!
//! Fields this runner does not use (`description`, `_comment`, ...) are
//! ignored, so a newer spec that adds annotation fields still loads.

use crate::bundle;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One session fixture (a `schemas/conformance/*.json` file).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct Fixture {
    pub mode: String,
    pub initiator: String,
    pub participants: Vec<String>,
    pub mode_version: String,
    pub configuration_version: String,
    pub policy_version: String,
    /// Inline governance policy, registered before the fixture's
    /// `SessionStart` so a non-default `policy_version` resolves.
    #[serde(default)]
    pub policy: Option<Value>,
    pub ttl_ms: i64,
    pub messages: Vec<FixtureMessage>,
    pub expected_final_state: String,
    #[serde(default)]
    pub expected_mode_state: Option<Value>,
    #[serde(default)]
    pub expected_resolution: Option<Value>,
    #[serde(default)]
    pub expect_resolution_present: Option<bool>,
    #[serde(default = "default_true")]
    pub verify_replay_equivalence: bool,
    /// Exact assertion on accepted history after the `SessionStart`,
    /// synthetic entries included.
    #[serde(default)]
    pub expected_history: Option<Vec<ExpectedHistoryEntry>>,
}

impl Fixture {
    /// Whether the fixture drives the acceptance clock (`accepted_at_ms`).
    /// The field is all-or-none; [`Fixture::check_clock_shape`] enforces it.
    pub fn is_clocked(&self) -> bool {
        self.messages.iter().any(|m| m.accepted_at_ms.is_some())
    }

    /// `accepted_at_ms` must be on every message or on none, and
    /// non-decreasing. A fixture that breaks this is malformed.
    pub fn check_clock_shape(&self) -> Result<(), String> {
        let stepped = self
            .messages
            .iter()
            .filter(|m| m.accepted_at_ms.is_some())
            .count();
        if stepped != 0 && stepped != self.messages.len() {
            return Err("accepted_at_ms is all-or-none".into());
        }
        let mut last = 0;
        for m in &self.messages {
            if let Some(at) = m.accepted_at_ms {
                if at < last {
                    return Err("accepted_at_ms must be non-decreasing".into());
                }
                last = at;
            }
        }
        Ok(())
    }
}

/// One step of a fixture.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FixtureMessage {
    pub sender: String,
    pub message_type: String,
    /// Fully-qualified protobuf message name of the payload.
    pub payload_type: String,
    pub payload: Value,
    /// `accept` or `reject`.
    pub expect: String,
    #[serde(default)]
    pub expected_error_code: Option<String>,
    /// The implementation's acceptance clock for this step (RFC-MACP-0001
    /// §7.6 item 4), distinct from the Envelope timestamp.
    #[serde(default)]
    pub accepted_at_ms: Option<i64>,
    #[serde(default)]
    pub envelope: Option<EnvelopeOverride>,
}

/// Per-step Envelope overrides.
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct EnvelopeOverride {
    pub session_id: Option<String>,
    pub macp_version: Option<String>,
    pub mode: Option<String>,
    pub timestamp_unix_ms: Option<i64>,
}

/// One `expected_history` entry.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ExpectedHistoryEntry {
    pub message_type: String,
    pub sender: String,
    #[serde(default)]
    pub payload: Option<Value>,
    #[serde(default)]
    pub synthetic: bool,
}

fn default_true() -> bool {
    true
}

/// One commitment-hash vector. Exactly one of the two shapes is populated:
/// a **compute** vector (`hash` + `payload`) or a **verify** vector
/// (`wire_hex` + `claimed_hash` + `expected_outcome`).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CmtHashVector {
    pub name: String,
    #[serde(default)]
    pub hash: Option<String>,
    #[serde(default)]
    pub payload: Option<VectorPayload>,
    #[serde(default)]
    pub must_differ_from: Option<String>,
    #[serde(default)]
    pub wire_hex: Option<String>,
    #[serde(default)]
    pub claimed_hash: Option<String>,
    /// `match`, `mismatch` or `cannot_verify`.
    #[serde(default)]
    pub expected_outcome: Option<String>,
}

/// A compute vector's `CommitmentPayload`, as JSON.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct VectorPayload {
    pub commitment_id: String,
    pub action: String,
    pub authority_scope: String,
    pub reason: String,
    pub mode_version: String,
    pub policy_version: String,
    pub configuration_version: String,
    pub outcome_positive: bool,
    /// `None` when the key is absent; `Some` (possibly with empty strings)
    /// when present. RFC-MACP-0013 §3 rule 3 tells the two apart.
    #[serde(default)]
    pub supersedes: Option<VectorSupersedes>,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct VectorSupersedes {
    pub session_id: String,
    pub commitment_hash: String,
}

impl From<&VectorPayload> for macp_pb::pb::CommitmentPayload {
    fn from(v: &VectorPayload) -> Self {
        macp_pb::pb::CommitmentPayload {
            commitment_id: v.commitment_id.clone(),
            action: v.action.clone(),
            authority_scope: v.authority_scope.clone(),
            reason: v.reason.clone(),
            mode_version: v.mode_version.clone(),
            policy_version: v.policy_version.clone(),
            configuration_version: v.configuration_version.clone(),
            outcome_positive: v.outcome_positive,
            supersedes: v.supersedes.as_ref().map(|s| macp_pb::pb::CommitmentRef {
                session_id: s.session_id.clone(),
                commitment_hash: s.commitment_hash.clone(),
            }),
        }
    }
}

/// A fixture with its file name (e.g. `decision_happy_path.json`).
#[derive(Debug, Clone)]
pub struct NamedFixture {
    pub name: String,
    pub fixture: Fixture,
}

impl NamedFixture {
    /// Load one fixture file, named by its file name.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let path = path.as_ref();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| LoadError(format!("{} has no file name", path.display())))?
            .to_string();
        let fixture = serde_json::from_slice(&read(path)?)
            .map_err(|e| LoadError(format!("cannot parse fixture {name}: {e}")))?;
        Ok(Self { name, fixture })
    }
}

/// A vector with its file name (e.g. `cmt_hash_001_minimal.json`).
#[derive(Debug, Clone)]
pub struct NamedVector {
    pub name: String,
    pub vector: CmtHashVector,
}

/// Where a [`FixtureSet`] came from, recorded in the report.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FixtureSource {
    /// A `schemas/conformance/` directory.
    Directory { path: String },
    /// A release bundle, verified against its `SHA256SUMS` line (when given)
    /// and every file against its `MANIFEST.json` digest.
    Bundle {
        path: String,
        version: String,
        spec_commit: String,
        sha256: String,
        checksum_verified: bool,
    },
}

/// Every fixture and vector one run uses.
#[derive(Debug, Clone)]
pub struct FixtureSet {
    pub source: FixtureSource,
    /// Sorted by name.
    pub fixtures: Vec<NamedFixture>,
    /// Sorted by name.
    pub vectors: Vec<NamedVector>,
}

/// A fixture set could not be loaded. Always a harness error: nothing was run.
#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LoadError {}

/// Is `name` a session fixture file (a top-level `*.json` other than the
/// format schema)?
fn is_fixture_name(name: &str) -> bool {
    name.ends_with(".json") && name != "schema.json"
}

/// Is `name` a vector file (a `cmt-hash/*.json` other than its schema)?
fn is_vector_name(name: &str) -> bool {
    name.ends_with(".json") && name != "vector-schema.json"
}

impl FixtureSet {
    /// Load a `schemas/conformance/` directory: every top-level `*.json`
    /// except `schema.json`, and every `cmt-hash/*.json` except
    /// `vector-schema.json`. A missing `cmt-hash/` yields no vectors.
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self, LoadError> {
        let dir = dir.as_ref();
        let mut files = Vec::new();
        for (name, path) in list_json(dir)? {
            if is_fixture_name(&name) {
                files.push((name, read(&path)?));
            }
        }
        let mut vectors = Vec::new();
        let vdir = dir.join("cmt-hash");
        if vdir.is_dir() {
            for (name, path) in list_json(&vdir)? {
                if is_vector_name(&name) {
                    vectors.push((name, read(&path)?));
                }
            }
        }
        Self::from_parts(
            FixtureSource::Directory {
                path: dir.display().to_string(),
            },
            files,
            vectors,
        )
    }

    /// Load a release bundle (`macp-conformance-<version>.tar.gz`).
    ///
    /// With `sha256sums`, the tarball's digest must match the line naming it
    /// in that `SHA256SUMS` file. Every bundled file is checked against the
    /// bundle's own `MANIFEST.json` either way, and a file the manifest does
    /// not list (or one it lists that is missing) is an error.
    pub fn from_bundle(
        tarball: impl AsRef<Path>,
        sha256sums: Option<&Path>,
    ) -> Result<Self, LoadError> {
        let tarball = tarball.as_ref();
        let opened = bundle::open(tarball, sha256sums)?;
        let prefix = "schemas/conformance/";
        let mut files = Vec::new();
        let mut vectors = Vec::new();
        for (path, bytes) in &opened.files {
            let Some(rest) = path.strip_prefix(prefix) else {
                continue;
            };
            if let Some(name) = rest.strip_prefix("cmt-hash/") {
                if !name.contains('/') && is_vector_name(name) {
                    vectors.push((name.to_string(), bytes.clone()));
                }
            } else if !rest.contains('/') && is_fixture_name(rest) {
                files.push((rest.to_string(), bytes.clone()));
            }
        }
        Self::from_parts(
            FixtureSource::Bundle {
                path: tarball.display().to_string(),
                version: opened.version,
                spec_commit: opened.spec_commit,
                sha256: opened.sha256,
                checksum_verified: opened.checksum_verified,
            },
            files,
            vectors,
        )
    }

    fn from_parts(
        source: FixtureSource,
        files: Vec<(String, Vec<u8>)>,
        vectors: Vec<(String, Vec<u8>)>,
    ) -> Result<Self, LoadError> {
        let mut fixtures = files
            .into_iter()
            .map(|(name, bytes)| {
                let fixture: Fixture = serde_json::from_slice(&bytes)
                    .map_err(|e| LoadError(format!("cannot parse fixture {name}: {e}")))?;
                Ok(NamedFixture { name, fixture })
            })
            .collect::<Result<Vec<_>, LoadError>>()?;
        fixtures.sort_by(|a, b| a.name.cmp(&b.name));
        let mut vectors = vectors
            .into_iter()
            .map(|(name, bytes)| {
                let vector: CmtHashVector = serde_json::from_slice(&bytes)
                    .map_err(|e| LoadError(format!("cannot parse vector {name}: {e}")))?;
                Ok(NamedVector { name, vector })
            })
            .collect::<Result<Vec<_>, LoadError>>()?;
        vectors.sort_by(|a, b| a.name.cmp(&b.name));
        if fixtures.is_empty() {
            return Err(LoadError("no conformance fixtures found".into()));
        }
        Ok(Self {
            source,
            fixtures,
            vectors,
        })
    }

    /// The fixture named `name`, if loaded.
    pub fn fixture(&self, name: &str) -> Option<&NamedFixture> {
        self.fixtures.iter().find(|f| f.name == name)
    }
}

fn list_json(dir: &Path) -> Result<Vec<(String, PathBuf)>, LoadError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| LoadError(format!("cannot read {}: {e}", dir.display())))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| LoadError(format!("cannot read {}: {e}", dir.display())))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.ends_with(".json") {
                out.push((name.to_string(), path.clone()));
            }
        }
    }
    Ok(out)
}

fn read(path: &Path) -> Result<Vec<u8>, LoadError> {
    std::fs::read(path).map_err(|e| LoadError(format!("cannot read {}: {e}", path.display())))
}
