use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, Once};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::InitializeRequest;

/// PIDs of servers still running, killed+reaped at process exit.
///
/// `ServerManager`s held in `static`s (the per-binary shared servers in
/// `tests/common` and `tier1_jwt`) are never dropped — Rust does not drop
/// statics — so Drop-based cleanup alone leaked one server per test binary
/// on every run. The atexit hook covers exactly that path; normally-owned
/// managers still clean up via `stop()`/Drop, which deregisters them here.
static LIVE_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static ATEXIT_REGISTER: Once = Once::new();

extern "C" fn kill_live_servers_at_exit() {
    // atexit context: must not panic or allocate carelessly.
    if let Ok(pids) = LIVE_PIDS.lock() {
        for pid in pids.iter() {
            unsafe {
                libc::kill(*pid as i32, libc::SIGKILL);
                libc::waitpid(*pid as i32, std::ptr::null_mut(), 0);
            }
        }
    }
}

fn track_live_pid(pid: u32) {
    ATEXIT_REGISTER.call_once(|| unsafe {
        libc::atexit(kill_live_servers_at_exit);
    });
    if let Ok(mut pids) = LIVE_PIDS.lock() {
        pids.push(pid);
    }
}

fn untrack_live_pid(pid: u32) {
    if let Ok(mut pids) = LIVE_PIDS.lock() {
        pids.retain(|p| *p != pid);
    }
}

/// Manages the lifecycle of a local MACP runtime server subprocess.
pub struct ServerManager {
    process: Option<Child>,
    pub endpoint: String,
}

impl ServerManager {
    /// Start a runtime server on a free port. Returns once the server is accepting connections.
    pub async fn start(binary_path: &str) -> Result<Self> {
        Self::start_with_env(binary_path, &[]).await
    }

    /// Start with additional process env vars (e.g. to enable JWT auth).
    pub async fn start_with_env(binary_path: &str, extra_env: &[(&str, &str)]) -> Result<Self> {
        let port = find_free_port()?;
        let bind_addr = format!("127.0.0.1:{port}");
        let endpoint = format!("http://{bind_addr}");

        tracing::info!("Starting MACP runtime: {binary_path} on {bind_addr}");

        let mut cmd = Command::new(binary_path);
        cmd.env("MACP_ALLOW_INSECURE", "1")
            .env("MACP_MEMORY_ONLY", "1")
            .env("MACP_BIND_ADDR", &bind_addr);
        // Allow callers to override RUST_LOG via extra_env; default to warn.
        let has_rust_log = extra_env.iter().any(|(k, _)| *k == "RUST_LOG");
        if !has_rust_log {
            cmd.env("RUST_LOG", "warn");
        }
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        // Do NOT inherit the test binary's stdio: a long-lived child holding
        // the parent's stdout/stderr write ends keeps piped `cargo test`
        // output from ever reaching EOF, even after all tests pass. No test
        // asserts on the shared server's output (output-asserting tests use
        // `Command::output()` on their own spawns).
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let child = cmd
            .spawn()
            .with_context(|| format!("failed to start runtime binary: {binary_path}"))?;
        track_live_pid(child.id());

        let mut manager = Self {
            process: Some(child),
            endpoint: endpoint.clone(),
        };

        if let Err(e) = wait_for_ready(&endpoint).await {
            manager.stop();
            bail!("Server failed to become ready: {e}");
        }

        tracing::info!("MACP runtime is ready at {endpoint}");
        Ok(manager)
    }

    /// Send SIGTERM and wait for the process to exit.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            tracing::info!("Stopping MACP runtime (pid={})", child.id());
            let _ = child.kill();
            let _ = child.wait();
            untrack_live_pid(child.id());
        }
    }
}

impl Drop for ServerManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Panic-safe guard for tests that spawn the runtime binary directly instead
/// of through [`ServerManager`] (e.g. to send it signals or read its exit
/// code). The child is registered with the atexit reaper on creation and
/// killed + reaped + deregistered on drop, so a panic between spawn and the
/// test's own cleanup cannot leak a server process (a leaked child holding
/// inherited pipes hangs piped `cargo test` runs).
pub struct TrackedChild(Option<Child>);

impl TrackedChild {
    pub fn new(child: Child) -> Self {
        track_live_pid(child.id());
        Self(Some(child))
    }

    pub fn id(&self) -> u32 {
        self.0.as_ref().expect("child taken only in drop").id()
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0
            .as_mut()
            .expect("child taken only in drop")
            .try_wait()
    }
}

impl Drop for TrackedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
            untrack_live_pid(child.id());
        }
    }
}

/// Find a free TCP port by binding to port 0 and reading the assigned port.
pub fn find_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

/// Poll the server with Initialize RPCs until it responds or timeout.
async fn wait_for_ready(endpoint: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut interval = tokio::time::interval(Duration::from_millis(100));

    loop {
        interval.tick().await;

        if tokio::time::Instant::now() > deadline {
            bail!("Timed out waiting for server at {endpoint}");
        }

        match MacpRuntimeServiceClient::connect(endpoint.to_string()).await {
            Ok(mut client) => {
                let req = InitializeRequest {
                    supported_protocol_versions: vec!["1.0".into()],
                    client_info: None,
                    capabilities: None,
                };
                if client.initialize(req).await.is_ok() {
                    return Ok(());
                }
            }
            Err(_) => continue,
        }
    }
}
