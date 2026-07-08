//! TLS transport: the runtime serves gRPC over TLS when MACP_TLS_CERT_PATH /
//! MACP_TLS_KEY_PATH are set, and a client configured with the server's CA
//! can complete the handshake and issue RPCs. Uses a throwaway self-signed
//! certificate; spawns its own server because the shared harness (and its
//! readiness probe) is plaintext-only.

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::{find_free_port, TrackedChild};
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use tonic::transport::{Certificate, Channel, ClientTlsConfig};

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

#[tokio::test]
async fn tls_handshake_and_rpc_round_trip() {
    // Self-signed cert for localhost (SAN: DNS localhost + IP 127.0.0.1).
    let rcgen::CertifiedKey { cert, key_pair } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .expect("cert generation");
    let dir = tempfile::tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.crt");
    let key_path = dir.path().join("server.key");
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();

    let port = find_free_port().expect("free port");
    let bind_addr = format!("127.0.0.1:{port}");
    let _child = TrackedChild::new(
        std::process::Command::new(test_binary())
            .env("MACP_ALLOW_INSECURE", "1") // dev auth; transport is still TLS
            .env("MACP_MEMORY_ONLY", "1")
            .env("MACP_BIND_ADDR", &bind_addr)
            .env("MACP_TLS_CERT_PATH", &cert_path)
            .env("MACP_TLS_KEY_PATH", &key_path)
            .env("RUST_LOG", "warn")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("binary must start"),
    );

    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(cert.pem()))
        .domain_name("localhost");

    // Poll readiness over TLS (the plaintext probe can't see a TLS server).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut client = loop {
        let attempt = async {
            let channel = Channel::from_shared(format!("https://127.0.0.1:{port}"))?
                .tls_config(tls.clone())?
                .connect()
                .await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(MacpRuntimeServiceClient::new(
                channel,
            ))
        }
        .await;
        match attempt {
            Ok(client) => break client,
            Err(e) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "TLS server never became reachable: {e}"
                );
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    };

    // A full session round-trip over the encrypted channel.
    let sid = new_session_id();
    let agent = "agent://tls-client";
    let partner = "agent://partner";
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent,
            session_start_payload("tls test", &[agent, partner], 60_000),
        ),
    )
    .await
    .expect("Send over TLS must succeed");
    assert!(ack.ok);

    let resp = get_session_as(&mut client, agent, &sid).await.unwrap();
    assert_eq!(resp.metadata.expect("metadata").state, 1); // OPEN

    // Plaintext against the TLS listener must fail (no accidental fallback).
    let plaintext = MacpRuntimeServiceClient::connect(format!("http://127.0.0.1:{port}")).await;
    let plaintext_works = match plaintext {
        Err(_) => false,
        Ok(mut c) => initialize(&mut c).await.is_ok(),
    };
    assert!(
        !plaintext_works,
        "plaintext RPCs must not succeed against a TLS-only listener"
    );
}
