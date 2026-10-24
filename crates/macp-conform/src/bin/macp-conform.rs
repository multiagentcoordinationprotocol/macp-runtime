//! `macp-conform`: run the MACP conformance fixtures against an
//! implementation and report the level it reaches.
//!
//! ```text
//! macp-conform --target grpc://127.0.0.1:50051 --fixtures spec/schemas/conformance --level standard
//! ```
//!
//! Exit status: 0 when no selected fixture failed, 1 when one did (or, with
//! `--strict`, when a level is incomplete), 2 when the run could not start.

use macp_conform::{
    run, ConformanceTarget, FixtureSet, GrpcTarget, InProcessTarget, Level, RunOptions,
};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: macp-conform --target <grpc://host:port | grpcs://host:port | in-process>
                    (--fixtures <dir> | --bundle <macp-conformance-X.tar.gz> [--sha256sums <file>])
                    [--level core|decision|standard] [--multi-round] [--strict]
                    [--report <path>] [--token <sender>=<token>]... [--quiet]

  --target       the implementation under test: a MACPRuntimeService endpoint
                 (Server profile) or this crate's in-process target (Embedded)
  --fixtures     a spec schemas/conformance/ directory
  --bundle       a spec release bundle; with --sha256sums, its checksum is verified
  --level        run this level and every level below it (default: standard)
  --multi-round  also run the optional multi_round fixtures
  --strict       treat an incomplete level (skipped fixtures) as failure
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
    report: Option<String>,
    tokens: Vec<(String, String)>,
    quiet: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        target: String::new(),
        fixtures: None,
        bundle: None,
        sha256sums: None,
        level: Level::Standard,
        multi_round: false,
        strict: false,
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
                println!("macp-conform {}", env!("CARGO_PKG_VERSION"));
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
    if a.sha256sums.is_some() && a.bundle.is_none() {
        return Err("--sha256sums needs --bundle".into());
    }
    Ok(a)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse() {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("macp-conform: {e}\n");
            }
            eprintln!("{USAGE}");
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
            eprintln!("macp-conform: {e}");
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
                Box::new(t)
            }
            Err(e) => {
                eprintln!("macp-conform: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let opts = RunOptions::default()
        .with_level(args.level)
        .with_multi_round(args.multi_round)
        .with_strict(args.strict);
    let report = run(target.as_mut(), &set, &opts).await;
    if !args.quiet {
        eprint!("{}", report.render_text());
    }
    if let Some(path) = &args.report {
        let json = match serde_json::to_string_pretty(&report) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("macp-conform: cannot serialize the report: {e}");
                return ExitCode::from(2);
            }
        };
        if path == "-" {
            println!("{json}");
        } else if let Err(e) = std::fs::write(path, json + "\n") {
            eprintln!("macp-conform: cannot write {path}: {e}");
            return ExitCode::from(2);
        }
    }
    if report.is_failure() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
