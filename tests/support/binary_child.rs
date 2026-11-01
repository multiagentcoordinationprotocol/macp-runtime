//! A spawned `macp-runtime` binary for the real-binary tests, with its output
//! pumped continuously into a buffer, so a test can wait for a log line (the
//! address a listener actually bound) while the process runs, and the pipes
//! never fill.
//!
//! Ports: a listener that logs its bound address (`HTTP listener bound`,
//! `metrics endpoint listening`) is given port 0 and read back from the log,
//! which cannot race. The gRPC listener logs its configured address before it
//! binds, so it still gets a [`free_port`]; that port can be taken by another
//! test between the probe and the child's bind (a TOCTOU race), so callers
//! respawn on [`addr_in_use`], at most [`SPAWN_ATTEMPTS`] times.
//!
//! Inherited sockets: on macOS std sets close-on-exec on a new socket only
//! after creating it, so a child spawned by another test thread inside that
//! window inherits the test's probe listener, and then appears to listen on
//! (and holds) a port it never bound. [`SOCKETS`] serializes this file's
//! socket creation ([`free_port`], [`bind_listener`]) with every spawn.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Held while this test process creates a listening socket or spawns a
/// child (see the module docs).
static SOCKETS: Mutex<()> = Mutex::new(());

/// How many times a caller spawns before giving up on a taken gRPC port.
pub const SPAWN_ATTEMPTS: usize = 3;

/// Kills the process on drop (a test that panics leaves nothing running).
pub struct Child {
    proc: std::process::Child,
    out: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.proc.kill();
        let _ = self.proc.wait();
    }
}

impl Child {
    /// Spawn `command` with stdout and stderr piped into the buffer.
    pub fn spawn(command: &mut Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut proc = {
            let _guard = SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
            command.spawn().expect("spawn macp-runtime")
        };
        let out = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::new();
        if let Some(s) = proc.stdout.take() {
            readers.push(pump(s, Arc::clone(&out)));
        }
        if let Some(s) = proc.stderr.take() {
            readers.push(pump(s, Arc::clone(&out)));
        }
        Self { proc, out, readers }
    }

    pub fn pid(&self) -> u32 {
        self.proc.id()
    }

    /// `kill -<sig>`.
    pub fn signal(&self, sig: &str) {
        let status = Command::new("kill")
            .args([&format!("-{sig}"), &self.proc.id().to_string()])
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{sig} failed");
    }

    pub fn alive(&mut self) -> bool {
        self.proc.try_wait().unwrap().is_none()
    }

    pub fn kill(&mut self) {
        let _ = self.proc.kill();
        let _ = self.proc.wait();
    }

    pub fn wait_exit(&mut self, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if let Some(status) = self.proc.try_wait().unwrap() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Combined stdout + stderr with ANSI escapes removed: everything once
    /// the process has exited, else what it has written so far.
    pub fn output(&mut self) -> String {
        if !self.alive() {
            for reader in self.readers.drain(..) {
                let _ = reader.join();
            }
        }
        strip_ansi(&self.out.lock().unwrap())
    }

    /// The first output line containing `needle` (ANSI removed), waiting at
    /// most `limit`. `Err(output)` if the process exits first or the wait
    /// times out.
    pub fn wait_line(&mut self, needle: &str, limit: Duration) -> Result<String, String> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(line) = self.output().lines().find(|l| l.contains(needle)) {
                return Ok(line.to_string());
            }
            if !self.alive() {
                return Err(self.output());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "no line containing {needle:?} within {limit:?}; output so far:\n{}",
                    self.output()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn pump(source: impl Read + Send + 'static, out: Arc<Mutex<String>>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(source);
        let mut line = Vec::new();
        while matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0) {
            out.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&line));
            line.clear();
        }
    })
}

/// `s` without ANSI CSI escapes (`ESC [ ... final byte`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The value of the structured field `name` in a (stripped) log line.
pub fn field(line: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    line.split_whitespace()
        .find_map(|token| token.strip_prefix(&key))
        .map(str::to_string)
}

/// A port that was free a moment ago (see the module docs for the race).
pub fn free_port() -> u16 {
    bind_listener("127.0.0.1:0").local_addr().unwrap().port()
}

/// Hold while creating a listening socket by other means, so no concurrently
/// spawned child inherits it.
pub fn socket_guard() -> std::sync::MutexGuard<'static, ()> {
    SOCKETS.lock().unwrap_or_else(|e| e.into_inner())
}

/// A std listener on `addr`, created so no concurrently spawned child can
/// inherit it.
pub fn bind_listener(addr: &str) -> std::net::TcpListener {
    let _guard = SOCKETS.lock().unwrap_or_else(|e| e.into_inner());
    std::net::TcpListener::bind(addr).unwrap()
}

/// The process exited because a port it was given was taken.
pub fn addr_in_use(output: &str) -> bool {
    output.contains("AddrInUse") || output.contains("Address already in use")
}

#[test]
fn log_parsing() {
    let line = "\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m HTTP listener bound \
                \u{1b}[3maddr\u{1b}[0m\u{1b}[2m=\u{1b}[0m127.0.0.1:57853 \u{1b}[3mtls\u{1b}[0m\u{1b}[2m=\u{1b}[0mfalse";
    let plain = strip_ansi(line);
    assert_eq!(
        plain,
        "2026  INFO HTTP listener bound addr=127.0.0.1:57853 tls=false"
    );
    assert_eq!(field(&plain, "addr").as_deref(), Some("127.0.0.1:57853"));
    assert_eq!(field(&plain, "tls").as_deref(), Some("false"));
    assert_eq!(field(&plain, "nope"), None);
    assert!(addr_in_use(
        "Error: tonic::transport::Error(Transport, Os { code: 48, kind: AddrInUse, .. })"
    ));
}
