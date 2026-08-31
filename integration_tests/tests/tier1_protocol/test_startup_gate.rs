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
    use macp_integration_tests::server_manager::{find_free_port, TrackedChild};
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    // Bind to a known free port so readiness can be observed by connecting,
    // instead of a fixed sleep hoping the server is up before the SIGINT.
    let bind_addr = format!("127.0.0.1:{}", find_free_port().expect("free port"));
    let mut child = TrackedChild::new(
        std::process::Command::new(&binary)
            .env("MACP_ALLOW_INSECURE", "1")
            .env("MACP_MEMORY_ONLY", "1")
            .env("MACP_BIND_ADDR", &bind_addr)
            .env("MACP_SHUTDOWN_DRAIN_SECS", "2")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("binary must start"),
    );

    // Wait until the listener accepts connections, then SIGINT.
    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::net::TcpStream::connect(&bind_addr).is_err() {
        assert!(
            std::time::Instant::now() < ready_deadline,
            "runtime never started listening on {bind_addr}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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
                panic!("runtime did not exit within the drain deadline after SIGINT");
            }
            None => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

/// D5: the opt-in Prometheus endpoint serves text-format counters.
#[test]
fn metrics_endpoint_serves_prometheus_text() {
    use macp_integration_tests::server_manager::{find_free_port, TrackedChild};
    use std::io::{Read, Write};
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    // Dynamic port: a fixed one collides with concurrent runs or anything
    // else on the machine that happens to hold it.
    let metrics_addr = format!("127.0.0.1:{}", find_free_port().expect("free port"));
    let _child = TrackedChild::new(
        std::process::Command::new(&binary)
            .env("MACP_ALLOW_INSECURE", "1")
            .env("MACP_MEMORY_ONLY", "1")
            .env("MACP_BIND_ADDR", "127.0.0.1:0")
            .env("MACP_METRICS_ADDR", &metrics_addr)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("binary must start"),
    );

    // Retry-connect until the endpoint is up (bounded).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let response = loop {
        match std::net::TcpStream::connect(&metrics_addr) {
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
            Err(e) => panic!("metrics endpoint never came up: {e}"),
        }
    };
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

/// Spawn the runtime with the given extra env and require it to EXIT.
///
/// Deliberately not `Command::output()`: `output()` blocks until the child
/// closes its pipes, so if the startup validation under test ever regresses the
/// binary starts a server that never exits and the test hangs forever instead
/// of failing. Bounding the wait turns that regression into a real failure.
///
/// Returns `(stdout, stderr, exit_status)`.
fn run_expecting_startup_abort(
    binary: &str,
    extra_env: &[(&str, &str)],
) -> (String, String, std::process::ExitStatus) {
    let mut cmd = std::process::Command::new(binary);
    cmd.env("MACP_ALLOW_INSECURE", "1")
        .env("MACP_MEMORY_ONLY", "1")
        .env("MACP_BIND_ADDR", "127.0.0.1:0")
        // Start from a clean slate so an exported value in the developer's
        // shell cannot mask or manufacture the condition under test.
        .env_remove("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE")
        .env_remove("MACP_LIST_SESSIONS_MAX_PAGE_SIZE")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    // Wrapped in `TrackedChild` so any panic below — the deadline panic, or a
    // `try_wait` error — still kills and reaps the process. A leaked child
    // holding the inherited pipes hangs the whole piped `cargo test` run.
    let mut child = macp_integration_tests::server_manager::TrackedChild::new(
        cmd.spawn().expect("binary must run"),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while child.try_wait().expect("try_wait").is_none() {
        assert!(
            std::time::Instant::now() <= deadline,
            "runtime did not abort for {extra_env:?}; it started a server instead"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let output = child.wait_with_output().expect("collect child output");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status,
    )
}

/// D5: `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` / `MACP_LIST_SESSIONS_MAX_PAGE_SIZE`
/// are positive integers. A `0` or an unparseable value must abort startup with
/// a message naming the offending variable — a page size of zero would make
/// `ListSessions` return an empty page forever.
///
/// `MACP_ALLOW_INSECURE=1` is set deliberately (see `run_expecting_startup_abort`):
/// without it the binary also refuses to start for an unrelated reason (the
/// no-auth gate pinned by `startup_refuses_without_auth_or_insecure_flag`), and
/// an exit-status assertion would pass while proving nothing about page-size
/// validation. Hence the assertions below are on the message naming the
/// variable, not on the status alone.
///
/// NOTE ON STREAMS: the per-error detail is folded into the error `main`
/// returns, so it lands on **stderr** with the "startup aborted" summary,
/// independently of the `tracing` subscriber. The assertions below are on
/// stderr alone, and the `RUST_LOG=off` case pins exactly that: with logging
/// silenced the offending variable must still be named on stderr.
#[test]
fn startup_refuses_zero_or_invalid_list_sessions_page_size() {
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());

    for (var, value) in [
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "0"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "0"),
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "not-a-number"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "12.5"),
        ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "-1"),
        ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", ""),
    ] {
        // `RUST_LOG=off` silences the `tracing::error!` copy of the detail, so
        // only the returned error can satisfy the assertions below.
        let (_stdout, stderr, status) =
            run_expecting_startup_abort(&binary, &[(var, value), ("RUST_LOG", "off")]);

        assert!(
            !status.success(),
            "{var}={value:?} must be fatal at startup"
        );
        assert!(
            stderr.contains("startup aborted"),
            "{var}={value:?}: stderr must report the aborted startup; stderr: {stderr}"
        );
        assert!(
            stderr.contains(var),
            "{var}={value:?}: stderr must name the offending variable even with RUST_LOG=off; stderr: {stderr}"
        );
        assert!(
            stderr.contains("positive integer"),
            "{var}={value:?}: stderr must explain the constraint even with RUST_LOG=off; stderr: {stderr}"
        );
    }
}

/// D5 / R7: a default page size above the max in force would be silently
/// clamped down by `SecurityLayer`, discarding the operator's stated intent
/// without ever telling them. Startup must refuse instead, naming both values.
///
/// The check is deliberately SYMMETRIC: leaving `MACP_LIST_SESSIONS_MAX_PAGE_SIZE`
/// unset is the same operator error, since the default is then clamped down to
/// the built-in cap just as silently (a `tracing::warn!` that vanishes under
/// `RUST_LOG=off`). Both cases are pinned below, and the max-unset message must
/// say where its number came from so `1000` is not mistaken for a configured
/// value.
///
/// See the `MACP_ALLOW_INSECURE` and stream notes on
/// `startup_refuses_zero_or_invalid_list_sessions_page_size`.
#[test]
fn startup_refuses_default_page_size_above_max() {
    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());

    // (a) Both set, default above the explicit max.
    let (_stdout, stderr, status) = run_expecting_startup_abort(
        &binary,
        &[
            ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "2000"),
            ("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", "1000"),
            ("RUST_LOG", "off"),
        ],
    );

    assert!(
        !status.success(),
        "a default page size above the max must be fatal at startup"
    );
    assert!(
        stderr.contains("startup aborted"),
        "stderr must report the aborted startup; stderr: {stderr}"
    );
    for needle in [
        "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE",
        "MACP_LIST_SESSIONS_MAX_PAGE_SIZE",
        "2000",
        "1000",
    ] {
        assert!(
            stderr.contains(needle),
            "the cross-field error must mention '{needle}' on stderr; stderr: {stderr}"
        );
    }
    assert!(
        stderr.contains("explicitly configured"),
        "with the max explicitly set the error must say so; stderr: {stderr}"
    );

    // (b) Max UNSET: the default is still above the max in force (the built-in
    // 1000), so startup must refuse just as loudly. `run_expecting_startup_abort`
    // removes both vars before applying `extra_env`, so the max really is unset.
    let (_stdout, stderr, status) = run_expecting_startup_abort(
        &binary,
        &[
            ("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "5000"),
            ("RUST_LOG", "off"),
        ],
    );

    assert!(
        !status.success(),
        "a default page size above the built-in max must be fatal even with the max unset"
    );
    assert!(
        stderr.contains("startup aborted"),
        "stderr must report the aborted startup; stderr: {stderr}"
    );
    for needle in ["MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", "5000", "1000"] {
        assert!(
            stderr.contains(needle),
            "the max-unset error must mention '{needle}' on stderr; stderr: {stderr}"
        );
    }
    assert!(
        stderr.contains("unset") && stderr.contains("built-in default"),
        "the max-unset error must say the 1000 is the built-in default, not a configured value; stderr: {stderr}"
    );

    // Valid configurations must still start. `default == max` pins that the
    // check is strictly `>` and not `>=`; `default < max` pins that it is a
    // comparison at all and not, say, `!=`. Without both, the refusals above
    // could be coming from a check that also rejects sane configurations.
    for (default_size, max_size) in [("1000", "1000"), ("50", "1000")] {
        let mut ok = macp_integration_tests::server_manager::TrackedChild::new(
            std::process::Command::new(&binary)
                .env("MACP_ALLOW_INSECURE", "1")
                .env("MACP_MEMORY_ONLY", "1")
                .env("MACP_BIND_ADDR", "127.0.0.1:0")
                .env("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE", default_size)
                .env("MACP_LIST_SESSIONS_MAX_PAGE_SIZE", max_size)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("binary must start"),
        );
        std::thread::sleep(std::time::Duration::from_millis(750));
        assert!(
            ok.try_wait().expect("try_wait").is_none(),
            "default={default_size} max={max_size} is a valid configuration and must not abort startup"
        );
    }
}
