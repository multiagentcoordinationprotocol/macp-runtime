//! The `macp-conform` command line, as a function, so another binary (the
//! `macp` CLI's `macp conform`) can run it unchanged.
//!
//! ```text
//! macp-conform --target grpc://127.0.0.1:50051 --fixtures spec/schemas/conformance --level standard
//! ```
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
                    (--fixtures <dir> | --bundle <macp-conformance-X.tar.gz> [--sha256sums <file>])
                    [--level core|decision|standard] [--multi-round] [--strict]
                    [--hash-vectors in-process]
                    [--report <path>] [--token <sender>=<token>]... [--quiet]

  --target       the implementation under test: a MACPRuntimeService endpoint
                 (Server profile) or this crate's in-process target (Embedded)
  --fixtures     a spec schemas/conformance/ directory
  --bundle       a spec release bundle; with --sha256sums, its checksum is verified
  --level        run this level and every level below it (default: standard)
  --multi-round  also run the optional multi_round fixtures
  --strict       treat an incomplete level (skipped fixtures) as failure
  --hash-vectors in-process
                 with a gRPC target, run the cmt-hash vectors through this
                 crate's in-process target instead (a server has no hash RPC).
                 The server must report itself as macp-runtime at this
                 runner's version: the spec counts the vectors only against
                 the same implementation and release
  --report       write the JSON report here ('-' for stdout)
  --token        bearer token to use for a sender (default: the sender's name,
                 as a server in MACP_ALLOW_INSECURE=1 development mode expects)
  --quiet        no human-readable summary on stderr";

struct Args {
    target: String,
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
        return Err("--hash-vectors in-process is for a gRPC target".into());
    }
    if a.sha256sums.is_some() && a.bundle.is_none() {
        return Err("--sha256sums needs --bundle".into());
    }
    Ok(a)
}

/// The in-process target is the reference kernel at this runner's version,
/// so its hash results stand in for the server's only when the server is
/// that implementation at that release (spec `docs/conformance.md`).
async fn same_release(t: &mut GrpcTarget) -> Result<(), String> {
    let (name, version) = t.runtime_info().await.map_err(|e| e.to_string())?;
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
        match GrpcTarget::connect(&args.target).await {
            Ok(mut t) => {
                for (s, tok) in &args.tokens {
                    t = t.with_token(s.clone(), tok.clone());
                }
                if args.hashes_in_process {
                    if let Err(e) = same_release(&mut t).await {
                        eprintln!("{prog}: --hash-vectors in-process: {e}");
                        return ExitCode::from(2);
                    }
                }
                Box::new(t)
            }
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
