//! The `macp-conform` command line, as a function, so another binary (the
//! `macp` CLI's `macp conform`) can run it unchanged.
//!
//! ```text
//! macp-conform --target grpc://127.0.0.1:50051 --fixtures spec/schemas/conformance --level standard
//! macp-conform --transport http --target http://127.0.0.1:8080 --fixtures spec/schemas/conformance
//! ```
//!
//! `--transport` picks the binding (default `grpc`); a URL's scheme never
//! does, so `http://` keeps meaning gRPC in the clear unless `--transport
//! http` is given. The HTTP transport needs the crate's `http` feature.
//!
//! Exit status: 0 when no selected fixture failed, 1 when one did (or, with
//! `--strict`, when a level is incomplete), 2 when the run could not start.

use crate::{
    run, run_with_hash_target, ConformanceTarget, FixtureSet, GrpcTarget, InProcessTarget, Level,
    RunOptions,
};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: macp-conform --target <grpc://host:port | grpcs://host:port | in-process>
                    [--transport grpc|http]
                    (--fixtures <dir> | --bundle <macp-conformance-X.tar.gz> [--sha256sums <file>])
                    [--level core|decision|standard] [--multi-round] [--strict]
                    [--hash-vectors in-process]
                    [--report <path>] [--token <sender>=<token>]... [--quiet]

  --target       the implementation under test: a MACPRuntimeService endpoint
                 (Server profile) or this crate's in-process target (Embedded)
  --transport    the binding a network target is driven over (default: grpc).
                 http: the HTTP/JSON binding (RFC-MACP-0006 §4), --target is
                 its base URL, http://host:port or https://host:port[/prefix].
                 A URL scheme never selects the binding: http:// with the
                 default transport is gRPC in the clear
  --fixtures     a spec schemas/conformance/ directory
  --bundle       a spec release bundle; with --sha256sums, its checksum is verified
  --level        run this level and every level below it (default: standard)
  --multi-round  also run the optional multi_round fixtures
  --strict       treat an incomplete level (skipped fixtures) as failure
  --hash-vectors in-process
                 with a network target, run the cmt-hash vectors through this
                 crate's in-process target instead (a server has no hash RPC).
                 The server must report itself as macp-runtime at this
                 runner's version: the spec counts the vectors only against
                 the same implementation and release
  --report       write the JSON report here ('-' for stdout)
  --token        bearer token to use for a sender (default: the sender's name,
                 as a server in MACP_ALLOW_INSECURE=1 development mode expects)
  --quiet        no human-readable summary on stderr";

/// The binding a network target is driven over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Grpc,
    Http,
}

#[derive(Debug)]
struct Args {
    target: String,
    transport: Transport,
    fixtures: Option<PathBuf>,
    bundle: Option<PathBuf>,
    sha256sums: Option<PathBuf>,
    level: Level,
    multi_round: bool,
    strict: bool,
    hashes_in_process: bool,
    report: Option<String>,
    tokens: Vec<(String, String)>,
    quiet: bool,
}

fn parse(prog: &str, args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut it = args.into_iter();
    let mut a = Args {
        target: String::new(),
        transport: Transport::Grpc,
        fixtures: None,
        bundle: None,
        sha256sums: None,
        level: Level::Standard,
        multi_round: false,
        strict: false,
        hashes_in_process: false,
        report: None,
        tokens: Vec::new(),
        quiet: false,
    };
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--target" => a.target = value("--target")?,
            "--transport" => {
                a.transport = match value("--transport")?.as_str() {
                    "grpc" => Transport::Grpc,
                    "http" => Transport::Http,
                    v => return Err(format!("--transport takes grpc or http, got {v:?}")),
                }
            }
            "--fixtures" => a.fixtures = Some(value("--fixtures")?.into()),
            "--bundle" => a.bundle = Some(value("--bundle")?.into()),
            "--sha256sums" => a.sha256sums = Some(value("--sha256sums")?.into()),
            "--level" => {
                let v = value("--level")?;
                a.level = Level::parse(&v).ok_or_else(|| format!("unknown level {v:?}"))?;
            }
            "--multi-round" => a.multi_round = true,
            "--strict" => a.strict = true,
            "--hash-vectors" => match value("--hash-vectors")?.as_str() {
                "in-process" => a.hashes_in_process = true,
                v => return Err(format!("--hash-vectors takes in-process, got {v:?}")),
            },
            "--report" => a.report = Some(value("--report")?),
            "--token" => {
                let v = value("--token")?;
                let (s, t) = v
                    .split_once('=')
                    .ok_or_else(|| format!("--token wants <sender>=<token>, got {v:?}"))?;
                a.tokens.push((s.into(), t.into()));
            }
            "--quiet" => a.quiet = true,
            "-h" | "--help" => return Err(String::new()),
            "-V" | "--version" => {
                println!("{prog} {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if a.target.is_empty() {
        return Err("--target is required".into());
    }
    if a.fixtures.is_some() == a.bundle.is_some() {
        return Err("give exactly one of --fixtures and --bundle".into());
    }
    if a.hashes_in_process && a.target == "in-process" {
        return Err("--hash-vectors in-process is for a network target".into());
    }
    if a.transport == Transport::Http && a.target == "in-process" {
        return Err("--transport http needs a network --target".into());
    }
    if a.transport == Transport::Http && !cfg!(feature = "http") {
        return Err(
            "--transport http: this build has no HTTP transport (build with --features http)"
                .into(),
        );
    }
    if a.sha256sums.is_some() && a.bundle.is_none() {
        return Err("--sha256sums needs --bundle".into());
    }
    Ok(a)
}

/// The in-process target is the reference kernel at this runner's version,
/// so its hash results stand in for the server's only when the server is
/// that implementation at that release (spec `docs/conformance.md`).
fn same_release((name, version): (String, String)) -> Result<(), String> {
    let want = env!("CARGO_PKG_VERSION");
    if name == "macp-runtime" && version == want {
        Ok(())
    } else {
        Err(format!(
            "the server reports {name} {version}, but the in-process target is macp-runtime {want}; \
             hash vectors count only against the same implementation and release"
        ))
    }
}

/// Connect the network target `args` names, apply the tokens and, with
/// `--hash-vectors in-process`, check the release.
async fn network_target(args: &Args) -> Result<Box<dyn ConformanceTarget>, String> {
    let checked = |r: Result<(String, String), crate::TargetError>| {
        r.map_err(|e| e.to_string())
            .and_then(same_release)
            .map_err(|e| format!("--hash-vectors in-process: {e}"))
    };
    match args.transport {
        Transport::Grpc => {
            let mut t = GrpcTarget::connect(&args.target)
                .await
                .map_err(|e| e.to_string())?;
            for (s, tok) in &args.tokens {
                t = t.with_token(s.clone(), tok.clone());
            }
            if args.hashes_in_process {
                checked(t.runtime_info().await)?;
            }
            Ok(Box::new(t))
        }
        #[cfg(feature = "http")]
        Transport::Http => {
            let mut t = crate::HttpTarget::connect(&args.target)
                .await
                .map_err(|e| e.to_string())?;
            for (s, tok) in &args.tokens {
                t = t.with_token(s.clone(), tok.clone());
            }
            if args.hashes_in_process {
                checked(t.runtime_info().await)?;
            }
            Ok(Box::new(t))
        }
        #[cfg(not(feature = "http"))]
        Transport::Http => unreachable!("refused in parse"),
    }
}

/// Run the CLI: `prog` names it in messages (`macp-conform`, or `macp conform`
/// when another binary embeds it) and `args` excludes the program name. The
/// exit status is the one documented in the [module docs](self).
pub async fn main(prog: &str, args: impl IntoIterator<Item = String>) -> ExitCode {
    let args = match parse(prog, args) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{prog}: {e}\n");
            }
            eprintln!("{}", USAGE.replacen("macp-conform", prog, 1));
            return ExitCode::from(2);
        }
    };
    let set = match (&args.fixtures, &args.bundle) {
        (Some(dir), _) => FixtureSet::from_dir(dir),
        (_, Some(b)) => FixtureSet::from_bundle(b, args.sha256sums.as_deref()),
        _ => unreachable!("checked in parse"),
    };
    let set = match set {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{prog}: {e}");
            return ExitCode::from(2);
        }
    };
    let mut target: Box<dyn ConformanceTarget> = if args.target == "in-process" {
        Box::new(InProcessTarget::new())
    } else {
        match network_target(&args).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{prog}: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let opts = RunOptions::default()
        .with_level(args.level)
        .with_multi_round(args.multi_round)
        .with_strict(args.strict);
    let report = if args.hashes_in_process {
        run_with_hash_target(target.as_mut(), &InProcessTarget::new(), &set, &opts).await
    } else {
        run(target.as_mut(), &set, &opts).await
    };
    if !args.quiet {
        eprint!("{}", report.render_text());
    }
    if let Some(path) = &args.report {
        let json = match serde_json::to_string_pretty(&report) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("{prog}: cannot serialize the report: {e}");
                return ExitCode::from(2);
            }
        };
        if path == "-" {
            println!("{json}");
        } else if let Err(e) = std::fs::write(path, json + "\n") {
            eprintln!("{prog}: cannot write {path}: {e}");
            return ExitCode::from(2);
        }
    }
    if report.is_failure() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Args, String> {
        parse("macp-conform", v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn transport_defaults_to_grpc_whatever_the_scheme() {
        let a = args(&["--target", "http://127.0.0.1:8080", "--fixtures", "f"]).unwrap();
        assert_eq!(a.transport, Transport::Grpc);
    }

    #[test]
    fn transport_flag() {
        let a = args(&[
            "--transport",
            "grpc",
            "--target",
            "grpc://h:1",
            "--fixtures",
            "f",
        ]);
        assert_eq!(a.unwrap().transport, Transport::Grpc);
        let e = args(&["--transport", "ws", "--target", "x", "--fixtures", "f"]);
        assert!(e.unwrap_err().contains("grpc or http"));
        let http = args(&[
            "--transport",
            "http",
            "--target",
            "http://h:1",
            "--fixtures",
            "f",
        ]);
        if cfg!(feature = "http") {
            assert_eq!(http.unwrap().transport, Transport::Http);
        } else {
            assert!(http.unwrap_err().contains("--features http"));
        }
        let e = args(&[
            "--transport",
            "http",
            "--target",
            "in-process",
            "--fixtures",
            "f",
        ]);
        assert!(e.is_err());
    }
}
