//! `macp up` / `macp down` / `macp status`: the self-host stack without a
//! clone.
//!
//! The compose file and `.env.example` are the repo's `deploy/` files,
//! embedded at build time, so a `macp` binary runs the stack pinned in its own
//! release. They are written to `<home>/.macp/deploy/<version>/` and driven
//! with `docker compose`; a `.env` placed in that directory is read as usual.
//! The copies under this crate's `deploy/` are byte-identical to the repo's
//! (a test checks it): a published crate cannot reach outside its own
//! directory.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `deploy/docker-compose.yml`, as of this release.
pub const COMPOSE_FILE: &str = include_str!("../deploy/docker-compose.yml");
/// `deploy/.env.example`, as of this release.
pub const ENV_EXAMPLE: &str = include_str!("../deploy/.env.example");
/// The compose project name, fixed so `down` and `status` find a stack that
/// another `macp` release brought up.
pub const PROJECT: &str = "macp";

/// Why a stack command could not complete.
#[derive(Debug)]
pub enum StackError {
    /// `docker compose` could not be run at all (exit 2).
    Unavailable(String),
    /// `docker compose` ran and failed (exit 1).
    Failed(String),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackError::Unavailable(m) | StackError::Failed(m) => f.write_str(m),
        }
    }
}

/// Where this release's stack files live: `$MACP_HOME/deploy/<version>`, or
/// `~/.macp/deploy/<version>`.
pub fn stack_dir() -> Result<PathBuf, StackError> {
    let base = match std::env::var_os("MACP_HOME") {
        Some(h) => PathBuf::from(h),
        None => std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|h| PathBuf::from(h).join(".macp"))
            .ok_or_else(|| {
                StackError::Unavailable("set HOME or MACP_HOME to choose a directory".into())
            })?,
    };
    Ok(base.join("deploy").join(env!("CARGO_PKG_VERSION")))
}

/// Write the embedded files into `dir`. The compose file is always
/// rewritten; `.env.example` too; an existing `.env` is left alone.
pub fn materialize(dir: &Path) -> Result<PathBuf, StackError> {
    let io = |e: std::io::Error| StackError::Unavailable(format!("{}: {e}", dir.display()));
    std::fs::create_dir_all(dir).map_err(io)?;
    let compose = dir.join("docker-compose.yml");
    std::fs::write(&compose, COMPOSE_FILE).map_err(io)?;
    std::fs::write(dir.join(".env.example"), ENV_EXAMPLE).map_err(io)?;
    Ok(compose)
}

/// `docker compose -p macp -f <compose> [--env-file <f>] <args>`.
pub fn compose(compose: &Path, env_file: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new("docker");
    cmd.arg("compose")
        .args(["-p", PROJECT])
        .arg("-f")
        .arg(compose);
    if let Some(env) = env_file {
        cmd.arg("--env-file").arg(env);
    }
    cmd.args(args);
    cmd
}

/// Run `cmd` with inherited stdio.
pub fn run(mut cmd: Command) -> Result<(), StackError> {
    match cmd.status() {
        Err(e) => Err(StackError::Unavailable(format!(
            "cannot run docker ({e}); install Docker with Compose v2"
        ))),
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(StackError::Failed(format!(
            "docker compose exited with {s}"
        ))),
    }
}

/// The URLs `macp up` prints, with the port overrides `.env.example`
/// documents applied from the environment.
pub fn urls() -> Vec<(&'static str, String)> {
    let port = |var: &str, default: &str| std::env::var(var).unwrap_or_else(|_| default.into());
    vec![
        (
            "console",
            format!("http://localhost:{}", port("MACP_CONSOLE_PORT", "3000")),
        ),
        (
            "control plane",
            format!("http://localhost:{}", port("MACP_CP_PORT", "3001")),
        ),
        (
            "playground",
            format!("http://localhost:{}", port("MACP_PLAYGROUND_PORT", "3100")),
        ),
        (
            "runtime (gRPC)",
            format!("grpc://127.0.0.1:{}", port("MACP_RUNTIME_PORT", "50051")),
        ),
    ]
}
