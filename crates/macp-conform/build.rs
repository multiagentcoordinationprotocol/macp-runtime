// With the `grpc` feature, generates the tonic CLIENT for `macp.v1` only. The
// message types come from `macp-pb` via `.extern_path()`, exactly as the
// runtime's own build.rs does for its server, so there is one set of message
// types. Without the feature this script does nothing and the crate builds
// with no transport dependency at all.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "grpc")]
    {
        let proto_dir =
            std::env::var("DEP_MACP_PROTO_PROTO_DIR").expect("macp-proto crate must set proto_dir");
        tonic_prost_build::configure()
            .build_server(false)
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
    }
    Ok(())
}
