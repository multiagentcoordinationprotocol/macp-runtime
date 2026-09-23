//! Verifies the gRPC reflection wiring (issue #187): the embedded
//! FileDescriptorSet actually describes `macp.v1.MACPRuntimeService` and its
//! RPCs, and `tonic_reflection`'s `Builder` accepts it without error. Only
//! compiled when the `reflection` feature is enabled -- see
//! docs/deployment.md's "gRPC reflection" note under Monitoring.
#![cfg(feature = "reflection")]

use prost::Message;
use prost_types::FileDescriptorSet;

#[test]
fn descriptor_set_describes_the_real_service_and_its_rpcs() {
    let descriptor = FileDescriptorSet::decode(macp_runtime::FILE_DESCRIPTOR_SET)
        .expect("build.rs must emit a valid FileDescriptorSet");

    let service = descriptor
        .file
        .iter()
        .filter(|f| f.package.as_deref() == Some("macp.v1"))
        .flat_map(|f| f.service.iter())
        .find(|s| s.name.as_deref() == Some("MACPRuntimeService"));

    let all_services: Vec<&str> = descriptor
        .file
        .iter()
        .flat_map(|f| f.service.iter().filter_map(|s| s.name.as_deref()))
        .collect();
    let service = service.unwrap_or_else(|| {
        panic!("expected macp.v1.MACPRuntimeService in the embedded FileDescriptorSet, got services: {all_services:?}")
    });

    let methods: Vec<&str> = service
        .method
        .iter()
        .filter_map(|m| m.name.as_deref())
        .collect();
    for expected in [
        "Send",
        "GetSession",
        "StreamSession",
        "WatchSignals",
        "ListModes",
    ] {
        assert!(
            methods.contains(&expected),
            "expected RPC {expected} in macp.v1.MACPRuntimeService, got {methods:?}"
        );
    }
}

#[test]
fn reflection_service_builds_from_the_embedded_descriptor_set() {
    tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(macp_runtime::FILE_DESCRIPTOR_SET)
        .build_v1()
        .expect("reflection service should build from the embedded descriptor set");
}
