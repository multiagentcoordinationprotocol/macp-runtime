//! The spec's 12 receipt vectors (RFC-MACP-0013 Appendix A.4), vendored
//! byte-identical under the repo's `tests/vectors/receipt/` (see SOURCE.md
//! there), against `macp_cli::receipt::verify`.

use std::path::PathBuf;

use macp_cli::receipt::{verify, TrustedKeys};

fn vector_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/vectors/receipt")
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
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let name = v["name"].as_str().unwrap();
        assert_eq!(path.file_stem().unwrap(), name, "name matches the file");
        let trusted: TrustedKeys = serde_json::from_value(v["trusted_keys"].clone()).unwrap();
        let got = serde_json::to_value(verify(&v["receipt"], &trusted)).unwrap();
        assert_eq!(got, v["expected"], "{name}: {}", v["description"]);
    }
}

#[test]
fn trusting_no_key_leaves_a_signed_receipt_integrity_only() {
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(vector_dir().join("receipt_001_attested.json")).unwrap(),
    )
    .unwrap();
    let got = serde_json::to_value(verify(&v["receipt"], &TrustedKeys::new())).unwrap();
    assert_eq!(
        got,
        serde_json::json!({"integrity": "match", "attestations": ["unknown_key"], "verdict": "integrity_only"})
    );
}

#[test]
fn the_embedded_stack_is_the_repo_deploy_stack() {
    let deploy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
    assert_eq!(
        macp_cli::stack::COMPOSE_FILE,
        std::fs::read_to_string(deploy.join("docker-compose.yml")).unwrap(),
        "crates/macp-cli/deploy/docker-compose.yml must be a copy of deploy/docker-compose.yml"
    );
    assert_eq!(
        macp_cli::stack::ENV_EXAMPLE,
        std::fs::read_to_string(deploy.join(".env.example")).unwrap(),
        "crates/macp-cli/deploy/.env.example must be a copy of deploy/.env.example"
    );
}
