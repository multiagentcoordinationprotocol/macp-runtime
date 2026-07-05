// Generates MACP protobuf *message* types only (no gRPC service stubs), so this
// crate stays prost-only and transport-free. The tonic service for `macp.v1`
// is generated separately in the `macp-runtime` crate via `.extern_path()`.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir =
        std::env::var("DEP_MACP_PROTO_PROTO_DIR").expect("macp-proto crate must set proto_dir");
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(false)
        .compile_protos(
            &[
                "macp/v1/envelope.proto",
                "macp/v1/core.proto",
                "macp/v1/policy.proto",
                "macp/modes/decision/v1/decision.proto",
                "macp/modes/proposal/v1/proposal.proto",
                "macp/modes/task/v1/task.proto",
                "macp/modes/handoff/v1/handoff.proto",
                "macp/modes/quorum/v1/quorum.proto",
                "macp/modes/multi_round/v1/multi_round.proto",
            ],
            &[&proto_dir],
        )?;
    Ok(())
}
