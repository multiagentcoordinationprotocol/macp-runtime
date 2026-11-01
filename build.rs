// Generates ONLY the tonic gRPC service stubs for `macp.v1`. The message types
// are generated once in the `macp-pb` crate; `.extern_path()` redirects every
// `.macp.v1` message reference in the service code to `::macp_pb::pb`, so they
// are not regenerated here. This keeps message types in a single transport-free
// crate while the service (which needs tonic) lives with the runtime.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir =
        std::env::var("DEP_MACP_PROTO_PROTO_DIR").expect("macp-proto crate must set proto_dir");
    let out_dir = std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR for build scripts");
    // Written unconditionally (it's a tiny file) so the `reflection` feature
    // can `include_bytes!` it without build.rs needing to know which
    // features are enabled. Only embedded into the binary when that feature
    // is on -- see src/main.rs.
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".macp.v1", "::macp_pb::pb")
        .file_descriptor_set_path(std::path::Path::new(&out_dir).join("macp_descriptor.bin"))
        .compile_protos(
            &[
                "macp/v1/envelope.proto",
                "macp/v1/core.proto",
                "macp/v1/policy.proto",
            ],
            &[&proto_dir],
        )?;
    // `http-listener` only: a descriptor-only protoc run covering all nine
    // protos (the three `macp.v1` files plus the six mode payload files), for
    // the canonical JSON codec's descriptor pool (`src/http/json/pool.rs`).
    // No Rust is generated, so the codegen above and `macp_descriptor.bin`
    // (reflection) are untouched. Cargo exposes the feature to build scripts
    // only as an env var, not as `cfg(feature)`.
    if std::env::var_os("CARGO_FEATURE_HTTP_LISTENER").is_some() {
        write_json_descriptor_set(&proto_dir, &out_dir)?;
    }
    Ok(())
}

/// The nine protos `crates/macp-pb/build.rs` compiles, in the same order.
const JSON_CODEC_PROTOS: [&str; 9] = [
    "macp/v1/envelope.proto",
    "macp/v1/core.proto",
    "macp/v1/policy.proto",
    "macp/modes/decision/v1/decision.proto",
    "macp/modes/proposal/v1/proposal.proto",
    "macp/modes/task/v1/task.proto",
    "macp/modes/handoff/v1/handoff.proto",
    "macp/modes/quorum/v1/quorum.proto",
    "macp/modes/multi_round/v1/multi_round.proto",
];

fn write_json_descriptor_set(
    proto_dir: &str,
    out_dir: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolved the way prost-build does (`prost_build::protoc_from_env`):
    // `PROTOC` if set, else `protoc` on PATH.
    let protoc = std::env::var_os("PROTOC").unwrap_or_else(|| "protoc".into());
    let out = std::path::Path::new(out_dir).join("macp_json_descriptor.bin");
    let status = std::process::Command::new(&protoc)
        .arg("--include_imports")
        .arg(format!("--descriptor_set_out={}", out.display()))
        .arg(format!("-I{proto_dir}"))
        .args(JSON_CODEC_PROTOS)
        .status()
        .map_err(|e| format!("failed to run protoc ({protoc:?}) for http-listener: {e}"))?;
    if !status.success() {
        return Err(format!("protoc ({protoc:?}) failed for http-listener: {status}").into());
    }
    for proto in JSON_CODEC_PROTOS {
        println!("cargo:rerun-if-changed={proto_dir}/{proto}");
    }
    println!("cargo:rerun-if-env-changed=PROTOC");
    Ok(())
}
