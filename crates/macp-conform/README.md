# macp-conform

Runs the MACP spec's conformance fixtures (`schemas/conformance/*.json`) and
commitment-hash vectors (`schemas/conformance/cmt-hash/*.json`) against an
implementation, then reports the conformance level it reaches (Core,
Core + Decision, Standard modes, plus the optional `multi_round` group) as JSON
that validates against [`schema/macp-conform-report.schema.json`](schema/macp-conform-report.schema.json).

There are three targets:

| Target | Profile | How |
|---|---|---|
| `InProcessTarget` | Embedded | `macp_modes::step` and `macp_modes::replay` directly. No transport, no storage, injectable clock. |
| `GrpcTarget` (feature `grpc`) | Server | Any `macp.v1.MACPRuntimeService`, through `Send`, `GetSession` and `StreamSession`. |
| `HttpTarget` (feature `http`) | Server | Any runtime serving the HTTP/JSON binding (RFC-MACP-0006 §4): `POST {base}/macp/v1/Send`, `GetSession`, and `StreamSession` as server-sent events. |

To test another implementation, implement `ConformanceTarget` for it. The
trait itself has no transport dependency.

## CLI

```sh
cargo install macp-conform --features grpc

# A running server. --fixtures takes a spec checkout's schemas/conformance:
macp-conform --target grpc://127.0.0.1:50051 --fixtures ../spec/schemas/conformance --level standard

# A macp-runtime server at this runner's release, with the commitment-hash
# vectors run in-process (see "What a Server target cannot show"):
macp-conform --target grpc://127.0.0.1:50051 --fixtures ../spec/schemas/conformance \
  --multi-round --hash-vectors in-process

# A release bundle, checked against its SHA256SUMS:
macp-conform --target grpc://127.0.0.1:50051 \
  --bundle macp-conformance-1.2.0.tar.gz --sha256sums SHA256SUMS --multi-round

# The same server's HTTP/JSON binding (a macp-runtime built with the
# http-listener feature and started with MACP_HTTP_BIND_ADDR=127.0.0.1:8080).
# Needs this crate's http feature: cargo install macp-conform --features grpc,http
macp-conform --transport http --target http://127.0.0.1:8080 \
  --fixtures ../spec/schemas/conformance --multi-round --strict --hash-vectors in-process

# The reference kernel, in-process:
macp-conform --target in-process --fixtures ../spec/schemas/conformance --multi-round
```

The flags are `--transport grpc|http` (default `grpc`; see below),
`--level core|decision|standard` (default `standard`),
`--multi-round`, `--strict` (an incomplete group fails the run),
`--hash-vectors in-process` (network targets only; see below), `--report
PATH|-` (the JSON report), `--token SENDER=TOKEN` (repeatable; the default
bearer token is the sender's name, which is what a server started with
`MACP_ALLOW_INSECURE=1` accepts) and `--quiet`.

`--transport` picks the binding a network target is driven over. A URL's
scheme never does: `http://host:port` with the default transport is still gRPC
in the clear, as it always was, and `--transport http` makes the same URL the
HTTP binding's base URL (`https://` for TLS, with an optional path prefix).
`--transport http` needs the `http` feature (the CLI itself needs `grpc`);
without it the run stops with status 2.

The exit status is 0 when no selected group failed, 1 when one did, and 2 on a
usage or harness error.

## What a Server target cannot show

Everything in this section applies to both bindings: an HTTP run reaches the
same claim as a gRPC run against the same server, with the same skips and
unverified assertions. A gRPC server exposes no clock, mode state, replay or commitment-hash API.
The runner reports what it could not check instead of guessing, and applies
the spec's two Server-profile rules (`docs/conformance.md`, Profiles):

- **Clock-dependent fixtures** (TTL expiry, implicit accept) are `skipped`
  with reason `needs-clock-control`, and **do not count against a level**.
  The group summary counts them in both `skipped` and `excused`, and a group
  whose only skips are excused is `pass`, with or without `--strict`. This
  applies to Server targets only: an Embedded target is expected to drive
  them with a controlled clock, so there a skip leaves the group
  `incomplete`.
- **Commitment-hash vectors** (`cmt_hash_001`-`010`, part of Core) are
  `skipped` with reason `needs-hash-api`, which leaves Core, and so every
  level, `incomplete`. A Server claim counts them when they pass in-process
  against the same implementation and release. `--hash-vectors in-process`
  does that: it runs the vectors through this crate's in-process target (the
  reference kernel at the runner's own version) after checking, through
  `Initialize`, that the server reports itself as `macp-runtime` at that same
  version; otherwise the run stops with status 2. The report names the hash
  target in `hash_vectors_target` and the claim ends in
  `(hash vectors in-process)`.
- `expected_mode_state` and `verify_replay_equivalence` show up in a passing
  fixture's `unverified` list.

So against a `macp-runtime` server of the same release:

```text
standard     Pass: 15 passed, 0 failed, 1 skipped (1 not counted), 0 errors, 18 unverified
claim: Standard modes + multi_round, Server profile (hash vectors in-process)
```

### What an HTTP run leaves unverified

The HTTP target encodes requests itself, independently of any runtime's JSON
codec, and keeps that encoder minimal: every Envelope goes out with
`payload_b64` (the payload's Protobuf bytes, which RFC-MACP-0001 §10.12
always permits), so the runtime is never tested against its own codec. As a
consequence an HTTP run does **not** exercise:

- the JSON `payload` form of a submitted Envelope (the §10.12 payload
  selection and the §10.13 conversion to Protobuf bytes on the way in). For
  macp-runtime, that path is covered by its own codec and HTTP tests instead;
- the binding's request-level errors other than those a fixture provokes, its
  transport statuses, CORS, discovery and `Last-Event-ID` resume (the target
  always subscribes from `after_sequence = 0`).

Responses are read leniently (absent or `null` fields take their default,
§10.10), and a history Envelope that arrives with a JSON `payload` is
re-encoded to Protobuf bytes through a small table of the payload messages the
fixtures use; one for any other `(mode, message_type)` is a target error. The
target speaks HTTP/1.1 only (the binding's MUST), over rustls with the WebPKI
roots for `https://`. History is the `StreamSession` subscribe stream, read
until it ends after the terminal Envelope, until `event: error`
`SESSION_NOT_OPEN` (a cancelled or expired session), or until it is idle for
250 ms (`HttpTarget::with_stream_idle`).

To test another server implementation the same way, drive its own in-process
hash code with the library's `run_with_hash_target`, which runs the session
fixtures against one target and the vectors against another. Whether the two
are the same implementation and release is the caller's to vouch for.

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

Neither `grpc` nor `http` is on by default: without them the crate depends on
no transport (no tonic, no HTTP client).

This is a tool crate. It ships in lockstep with the rest of the workspace, but
it is not one of the embeddable crates covered by
[docs/stability.md](../../docs/stability.md). The report's shape is versioned
by its own `report_format` field.
