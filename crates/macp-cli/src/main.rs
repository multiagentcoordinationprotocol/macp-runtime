//! `macp`: the MACP command line.
//!
//! Exit status, for every command: 0 when it succeeded or its check passed, 1
//! when its check ran and failed, 2 when it could not run (bad usage,
//! unreadable input, docker missing, ...).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use macp_cli::receipt::{self, TrustedKeys, Verdict};
use macp_cli::replay::{self, ReplayError, Source};
use macp_cli::stack::{self, StackError};
use macp_cli::tail::{self, TailEnd, TailOptions};

#[derive(Parser)]
#[command(name = "macp", version, about = "The MACP command line")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the self-host stack (runtime, control plane, console, playground)
    /// pinned in this release, without a clone.
    Up {
        /// Follow the logs in the foreground instead of returning once the
        /// stack is healthy.
        #[arg(long)]
        attach: bool,
        /// A .env file overriding the defaults in .env.example.
        #[arg(long)]
        env_file: Option<PathBuf>,
    },
    /// Stop the self-host stack.
    Down {
        /// Also delete its volumes: every recorded run and session.
        #[arg(long)]
        volumes: bool,
    },
    /// Show the self-host stack's containers.
    Status,
    /// Verify a decision receipt (RFC-MACP-0013 Appendix A.4).
    Verify {
        /// The receipt JSON, or '-' for stdin.
        receipt: String,
        /// A trusted Ed25519 key: a JWK or JWKS file, or `<kid>=<x>` with x the
        /// base64url public key. Repeatable.
        #[arg(long = "key", value_name = "FILE|KID=X")]
        keys: Vec<String>,
        /// The weakest verdict that exits 0.
        #[arg(long, value_enum, default_value_t = Require::Attested)]
        require: Require,
        /// Print the result as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Read a session on a running runtime.
    Session {
        #[command(subcommand)]
        command: SessionCmd,
    },
    /// Replay a recorded session, alone (an integrity check) or under a
    /// candidate policy (what-if, RFC-MACP-0012 §8.1).
    #[command(group(clap::ArgGroup::new("source").required(true).args(["data_dir", "file", "session"])))]
    Replay {
        /// A file-backend data directory (`MACP_DATA_DIR`); give the session id.
        #[arg(long, value_name = "DIR", requires = "session_id")]
        data_dir: Option<PathBuf>,
        /// The session id in `--data-dir`.
        #[arg(requires = "data_dir")]
        session_id: Option<String>,
        /// A session log: JSON lines of log entries, or a JSON array of them.
        #[arg(long, value_name = "LOG")]
        file: Option<PathBuf>,
        /// A session on a running runtime, replayed there with `WhatIfReplay`.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// The candidate policy definition (JSON, as in `MACP_POLICIES_DIR`).
        #[arg(long, value_name = "FILE")]
        policy: Option<PathBuf>,
        /// With --session: the runtime's gRPC endpoint.
        #[arg(long, env = "MACP_TARGET", default_value = "grpc://127.0.0.1:50051")]
        target: String,
        /// With --session: a bearer token that can read the session.
        #[arg(
            long,
            env = "MACP_TOKEN",
            default_value = "macp-dev-cp-observer",
            hide_env_values = true
        )]
        token: String,
        /// Print the report as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Run the spec's conformance fixtures against an implementation. Takes
    /// macp-conform's arguments; `macp conform --help` lists them.
    #[command(disable_help_flag = true, disable_version_flag = true)]
    Conform {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Print a session's accepted envelopes, then follow live traffic until
    /// the session ends.
    Tail {
        /// The session id.
        session_id: String,
        /// The runtime's gRPC endpoint: grpc://, grpcs://, http:// or https://.
        #[arg(long, env = "MACP_TARGET", default_value = "grpc://127.0.0.1:50051")]
        target: String,
        /// Bearer token: an observer, the initiator or a participant. The
        /// default is the self-host stack's observer token.
        #[arg(
            long,
            env = "MACP_TOKEN",
            default_value = "macp-dev-cp-observer",
            hide_env_values = true
        )]
        token: String,
        /// Skip the first N accepted envelopes.
        #[arg(long, value_name = "N", default_value_t = 0)]
        after: u64,
        /// Print the history and exit, even if the session is still open.
        #[arg(long)]
        no_follow: bool,
        /// One JSON object per line, payload in base64.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Require {
    /// Integrity holds and a trusted key signed it.
    Attested,
    /// Integrity holds; no signature needed.
    Integrity,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Cmd::Up { attach, env_file } => up(attach, env_file),
        Cmd::Down { volumes } => {
            let args: &[&str] = if volumes { &["down", "-v"] } else { &["down"] };
            stack_cmd(args)
        }
        Cmd::Status => stack_cmd(&["ps", "-a"]),
        Cmd::Verify {
            receipt,
            keys,
            require,
            json,
        } => verify(&receipt, &keys, require, json),
        Cmd::Session {
            command:
                SessionCmd::Tail {
                    session_id,
                    target,
                    token,
                    after,
                    no_follow,
                    json,
                },
        } => {
            let mut opts = TailOptions::new(session_id);
            opts.after = after;
            opts.follow = !no_follow;
            opts.json = json;
            session_tail(&target, &token, &opts)
        }
        Cmd::Replay {
            data_dir,
            session_id,
            file,
            session,
            policy,
            target,
            token,
            json,
        } => {
            let source = match (data_dir, session_id, file, session) {
                (Some(dir), Some(session_id), _, _) => Source::DataDir { dir, session_id },
                (_, _, Some(f), _) => Source::File(f),
                (_, _, _, Some(session_id)) => Source::Session {
                    target,
                    token,
                    session_id,
                },
                _ => unreachable!("clap requires one source"),
            };
            replay_cmd(&source, policy.as_deref(), json)
        }
        Cmd::Conform { args } => conform(args),
    }
}

fn fail(e: StackError) -> ExitCode {
    eprintln!("macp: {e}");
    match e {
        StackError::Unavailable(_) => ExitCode::from(2),
        StackError::Failed(_) => ExitCode::from(1),
    }
}

fn up(attach: bool, env_file: Option<PathBuf>) -> ExitCode {
    let compose = match stack::stack_dir().and_then(|d| stack::materialize(&d)) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    eprintln!(
        "macp: stack files in {}",
        compose.parent().unwrap().display()
    );
    let args: &[&str] = if attach {
        &["up"]
    } else {
        &["up", "-d", "--wait", "--wait-timeout", "300"]
    };
    if let Err(e) = stack::run(stack::compose(&compose, env_file.as_deref(), args)) {
        return fail(e);
    }
    if !attach {
        for (name, url) in stack::urls() {
            println!("{name:<16} {url}");
        }
        eprintln!("macp: open the console and launch the Showcase run; `macp down` stops it");
    }
    ExitCode::SUCCESS
}

fn stack_cmd(args: &[&str]) -> ExitCode {
    let compose = match stack::stack_dir().and_then(|d| stack::materialize(&d)) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    match stack::run(stack::compose(&compose, None, args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

fn verify(path: &str, keys: &[String], require: Require, json: bool) -> ExitCode {
    let usage = |m: String| {
        eprintln!("macp verify: {m}");
        ExitCode::from(2)
    };
    let text = if path == "-" {
        std::io::read_to_string(std::io::stdin())
    } else {
        std::fs::read_to_string(path)
    };
    let receipt: serde_json::Value = match text
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(r) => r,
        Err(e) => return usage(format!("{path}: {e}")),
    };
    let mut trusted = TrustedKeys::new();
    for key in keys {
        if let Err(e) = add_key(key, &mut trusted) {
            return usage(format!("--key {key}: {e}"));
        }
    }

    let result = receipt::verify(&receipt, &trusted);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("a Verification always serializes")
        );
    } else {
        let integrity = result
            .integrity
            .map(|i| serde_json::to_value(i).unwrap())
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "-".into());
        println!("integrity     {integrity}");
        let atts = receipt.get("attestations").and_then(|a| a.as_array());
        for (i, outcome) in result.attestations.iter().enumerate() {
            let att = atts.and_then(|a| a.get(i));
            let kind = att
                .and_then(|a| a.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            println!(
                "attestation {i} {kind:<6} {}",
                serde_json::to_value(outcome).unwrap().as_str().unwrap()
            );
        }
        println!("verdict       {}", result.verdict.as_str());
    }
    let ok = match require {
        Require::Attested => result.verdict == Verdict::Attested,
        Require::Integrity => matches!(result.verdict, Verdict::Attested | Verdict::IntegrityOnly),
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn add_key(key: &str, trusted: &mut TrustedKeys) -> Result<(), String> {
    if let Some((kid, x)) = key.split_once('=') {
        if !kid.is_empty() && !std::path::Path::new(key).exists() {
            trusted.insert(kid.into(), x.into());
            return Ok(());
        }
    }
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(key).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    match receipt::add_jwks(&doc, trusted).map_err(|e| e.to_string())? {
        0 => Err("no Ed25519 key in it".into()),
        _ => Ok(()),
    }
}

fn session_tail(target: &str, token: &str, opts: &TailOptions) -> ExitCode {
    let Some(rt) = runtime("macp session tail") else {
        return ExitCode::from(2);
    };
    let mut out = std::io::stdout().lock();
    match rt.block_on(tail::run(target, token, opts, &mut out)) {
        Ok(TailEnd::Ended { state, printed } | TailEnd::Caught { state, printed }) => {
            eprintln!(
                "macp: session {} is {state} ({printed} shown)",
                opts.session_id
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("macp session tail: {e}");
            ExitCode::from(2)
        }
    }
}

fn replay_cmd(source: &Source, policy: Option<&std::path::Path>, json: bool) -> ExitCode {
    let report = |e: ReplayError| {
        eprintln!("macp replay: {e}");
        match e {
            ReplayError::Integrity(_) => ExitCode::from(1),
            ReplayError::Unavailable(_) => ExitCode::from(2),
        }
    };
    let candidate = match policy.map(replay::read_policy).transpose() {
        Ok(c) => c,
        Err(e) => return report(e),
    };
    let Some(rt) = runtime("macp replay") else {
        return ExitCode::from(2);
    };
    let resp = match rt.block_on(replay::run(source, candidate.as_ref())) {
        Ok(r) => r,
        Err(e) => return report(e),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&replay::render_json(&resp)).unwrap()
        );
    } else {
        print!("{}", replay::render_text(&resp, candidate.is_some()));
    }
    if candidate.is_some() && replay::outcome_changed(&resp) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn conform(args: Vec<String>) -> ExitCode {
    let Some(rt) = runtime("macp conform") else {
        return ExitCode::from(2);
    };
    rt.block_on(macp_conform::cli::main("macp conform", args))
}

fn runtime(prog: &str) -> Option<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| eprintln!("{prog}: {e}"))
        .ok()
}
