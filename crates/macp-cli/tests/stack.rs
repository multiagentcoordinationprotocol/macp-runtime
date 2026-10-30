//! The embedded self-host stack against the repo's `deploy/`. (The receipt
//! vectors moved with the verifier to `macp-core`'s `receipt_vectors` test.)

use std::path::PathBuf;

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

#[test]
fn the_written_stack_runs_this_releases_runtime() {
    let pinned = format!("macp-runtime:{}}}", env!("CARGO_PKG_VERSION"));
    let written = macp_cli::stack::compose_file();
    assert_eq!(written.matches(&pinned).count(), 1, "{written}");
    let differing: Vec<_> = written
        .lines()
        .zip(macp_cli::stack::COMPOSE_FILE.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert!(differing.len() <= 1, "{differing:?}");
    assert!(
        differing
            .iter()
            .all(|(a, _)| a.contains("MACP_RUNTIME_IMAGE")),
        "{differing:?}"
    );
}
