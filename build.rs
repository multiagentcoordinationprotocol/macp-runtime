// Generates ONLY the tonic gRPC service stubs for `macp.v1`. The message types
// are generated once in the `macp-pb` crate; `.extern_path()` redirects every
// `.macp.v1` message reference in the service code to `::macp_pb::pb`, so they
// are not regenerated here. This keeps message types in a single transport-free
// crate while the service (which needs tonic) lives with the runtime.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir =
        std::env::var("DEP_MACP_PROTO_PROTO_DIR").expect("macp-proto crate must set proto_dir");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .extern_path(".macp.v1", "::macp_pb::pb")
        .compile_protos(
            &[
                "macp/v1/envelope.proto",
                "macp/v1/core.proto",
                "macp/v1/policy.proto",
            ],
            &[&proto_dir],
        )?;
    Ok(())
}
