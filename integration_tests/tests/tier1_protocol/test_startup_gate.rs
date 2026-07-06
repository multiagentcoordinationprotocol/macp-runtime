use std::process::Command;

/// B5 (master plan §1.7): with no authentication configured and no explicit
/// MACP_ALLOW_INSECURE=1 opt-in, the runtime must refuse to start rather than
/// silently running dev-mode auth where any bearer token is fully privileged.
#[test]
fn startup_refuses_without_auth_or_insecure_flag() {
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    let output = Command::new(&binary)
        .env_remove("MACP_ALLOW_INSECURE")
        .env_remove("MACP_AUTH_TOKENS_FILE")
        .env_remove("MACP_AUTH_TOKENS_JSON")
        .env_remove("MACP_AUTH_ISSUER")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:0")
        .output()
        .expect("binary must run");
    assert!(
        !output.status.success(),
        "runtime must refuse to start without configured auth"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no authentication configured"),
        "startup error must explain the auth requirement; stderr: {stderr}"
    );
}

/// D4: on SIGINT the runtime drains and exits cleanly (code 0) within the
/// drain deadline instead of being killed mid-flight.
#[test]
fn sigint_shuts_down_gracefully_within_deadline() {
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    let mut child = std::process::Command::new(&binary)
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:0")
        .env("MACP_SHUTDOWN_DRAIN_SECS", "2")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("binary must start");

    // Give it a moment to bind, then SIGINT.
    std::thread::sleep(std::time::Duration::from_millis(800));
    let _ = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill must run");

    // Must exit within drain deadline + margin.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert!(
                    status.success(),
                    "graceful shutdown must exit 0, got {status:?}"
                );
                break;
            }
            None if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("runtime did not exit within the drain deadline after SIGINT");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

/// D5: the opt-in Prometheus endpoint serves text-format counters.
#[test]
fn metrics_endpoint_serves_prometheus_text() {
    use std::io::{Read, Write};
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    let metrics_addr = "127.0.0.1:19464";
    let mut child = std::process::Command::new(&binary)
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:0")
        .env("MACP_METRICS_ADDR", metrics_addr)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("binary must start");

    // Retry-connect until the endpoint is up (bounded).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let response = loop {
        match std::net::TcpStream::connect(metrics_addr) {
            Ok(mut stream) => {
                stream
                    .write_all(b"GET /metrics HTTP/1.1\r\nHost: x\r\n\r\n")
                    .unwrap();
                let mut out = String::new();
                stream.read_to_string(&mut out).unwrap();
                break out;
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("metrics endpoint never came up: {e}");
            }
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "got: {response}");
    assert!(response.contains("text/plain"), "got: {response}");
}

/// E1 (RFC-MACP-0012 §9): policies preload from MACP_POLICIES_DIR; the wire
/// registry becomes read-only; a broken policies dir is fatal at startup.
#[tokio::test]
async fn policies_dir_loads_and_registry_is_read_only() {
    use macp_integration_tests::server_manager::ServerManager;
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("strict.json"),
        serde_json::json!({
            "policy_id": "policy.test.filepolicy",
            "mode": "macp.mode.decision.v1",
            "description": "file-loaded test policy",
            "rules": { "commitment": { "authority": "initiator_only" } },
            "schema_version": 1
        })
        .to_string(),
    )
    .unwrap();

    let dir_str = dir.path().to_string_lossy().to_string();
    let manager = ServerManager::start_with_env(&binary, &[("MACP_POLICIES_DIR", &dir_str)])
        .await
        .expect("runtime must start with a valid policies dir");
    let mut client =
        macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient::connect(
            manager.endpoint.clone(),
        )
        .await
        .expect("connect");

    fn auth<T>(inner: T) -> tonic::Request<T> {
        let mut req = tonic::Request::new(inner);
        req.metadata_mut().insert(
            "authorization",
            "Bearer agent://policy-admin".parse().unwrap(),
        );
        req
    }

    // The file-loaded policy is visible.
    let policies = client
        .list_policies(auth(macp_runtime::pb::ListPoliciesRequest {
            mode: String::new(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(
        policies
            .descriptors
            .iter()
            .any(|p| p.policy_id == "policy.test.filepolicy"),
        "file-loaded policy must be listed"
    );

    // The wire registry is read-only in this profile.
    let err = client
        .register_policy(auth(macp_runtime::pb::RegisterPolicyRequest {
            policy_descriptor: Some(macp_runtime::pb::PolicyDescriptor {
                policy_id: "policy.test.other".into(),
                mode: "*".into(),
                description: "x".into(),
                rules: "{}".to_string(),
                schema_version: 1,
                registered_at_unix_ms: 0,
            }),
        }))
        .await
        .expect_err("register must be refused in file-loaded profile");
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    // Capability advertisement matches.
    let init = client
        .initialize(tonic::Request::new(macp_runtime::pb::InitializeRequest {
            supported_protocol_versions: vec!["1.0".into()],
            capabilities: None,
            client_info: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let pr = init.capabilities.unwrap().policy_registry.unwrap();
    assert!(!pr.register_policy, "register_policy must advertise false");
    assert!(pr.list_policies);
}

/// E1 fail-fast: a runtime configured to preload governance must not start if
/// a policy file is invalid.
#[test]
fn startup_refuses_invalid_policies_dir() {
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.json"), "{not json").unwrap();

    let output = std::process::Command::new(&binary)
        .env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:0")
        .env("MACP_POLICIES_DIR", dir.path())
        .output()
        .expect("binary must run");
    assert!(
        !output.status.success(),
        "invalid policy file must be fatal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("MACP_POLICIES_DIR"),
        "error must name the policies dir; stderr: {stderr}"
    );
}
