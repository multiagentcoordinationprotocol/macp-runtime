# macp-runtime

Reference runtime for the Multi-Agent Coordination Protocol (MACP).

This runtime implements the current MACP core/service surface, five standards-track modes, and one built-in extension mode: strict `SessionStart`, mode-semantic correctness, authenticated senders, bounded resources, durable restart recovery, and extension mode lifecycle management.

## Recent changes

Release-by-release history lives in [`CHANGELOG.md`](CHANGELOG.md), which is
generated from the commit history. A few changes since the 0.5.x line need
action from consumers rather than just reading:

- **The handoff implicit accept is recorded (0.8.0).** In a
  `macp.mode.handoff.v1` session started at `semantics_rev >= 2`, the runtime
  mints its own `HandoffAccept` when the offer deadline passes, as a real
  history entry that consumes an accepted ordinal and reaches `StreamSession`
  subscribers. The `implicit-accept:` `message_id` prefix is reserved against
  client envelopes, and a client-sent `HandoffAccept` with `implicit = true` is
  rejected. See
  [Handoff implicit accept](docs/modes.md#implicit-accept-rfc-macp-0010-51).
- **Mode-state records are sealed (0.8.0).** The `pub` records behind
  `session.mode_state`, and `PersistedSession`, are `#[non_exhaustive]`, so code
  outside the owning crate can no longer construct one by struct literal. This
  is breaking for external callers that did, and makes every subsequent field
  addition additive. See
  [Mode-state records are sealed](docs/modes.md#mode-state-records-are-sealed).
- **Governance policies are validated at registration time.** Rules the
  canonical RFC-MACP-0012 schemas forbid are now refused when the policy is
  registered or preloaded, so a policy file an older runtime accepted can refuse
  a server start. See
  [Upgrading into registration-time policy validation](docs/deployment.md#upgrading-into-registration-time-policy-validation).

## Implemented modes

Standards-track modes:

- `macp.mode.decision.v1`
- `macp.mode.proposal.v1`
- `macp.mode.task.v1`
- `macp.mode.handoff.v1`
- `macp.mode.quorum.v1`

Built-in extension modes:

- `ext.multi_round.v1`

## Runtime behavior that SDKs should assume

### Session bootstrap

For all standards-track modes and built-in extensions, `SessionStartPayload` must include:

- `participants`
- `mode_version`
- `configuration_version`
- `ttl_ms`

`policy_version` MUST be present in the payload (RFC-MACP-0001 §7.1), but MAY be
empty — an empty value resolves the session to `policy.default`. See
[Policy](docs/policy.md) for resolution and version-binding rules. Empty `mode`
is rejected. Empty `SessionStartPayload` is rejected.

### Mode authority

Only `Commitment` authority is granted regardless of participant-list
membership: the `SessionStart` sender may emit `Commitment` (and may invoke
`CancelSession`, which the runtime — not the initiator — emits as a session
entry) whether or not the initiator appears in `participants`. Authority for
every other message is **mode-specific, not uniform**: Decision requires the
initiator to be a declared participant to emit `Proposal`, `Evaluation`,
`Objection`, or `Vote` (RFC-MACP-0007 §2), while Task's `TaskRequest` and
Quorum's `ApprovalRequest` are authorized by the initiator role itself, with
no participant-list membership required — an external orchestrator can drive
either without listing itself. Handoff is the exception in the other
direction: `HandoffOffer` is likewise role-authorized, but `SessionStart`
itself requires the initiator to be one of the two declared parties, since
the initiator is a transfer party rather than just a coordinator. See
[Modes](docs/modes.md) for the full per-mode authority rules.

### Streaming

- `StreamSession` binds one gRPC stream to one session and emits accepted envelopes in order
  - Passive subscribe (RFC-MACP-0006-A1): a `subscribe_session_id` + `after_sequence` frame replays accepted history and then delivers live envelopes; allowed for declared participants, the initiator, or observer identities
- `WatchSignals` broadcasts ambient Signal envelopes to all subscribers in real time; Signals never enter session history

### Session lifecycle observability

`ListSessions` enumerates current session metadata in bounded pages (`page_size`
is clamped to a server maximum; pass `next_page_token` back verbatim until it
comes back empty). `WatchSessions` streams `Created`, `Resolved`, `Expired`,
`Suspended`, `Resumed`, and `Cancelled` events, with a `Created` initial sync on
connect — see [WatchSessions](docs/API.md#watchsessions).

### Security

In production, requests should be authenticated with a bearer token. The runtime derives `Envelope.sender` from the authenticated identity and rejects spoofed sender values.

For local development, opt into insecure transport with:

```bash
MACP_ALLOW_INSECURE=1
```

When no auth resolvers are configured (no `MACP_AUTH_TOKENS_*` and no
`MACP_AUTH_ISSUER`), the runtime falls back to dev-mode auth: any
`Authorization: Bearer <value>` header authenticates the caller as
sender `<value>`. Use only for local development.

### Persistence

Unless `MACP_MEMORY_ONLY=1` is set, the runtime persists session and log snapshots under `MACP_DATA_DIR` (default: `.macp-data`). If a persistence file contains corrupt or incompatible JSON on startup, the runtime logs a warning to stderr and starts with empty state rather than failing.

## Configuration

### Core server configuration

| Variable | Meaning | Default |
|---|---|---|
| `MACP_BIND_ADDR` | bind address | `127.0.0.1:50051` |
| `MACP_DATA_DIR` | persistence directory | `.macp-data` |
| `MACP_MEMORY_ONLY` | disable persistence when set to `1` | unset |
| `MACP_CLEANUP_INTERVAL_SECS` | background maintenance interval: TTL expiry, eviction, and the eager observation of mode deadlines such as the handoff implicit accept (RFC-MACP-0010 §5.1(2)) | `60` |
| `RUST_LOG` | `tracing` log level filter (e.g. `info`, `debug`) | unset |
| `MACP_ALLOW_INSECURE` | allow plaintext transport when set to `1` | unset |
| `MACP_TLS_CERT_PATH` | PEM certificate for TLS | unset |
| `MACP_TLS_KEY_PATH` | PEM private key for TLS | unset |
| `MACP_POLICIES_DIR` | directory of governance policy JSON files preloaded at startup; a file that fails validation aborts startup, and the wire registry becomes read-only | unset |
| `MACP_POLICIES_DRY_RUN` | validate `MACP_POLICIES_DIR` and exit `0`/`1` without starting the server, when set to `1` | unset |

### Authentication and authorization

| Variable | Meaning | Default |
|---|---|---|
| `MACP_AUTH_TOKENS_JSON` | inline static bearer token config JSON | unset |
| `MACP_AUTH_TOKENS_FILE` | path to static bearer token config JSON | unset |
| `MACP_AUTH_ISSUER` | JWT resolver expected `iss` claim (enables JWT auth) | unset |
| `MACP_AUTH_AUDIENCE` | JWT resolver expected `aud` claim | `macp-runtime` |
| `MACP_AUTH_JWKS_JSON` | inline JWKS document used to validate JWTs | unset |
| `MACP_AUTH_JWKS_URL` | JWKS endpoint URL (fetched + cached) | unset |
| `MACP_AUTH_JWKS_TTL_SECS` | JWKS cache TTL when fetched from URL | `300` |

Auth is layered as a resolver chain: configured JWT first, then static
bearer, with a dev-mode fallback only when both are absent. JWT tokens
supply MACP scopes via a `macp_scopes` claim matching the static token
schema.

Token JSON may be either a raw list or an object with a `tokens` array. Example:

```json
{
  "tokens": [
    {
      "token": "demo-coordinator-token",
      "sender": "coordinator",
      "allowed_modes": [
        "macp.mode.decision.v1",
        "macp.mode.quorum.v1"
      ],
      "can_start_sessions": true,
      "max_open_sessions": 25
    },
    {
      "token": "demo-worker-token",
      "sender": "worker",
      "allowed_modes": [
        "macp.mode.task.v1"
      ],
      "can_start_sessions": false,
      "can_manage_mode_registry": false
    }
  ]
}
```

### Resource limits

| Variable | Meaning | Default |
|---|---|---|
| `MACP_MAX_PAYLOAD_BYTES` | max envelope payload size | `1048576` |
| `MACP_SESSION_START_LIMIT_PER_MINUTE` | per-sender session start limit | `60` |
| `MACP_MESSAGE_LIMIT_PER_MINUTE` | per-sender message limit | `600` |
| `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` | `ListSessions` page size when the request sends `page_size = 0` | `100` |
| `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` | hard cap a requested `ListSessions` `page_size` is clamped to | `1000` |

## Quick start

### Production-style startup with TLS

```bash
export MACP_TLS_CERT_PATH=/path/to/server.crt
export MACP_TLS_KEY_PATH=/path/to/server.key
export MACP_AUTH_TOKENS_FILE=/path/to/tokens.json
cargo run
```

### Local development startup

```bash
export MACP_ALLOW_INSECURE=1
cargo run
```

With no auth tokens configured, clients authenticate by sending their
sender identity as a bearer token (e.g. `Authorization: Bearer agent://alice`).

### Running the example clients

The example clients in `src/bin` assume the local development startup shown above.

```bash
cargo run --bin client
cargo run --bin proposal_client
cargo run --bin task_client
cargo run --bin handoff_client
cargo run --bin quorum_client
cargo run --bin multi_round_client
cargo run --bin fuzz_client
```

## Client libraries

This repository is the Rust reference runtime — the server side. Agent-side
clients are maintained as separate SDKs:

- [`macp-sdk-python`](https://github.com/multiagentcoordinationprotocol/macp-sdk-python)
- [`macp-sdk-typescript`](https://github.com/multiagentcoordinationprotocol/macp-sdk-typescript)

Installation, client configuration, and client-side usage are documented in
those repositories, not here. The example clients in `src/bin` are Rust
development aids for exercising a local server, not a supported SDK.

## Freeze-profile capability summary

| RPC | Status |
|---|---|
| `Initialize` | implemented |
| `Send` | implemented |
| `StreamSession` | implemented (active + passive subscribe) |
| `GetSession` | implemented |
| `ListSessions` | implemented |
| `WatchSessions` | implemented |
| `CancelSession` | implemented |
| `SuspendSession` | implemented (initiator-only; suspended time is banked against the session's `max_suspend_ms`) |
| `ResumeSession` | implemented (returns a `SUSPENDED` session to `OPEN`, banking the pause into the TTL deadline) |
| `GetManifest` | implemented |
| `ListModes` | implemented |
| `ListExtModes` | implemented |
| `RegisterExtMode` | implemented |
| `UnregisterExtMode` | implemented |
| `PromoteMode` | implemented |
| `WatchModeRegistry` | implemented |
| `ListRoots` | implemented |
| `WatchRoots` | implemented |
| `WatchSignals` | implemented |
| `RegisterPolicy` | implemented |
| `UnregisterPolicy` | implemented |
| `GetPolicy` | implemented |
| `ListPolicies` | implemented |
| `WatchPolicies` | implemented |

## Architecture

```
Client Request
       |
  [Transport/gRPC] -- macp-runtime: src/server.rs
       |
  [Auth Chain]    -- macp-auth  (JWT → static → dev fallback)
       |
  [Coordination Kernel] -- macp-runtime: src/runtime.rs
       |
  [Mode Registry] -- macp-modes: mode_registry.rs
       |            \
  [Mode Logic]     [Discovery + Extension Lifecycle]
   macp-modes      ListModes, ListExtModes, GetManifest,
                   RegisterExtMode, UnregisterExtMode, PromoteMode
       |
  [Policy Engine] -- macp-policy  (commitment-time evaluation via the
       |             macp-core PolicyEvaluator trait)
  [Storage Layer] -- macp-storage  (log_store + backends)
       |
  [Replay] -- macp-runtime: src/replay.rs
```

The runtime is a Cargo workspace. The root `macp-runtime` crate is the kernel +
gRPC server + binary; it re-exports the lower crates so the historical
`macp_runtime::*` paths are preserved. `macp-core` (vocabulary + the
`PolicyEvaluator` trait) and `macp-pb` (generated protobuf messages) are
transport-free, and modes evaluate governance through an injected evaluator
rather than a concrete policy engine.

See `docs/architecture.md` and `CLAUDE.md` → "Workspace crates" for detailed
layer and crate descriptions.

## Project structure

The runtime is a Cargo workspace. The root `macp-runtime` crate is the kernel +
gRPC server + binary; the lower crates form a one-way dependency graph with
`macp-core` at the base (see `CLAUDE.md` → "Workspace crates").

```text
runtime/
├── src/                    # macp-runtime crate: kernel + gRPC server + binary
│   ├── main.rs             # server startup, TLS, persistence, auth wiring
│   ├── server.rs           # gRPC adapter (24 RPCs) and envelope validation
│   ├── runtime.rs          # coordination kernel, mode dispatch, lifecycle bus
│   ├── replay.rs           # session rebuild from append-only log
│   ├── stream_bus.rs       # per-session broadcast channels
│   ├── metrics.rs          # per-mode metrics counters
│   ├── error.rs            # thin re-export shim for macp_core::error
│   ├── session.rs          # thin re-export shim for macp_core::session
│   ├── extensions/         # session-extension provider plumbing
│   │   ├── provider.rs     # SessionExtensionProvider trait
│   │   └── registry.rs     # ExtensionProviderRegistry
│   └── bin/                # local development example clients
├── crates/
│   ├── macp-pb/            # generated protobuf message types (prost-only, no tonic)
│   ├── macp-core/          # vocabulary: error, session, decision/policy value
│   │   │                   #   types, CommitmentRules, PolicyEvaluator trait
│   │   └── src/{error.rs, session.rs, decision.rs, mode.rs, policy/}
│   ├── macp-storage/       # append-only log, session registry, storage backends
│   │   └── src/{log_store.rs, registry.rs, storage/{file,memory,rocksdb,redis_backend,recovery}.rs}
│   ├── macp-policy/        # per-mode rule schemas, registry, DefaultPolicyEvaluator
│   │   └── src/{registry.rs, evaluator.rs, defaults.rs}
│   ├── macp-modes/         # mode implementations + registry (governance via
│   │   │                   #   an injected macp_core::PolicyEvaluator)
│   │   └── src/{mode_registry.rs, mode/{decision,proposal,task,handoff,quorum,multi_round,passthrough,util}.rs}
│   └── macp-auth/          # security layer + bearer/JWT resolver chain
│       └── src/{security.rs, auth/{chain,resolver,resolvers/{jwt_bearer,static_bearer}}.rs}
├── tests/                  # macp-runtime integration tests
│   ├── replay_round_trip.rs           # replay tests for all modes
│   ├── conformance_loader.rs          # JSON fixture runner
│   └── conformance/                   # per-mode conformance fixtures
├── integration_tests/                 # gRPC boundary tests (Tier 1/2/3, separate crate)
├── docs/
└── build.rs                           # macp.v1 service codegen via .extern_path
```

## Troubleshooting

**TLS required error on startup**
Set `MACP_ALLOW_INSECURE=1` for local development, or provide `MACP_TLS_CERT_PATH` and `MACP_TLS_KEY_PATH` for production.

**`InvalidSessionId` error**
Session IDs must be UUID v4/v7 in hyphenated lowercase form (36 chars) or base64url tokens (22+ chars). Short or human-readable IDs like `"s1"` or `"my-session"` are rejected.

**`InvalidPayload` on `SessionStart`**
For standards-track modes and built-in extensions (including `ext.multi_round.v1`), `SessionStartPayload` must include non-empty `participants`, `mode_version`, `configuration_version`, and a positive `ttl_ms`. Empty payloads are rejected.

**`Forbidden` error**
Check that the sender identity matches the session's participant list. For `Commitment` messages, only the session initiator is authorized. Verify your bearer token maps to the correct sender.

**`StorageFailed` error**
The runtime requires write access to `MACP_DATA_DIR`. Check directory permissions. Log append failures are fatal — the runtime will not acknowledge a message without a durable record.

**Proto version mismatch**
Update the `macp-proto` version in `Cargo.toml` (published on crates.io) and run `cargo build`.

## Testing

```bash
cargo test --all-targets          # Unit tests + Rust integration tests
make test-conformance             # JSON fixture-driven conformance suite
```

A separate integration test crate (`integration_tests/`) tests the runtime through the real gRPC boundary:

```bash
cargo build
cd integration_tests
MACP_TEST_BINARY=../target/debug/macp-runtime cargo test -- --test-threads=1
```

The integration suite has three tiers:

- **Tier 1 (Protocol)** — scripted gRPC tests (including JWT bearer auth): all modes, error paths, signals, version binding, dedup, suspend/resume, TLS transport, persistence/restart-replay, payload and rate limits, `ListSessions` pagination, concurrent senders, passive subscribe, policy registry (including the reserved `policy.std.` namespace and the RFC-MACP-0012 §5.2 outcome table) and watch streams, mode promotion, and RFC cross-cutting features
- **Tier 2 (Rig Tools)** — 5 tests using [Rig](https://rig.rs) agent framework `Tool` implementations for all MACP operations
- **Tier 3 (E2E)** — 3 tests with real OpenAI GPT-4o-mini agents coordinating through the runtime (requires `OPENAI_API_KEY`)

See `docs/testing.md` for full details on running locally, in CI, or against a hosted runtime.

## Releasing

The workspace publishes to crates.io as seven crates that share one version,
pinned in `[workspace.package]` in the root `Cargo.toml`. Internal dependencies
are declared as `{ version = "...", path = "..." }`, so the same manifests build
locally from `path` and resolve from the registry once published.

**Releases are automated — do not bump versions or push tags by hand.** On every
push to `main`, release-plz (`.github/workflows/release-plz.yml`, configured by
`release-plz.toml`) opens or updates a **release PR** that bumps the shared
workspace version and rewrites `CHANGELOG.md` from the
conventional-commit history. It also runs `cargo semver-checks` while computing
that PR, so an unintended API break blocks the release rather than shipping.
Merging the PR creates the per-crate git tags and one GitHub Release, then calls
two workflows directly: `publish.yml`, which runs `cargo publish --workspace`
(cargo computes the seven-crate publish order itself and waits for index
propagation, and a crate already live is skipped, so a re-run after a partial
failure is safe), and `docker.yml`, which publishes the versioned GHCR image.

Both are **called** by `release-plz.yml` rather than triggered by the
`macp-runtime-v*` tag. GitHub does not start workflow runs from events created
with the default `GITHUB_TOKEN`, so that trigger never fires for release-plz's
own tags — which is why 0.6.1 was tagged and never reached crates.io. The tag
triggers survive only as a backstop for a tag pushed by a human or a PAT.

Approving the release PR has one trap worth reading before you merge: a
`sync-integration-lock` job regenerates `integration_tests/Cargo.lock` on the
PR, which moves its head SHA, and the run you approve must be the one at the new
SHA. See [Approving a release PR](CONTRIBUTING.md#approving-a-release-pr) for the
procedure and [Published image tags](docs/deployment.md#published-image-tags)
for the container tag contract. Publishing requires a `CARGO_REGISTRY_TOKEN`
repository secret; `publish.yml` can also be dispatched manually (defaulting to
a dry run) for recovery.

## Development notes

- The RFC/spec repository remains the normative source for protocol semantics.
- Five standards-track modes use the canonical `macp.mode.*` identifiers.
- `multi_round` is a built-in extension (`ext.multi_round.v1`) — not standards-track, but ships with the runtime and enforces strict `SessionStart`.
- Extension modes can be dynamically registered, unregistered, and promoted via `RegisterExtMode`, `UnregisterExtMode`, and `PromoteMode` RPCs.
- `StreamSession` and `WatchSignals` behavior is described under [Runtime behavior that SDKs should assume](#runtime-behavior-that-sdks-should-assume) above.

See `docs/README.md` and `docs/examples.md` for the updated local development and usage guidance.
