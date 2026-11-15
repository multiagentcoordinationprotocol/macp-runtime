//! Generates the machine-readable facts manifest, `docs/kb-facts.json`, and
//! fails when the committed copy differs from what the code says.
//!
//! The manifest is built from live code, not a scan: the `MACP_*` rows come
//! from `macp_runtime::env::REGISTRY` (whose completeness `tests/env_registry.rs`
//! guards), the RPC list from the compiled `macp.v1` descriptor, and the
//! session constants from `macp_core::session`. Error codes are not repeated
//! here: `macp_core::error::ERROR_CODE_REGISTRY` is already the one table, and
//! the docs extractor reads it directly.
//!
//! Deterministic: every object is a struct (fixed field order), the env rows
//! are sorted by name, RPCs keep proto declaration order, and nothing depends
//! on time, the environment or the crate version (so a release-plz version
//! bump does not stale the snapshot).
//!
//! **Regenerate** after an intended change:
//! `MACP_REGEN_GOLDEN=1 cargo test -p macp-runtime --test kb_facts`.
//! CI publishes the checked file as the `kb-facts` artifact, and attaches it
//! to each GitHub Release.

use std::path::PathBuf;

use macp_runtime::env::REGISTRY;
use macp_runtime::macp_core::session::{
    SessionState, CURRENT_SEMANTICS_REV, MAX_SUSPENSION_CYCLES,
};
use prost::Message;
use serde::Serialize;

/// Bump when a field is renamed or removed (adding one is compatible).
const SCHEMA_VERSION: u32 = 1;
const SERVICE: &str = "macp.v1.MACPRuntimeService";

#[derive(Serialize)]
struct Manifest {
    schema_version: u32,
    generated_by: &'static str,
    session: SessionFacts,
    rpcs: Vec<Rpc>,
    env_counts: EnvCounts,
    env: Vec<Env>,
}

#[derive(Serialize)]
struct SessionFacts {
    current_semantics_rev: u32,
    max_suspension_cycles: usize,
    states: Vec<State>,
}

#[derive(Serialize)]
struct State {
    name: &'static str,
    terminal: bool,
}

#[derive(Serialize)]
struct Rpc {
    name: String,
    kind: &'static str,
    request: String,
    response: String,
}

#[derive(Serialize)]
struct EnvCounts {
    /// Distinct `MACP_*` names read by shipped binaries (runtime + cli).
    non_test: usize,
    runtime: usize,
    cli: usize,
    test: usize,
}

#[derive(Serialize)]
struct Env {
    name: &'static str,
    default: Option<&'static str>,
    test_only: bool,
    component: &'static str,
    read_in: &'static [&'static str],
    summary: &'static str,
}

/// Every state, through an exhaustive match so a new variant fails to compile
/// here until it is added.
fn states() -> Vec<State> {
    let name = |s: &SessionState| match s {
        SessionState::Open => "Open",
        SessionState::Suspended => "Suspended",
        SessionState::Resolved => "Resolved",
        SessionState::Expired => "Expired",
        SessionState::Cancelled => "Cancelled",
    };
    [
        SessionState::Open,
        SessionState::Suspended,
        SessionState::Resolved,
        SessionState::Expired,
        SessionState::Cancelled,
    ]
    .iter()
    .map(|s| State {
        name: name(s),
        terminal: s.is_terminal(),
    })
    .collect()
}

fn rpcs() -> Vec<Rpc> {
    let set = prost_types::FileDescriptorSet::decode(
        &include_bytes!(concat!(env!("OUT_DIR"), "/macp_descriptor.bin"))[..],
    )
    .expect("build.rs writes a valid descriptor set");
    let mut out = Vec::new();
    for file in &set.file {
        for service in &file.service {
            if format!("{}.{}", file.package(), service.name()) != SERVICE {
                continue;
            }
            for m in &service.method {
                let kind = match (m.client_streaming(), m.server_streaming()) {
                    (false, false) => "unary",
                    (false, true) => "server_stream",
                    (true, false) => "client_stream",
                    (true, true) => "bidi_stream",
                };
                let short = |t: &str| t.trim_start_matches('.').to_string();
                out.push(Rpc {
                    name: m.name().to_string(),
                    kind,
                    request: short(m.input_type()),
                    response: short(m.output_type()),
                });
            }
        }
    }
    assert!(!out.is_empty(), "{SERVICE} not found in the descriptor set");
    out
}

fn manifest() -> Manifest {
    let env: Vec<Env> = REGISTRY
        .iter()
        .map(|v| Env {
            name: v.name,
            default: v.default,
            test_only: v.test_only,
            component: v.component.as_str(),
            read_in: v.read_in,
            summary: v.summary,
        })
        .collect();
    let count = |c: &str| env.iter().filter(|e| e.component == c).count();
    Manifest {
        schema_version: SCHEMA_VERSION,
        generated_by: "macp-runtime tests/kb_facts.rs",
        session: SessionFacts {
            current_semantics_rev: CURRENT_SEMANTICS_REV,
            max_suspension_cycles: MAX_SUSPENSION_CYCLES,
            states: states(),
        },
        rpcs: rpcs(),
        env_counts: EnvCounts {
            non_test: env.iter().filter(|e| !e.test_only).count(),
            runtime: count("runtime"),
            cli: count("cli"),
            test: count("test"),
        },
        env,
    }
}

fn render() -> String {
    let mut s = serde_json::to_string_pretty(&manifest()).unwrap();
    s.push('\n');
    s
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/kb-facts.json")
}

#[test]
fn rendering_is_deterministic() {
    assert_eq!(render(), render());
}

#[test]
fn committed_manifest_matches_the_code() {
    let actual = render();
    let path = snapshot_path();
    if std::env::var("MACP_REGEN_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "no manifest at {} ({e}); regenerate with \
             MACP_REGEN_GOLDEN=1 cargo test -p macp-runtime --test kb_facts",
            path.display()
        )
    });
    assert!(
        expected == actual,
        "docs/kb-facts.json is stale; regenerate with \
         MACP_REGEN_GOLDEN=1 cargo test -p macp-runtime --test kb_facts\n--- actual\n{actual}"
    );
}
