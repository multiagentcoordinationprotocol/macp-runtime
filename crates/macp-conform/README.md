# macp-conform

Runs the MACP spec's conformance fixtures (`schemas/conformance/*.json`) and
commitment-hash vectors (`schemas/conformance/cmt-hash/*.json`) against an
implementation, then reports the conformance level it reaches (Core,
Core + Decision, Standard modes, plus the optional `multi_round` group) as JSON
that validates against [`schema/macp-conform-report.schema.json`](schema/macp-conform-report.schema.json).

There are two targets:

| Target | Profile | How |
|---|---|---|
| `InProcessTarget` | Embedded | `macp_modes::step` and `macp_modes::replay` directly. No transport, no storage, injectable clock. |
| `GrpcTarget` (feature `grpc`) | Server | Any `macp.v1.MACPRuntimeService`, through `Send`, `GetSession` and `StreamSession`. |

To test another implementation, implement `ConformanceTarget` for it. The
trait itself has no transport dependency.

## CLI

```sh
cargo install macp-conform --features grpc

# A running server. --fixtures takes a spec checkout's schemas/conformance:
macp-conform --target grpc://127.0.0.1:50051 --fixtures ../spec/schemas/conformance --level standard

# A release bundle, checked against its SHA256SUMS:
macp-conform --target grpc://127.0.0.1:50051 \
  --bundle macp-conformance-1.2.0.tar.gz --sha256sums SHA256SUMS --multi-round

# The reference kernel, in-process:
macp-conform --target in-process --fixtures ../spec/schemas/conformance --multi-round
```

The flags are `--level core|decision|standard` (default `standard`),
`--multi-round`, `--strict` (an incomplete group fails the run), `--report
PATH|-` (the JSON report), `--token SENDER=TOKEN` (repeatable; the default
bearer token is the sender's name, which is what a server started with
`MACP_ALLOW_INSECURE=1` accepts) and `--quiet`.

The exit status is 0 when no selected group failed, 1 when one did, and 2 on a
usage or harness error.

## What a Server target cannot show

A gRPC server exposes no clock, mode state, replay or commitment-hash API. The
runner reports what it could not check instead of guessing:

- clocked fixtures are `skipped` with reason `needs-clock-control`;
- `cmt-hash` vectors are `skipped` with reason `needs-hash-api`;
- `expected_mode_state` and `verify_replay_equivalence` show up in a passing
  fixture's `unverified` list.

A group with skipped fixtures is `incomplete`, and an incomplete group never
counts toward `achieved_level`. Over gRPC that means Core, and therefore every
level, stays incomplete until servers expose a commitment-hash API.

## Library

```rust
use macp_conform::{run, FixtureSet, InProcessTarget, Level, RunOptions};

async fn check() -> Result<(), Box<dyn std::error::Error>> {
    let set = FixtureSet::from_dir("spec/schemas/conformance")?;
    let report = run(
        &mut InProcessTarget::new(),
        &set,
        &RunOptions::default().with_level(Level::Standard).with_multi_round(true),
    )
    .await;
    assert!(!report.is_failure(), "{}", report.render_text());
    Ok(())
}
```

## Stability

This is a tool crate. It ships in lockstep with the rest of the workspace, but
it is not one of the embeddable crates covered by
[docs/stability.md](../../docs/stability.md). The report's shape is versioned
by its own `report_format` field.
