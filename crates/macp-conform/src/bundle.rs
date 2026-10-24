//! Opening a spec release bundle (`macp-conformance-<version>.tar.gz`).
//!
//! The bundle format is `bundle_format: 1` as written by the spec repo's
//! `scripts/build-conformance-bundle.py`: one top-level directory
//! `macp-conformance-<version>/` holding the bundled files at their spec-repo
//! paths, a `MANIFEST.json` (`{bundle_format, version, spec_commit, files:
//! {path: sha256}}`) and `moved-files.json`. The release also carries a
//! `SHA256SUMS` file in `sha256sum` format covering the tarball.
//!
//! The tarball is read into memory; nothing is extracted to disk, so a
//! hostile entry name cannot write outside a directory.

use crate::fixture::LoadError;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// The only bundle format this runner reads.
pub const BUNDLE_FORMAT: u64 = 1;

/// Upper bound on the decompressed bundle, as a guard against a decompression
/// bomb. Real bundles are well under 1 MiB.
const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;

/// A verified, in-memory bundle.
#[derive(Debug)]
pub(crate) struct OpenedBundle {
    pub version: String,
    pub spec_commit: String,
    /// Hex SHA-256 of the tarball.
    pub sha256: String,
    /// Whether a `SHA256SUMS` line was checked.
    pub checksum_verified: bool,
    /// Bundled files by spec-repo path (`MANIFEST.json` and
    /// `moved-files.json` excluded).
    pub files: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The digest `SHA256SUMS` gives for the file named `file_name`.
fn expected_digest(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, name) = line.trim().split_once(char::is_whitespace)?;
        // sha256sum writes "<hex>  <name>", or "<hex> *<name>" in binary mode.
        let name = name.trim_start().trim_start_matches('*');
        (name == file_name).then(|| digest.to_ascii_lowercase())
    })
}

pub(crate) fn open(tarball: &Path, sha256sums: Option<&Path>) -> Result<OpenedBundle, LoadError> {
    let raw = std::fs::read(tarball)
        .map_err(|e| LoadError(format!("cannot read bundle {}: {e}", tarball.display())))?;
    let sha256 = sha256_hex(&raw);

    let checksum_verified = match sha256sums {
        Some(path) => {
            let sums = std::fs::read_to_string(path)
                .map_err(|e| LoadError(format!("cannot read {}: {e}", path.display())))?;
            let file_name = tarball
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| LoadError("bundle path has no file name".into()))?;
            let expected = expected_digest(&sums, file_name).ok_or_else(|| {
                LoadError(format!("{} has no line for {file_name}", path.display()))
            })?;
            if expected != sha256 {
                return Err(LoadError(format!(
                    "bundle checksum mismatch: {file_name} is {sha256}, SHA256SUMS says {expected}"
                )));
            }
            true
        }
        None => false,
    };

    let decoder = flate2::read::GzDecoder::new(raw.as_slice());
    let mut archive = tar::Archive::new(decoder.take(MAX_UNPACKED_BYTES + 1));
    let mut top: Option<String> = None;
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut total: u64 = 0;
    for entry in archive
        .entries()
        .map_err(|e| LoadError(format!("bundle is not a tar.gz: {e}")))?
    {
        let mut entry = entry.map_err(|e| LoadError(format!("bad bundle entry: {e}")))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry
            .path()
            .map_err(|e| LoadError(format!("bad bundle entry path: {e}")))?
            .to_string_lossy()
            .replace('\\', "/");
        let (head, rest) = path.split_once('/').ok_or_else(|| {
            LoadError(format!(
                "bundle entry {path} is outside a top-level directory"
            ))
        })?;
        match &top {
            None => top = Some(head.to_string()),
            Some(t) if t != head => {
                return Err(LoadError(format!(
                    "bundle has more than one top-level directory ({t}, {head})"
                )))
            }
            _ => {}
        }
        if rest.split('/').any(|seg| seg == ".." || seg.is_empty()) {
            return Err(LoadError(format!("bundle entry {path} has an unsafe path")));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| LoadError(format!("cannot read bundle entry {path}: {e}")))?;
        total += data.len() as u64;
        if total > MAX_UNPACKED_BYTES {
            return Err(LoadError("bundle unpacks to more than 64 MiB".into()));
        }
        entries.insert(rest.to_string(), data);
    }

    let manifest_raw = entries
        .remove("MANIFEST.json")
        .ok_or_else(|| LoadError("bundle has no MANIFEST.json".into()))?;
    entries.remove("moved-files.json");
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_raw)
        .map_err(|e| LoadError(format!("bundle MANIFEST.json is not JSON: {e}")))?;
    let format = manifest["bundle_format"].as_u64().unwrap_or(0);
    if format != BUNDLE_FORMAT {
        return Err(LoadError(format!(
            "unsupported bundle_format {format} (this runner reads {BUNDLE_FORMAT})"
        )));
    }
    let version = manifest["version"].as_str().unwrap_or_default().to_string();
    let spec_commit = manifest["spec_commit"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if let Some(top) = &top {
        let expected_top = format!("macp-conformance-{version}");
        if top != &expected_top {
            return Err(LoadError(format!(
                "bundle top-level directory {top} does not match its version ({expected_top})"
            )));
        }
    }
    let listed = manifest["files"]
        .as_object()
        .ok_or_else(|| LoadError("bundle MANIFEST.json has no files map".into()))?;
    for (path, digest) in listed {
        let digest = digest.as_str().unwrap_or_default();
        let data = entries.get(path).ok_or_else(|| {
            LoadError(format!("bundle is missing {path}, listed in MANIFEST.json"))
        })?;
        if sha256_hex(data) != digest {
            return Err(LoadError(format!(
                "bundle file {path} does not match its MANIFEST.json digest"
            )));
        }
    }
    if let Some(extra) = entries.keys().find(|p| !listed.contains_key(*p)) {
        return Err(LoadError(format!(
            "bundle file {extra} is not listed in MANIFEST.json"
        )));
    }

    Ok(OpenedBundle {
        version,
        spec_commit,
        sha256,
        checksum_verified,
        files: entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256sums_lines_parse() {
        let sums = "abc123  macp-conformance-0.1.12.tar.gz\nDEF  other\n";
        assert_eq!(
            expected_digest(sums, "macp-conformance-0.1.12.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(expected_digest(sums, "other").as_deref(), Some("def"));
        assert_eq!(
            expected_digest("ff *bin.tar.gz", "bin.tar.gz").as_deref(),
            Some("ff")
        );
        assert!(expected_digest(sums, "missing").is_none());
    }
}
