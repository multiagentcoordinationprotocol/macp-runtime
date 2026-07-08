# macp-runtime v0.5.0

Reference runtime for the Multi-Agent Coordination Protocol (MACP).

This runtime implements the current MACP core/service surface, five standards-track modes, and one built-in extension mode. The focus of this release is freeze-readiness for SDKs and real-world unary and streaming integrations: strict `SessionStart`, mode-semantic correctness, authenticated senders, bounded resources, durable restart recovery, and extension mode lifecycle management.

## What changed in v0.5.0

The improvement-plan release (see `CHANGELOG.md` for the complete list):

- **Security**: dev-mode auth is opt-in (`MACP_ALLOW_INSECURE=1` required with no auth configured); HS256 removed from the default JWT allowlist; `WatchSignals` authenticated; JWKS hardening (timeouts, stale-cache grace, kid selection, single-flight refresh); handoff implicit-accept timing no longer trusts client timestamps.
- **Determinism**: session-bound `max_suspend_ms` (recorded at SessionStart, used by replay); passive-subscribe sequence contract (1-based accepted-envelope ordinals, exclusive `after_sequence`, compaction-stable); extension-mode version binding recorded and replayed.
- **Durability**: RocksDB appends fsync before ack; atomic Redis `replace_log`; corrupt-entry parity across backends; crash-atomic snapshot writes.
- **Throughput**: per-session locking replaces the global write lock (−38% on the fsync-contended path); bounded memory (eviction covers registry, log cache, stream channels); tonic limits + graceful shutdown.
- **Operations**: opt-in Prometheus metrics endpoint; disk retention/GC; replay-consistency validation at recovery; `MACP_POLICIES_DIR` (RFC-0012 §9 read-only registry profile); pluggable ingress `PolicyEngine`.
- **Wire**: `ext.multi_round.v1` on the canonical proto payload (JSON still accepted for replay compatibility); Task mode accepts an external orchestrator (RFC-0009 conformance); quorum `threshold` is the RFC-0012 §4.2 approval bar.

## What changed in v0.4.0

- **Strict canonical `SessionStart` for standard modes**
  - no empty payloads
  - no implicit default mode
  - explicit `mode_version`, `configuration_version`, and positive `ttl_ms`
  - explicit unique participants for standards-track modes
- **Decision Mode authority clarified**
  - initiator/coordinator may emit `Proposal` and `Commitment`
  - participants emit `Evaluation`, `Objection`, and `Vote`
  - duplicate `proposal_id` values are rejected
  - votes are tracked per proposal, per sender
- **Proposal Mode commitment gating fixed**
  - `Commitment` is accepted only after acceptance convergence or a terminal rejection
- **Security boundary added**
  - TLS-capable startup
  - authenticated sender derivation via bearer token or dev header mode
  - per-request authorization
  - payload size limits
  - rate limiting
- **Durable local persistence**
  - per-session append-only log files and session snapshots via `FileBackend`
  - crash recovery with dedup state reconciliation
  - atomic writes (tmp file + rename) prevent partial-write corruption
- **Authoritative accepted history**
  - log append failures are now fatal — messages are not acknowledged without a durable record
  - session state is rebuilt from append-only logs on startup via replay (no snapshot dependency)
  - `LogEntry` enriched with `session_id`, `mode`, `macp_version` for self-describing replay
- **Session ID security policy**
  - session IDs must be UUID v4/v7 (hyphenated lowercase) or base64url tokens (22+ chars)
  - weak/human-readable IDs are rejected with `INVALID_SESSION_ID`
- **Signal enforcement**
  - Signals are strictly ambient — non-empty `session_id` or `mode` is rejected
- **StreamSession enabled**
  - `Initialize` advertises `stream: true`
  - `StreamSession` provides per-session bidirectional streaming of accepted envelopes
  - Passive subscribe (RFC-MACP-0006-A1): a `subscribe_session_id` + `after_sequence` frame replays accepted history and then delivers live envelopes; allowed for declared participants, the initiator, or observer identities
  - `WatchModeRegistry` fires live `RegistryChanged` events on mode register/unregister/promote
  - `WatchRoots` implemented (basic: send initial state, hold stream open)
- **Extension mode lifecycle**
  - `multi_round` demoted from standards-track to built-in extension (`ext.multi_round.v1`)
  - `ListExtModes` returns extension mode descriptors
  - `RegisterExtMode` dynamically registers new extension modes with a passthrough handler
  - `UnregisterExtMode` removes dynamically registered extensions (built-in modes protected)
  - `PromoteMode` promotes extensions to standards-track with optional identifier rename
- **Pluggable authentication chain**
  - JWT bearer resolver validates signature, issuer, audience, and expiration against a JWKS (inline JSON or URL-fetched with TTL cache); `RS256`, `ES256`, and `HS256` supported
  - Static bearer resolver maps opaque tokens to identities via `MACP_AUTH_TOKENS_FILE`/`MACP_AUTH_TOKENS_JSON`
  - Resolvers run in chain order (JWT → static); dev-mode fallback only when both are absent
  - Identities carry capability flags: `allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, `is_observer`
- **Governance policy framework (RFC-MACP-0012)**
  - `RegisterPolicy`, `UnregisterPolicy`, `GetPolicy`, `ListPolicies`, `WatchPolicies` RPCs
  - Per-mode rule schemas (voting, objection handling, quorum thresholds, acceptance, assignment, handoff acceptance)
  - Policies evaluated at commitment time; version binding enforced at SessionStart
- **Session lifecycle observability**
  - `ListSessions` enumerates current session metadata
  - `WatchSessions` streams `Created`/`Resolved`/`Expired` events with a `Created` initial-sync on connect
- **Session extension plumbing**
  - `SessionExtensionProvider` trait and `ExtensionProviderRegistry` let hosts hook lifecycle callbacks for custom session-level extensions carried in the `extensions` map; provider errors are non-fatal
- **Pluggable storage backends**
  - File (default), in-memory, RocksDB (`rocksdb-backend` feature), Redis (`redis-backend` feature)
  - Checkpoint-based replay and terminal-session log compaction
- **Structured logging via `tracing`**
  - use `RUST_LOG` env var to control log level (e.g. `RUST_LOG=info`)
- **Per-mode metrics**
  - tracked via `src/metrics.rs`

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

`policy_version` is optional unless your policy requires it. Empty `mode` is rejected. Empty `SessionStartPayload` is rejected.

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
| `RUST_LOG` | `tracing` log level filter (e.g. `info`, `debug`) | unset |
| `MACP_ALLOW_INSECURE` | allow plaintext transport when set to `1` | unset |
| `MACP_TLS_CERT_PATH` | PEM certificate for TLS | unset |
| `MACP_TLS_KEY_PATH` | PEM private key for TLS | unset |

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

- **Tier 1 (Protocol)** — 90 scripted gRPC tests plus 8 JWT bearer auth tests: all modes, error paths, signals, version binding, dedup, suspend/resume, TLS transport, persistence/restart-replay, payload and rate limits, concurrent senders, passive subscribe, policy registry and watch streams, mode promotion, and RFC cross-cutting features
- **Tier 2 (Rig Tools)** — 5 tests using [Rig](https://rig.rs) agent framework `Tool` implementations for all MACP operations
- **Tier 3 (E2E)** — 3 tests with real OpenAI GPT-4o-mini agents coordinating through the runtime (requires `OPENAI_API_KEY`)

See `docs/testing.md` for full details on running locally, in CI, or against a hosted runtime.

## Releasing

The workspace publishes to crates.io as seven crates that share one version
(`0.5.0`), pinned in `[workspace.package]`. Internal dependencies are declared
as `{ version = "...", path = "..." }`, so the same manifests build locally
from `path` and resolve from the registry once published.

Releases are automated by `.github/workflows/publish.yml`, triggered by pushing
a version tag:

```bash
git tag v0.5.0
git push origin v0.5.0
```

The publish workflow verifies the tag against the workspace version, checks
that `CHANGELOG.md` has a section for the release, runs `cargo semver-checks`
against the last published release, publishes the workspace, and creates a
GitHub Release with the CHANGELOG section as its notes.

The workflow verifies the tag matches the workspace version, then publishes
bottom-up so each crate's dependencies are already on the index:

```
macp-pb → macp-core → macp-storage → macp-policy → macp-modes → macp-auth → macp-runtime
```

A crate whose version is already live is skipped, so a re-run after a partial
failure is safe. Publishing requires a `CARGO_REGISTRY_TOKEN` repository secret.
To validate without uploading, run the workflow manually (`workflow_dispatch`)
with the default `dry_run` enabled.

## Development notes

- The RFC/spec repository remains the normative source for protocol semantics.
- Five standards-track modes use the canonical `macp.mode.*` identifiers.
- `multi_round` is a built-in extension (`ext.multi_round.v1`) — not standards-track, but ships with the runtime and enforces strict `SessionStart`.
- Extension modes can be dynamically registered, unregistered, and promoted via `RegisterExtMode`, `UnregisterExtMode`, and `PromoteMode` RPCs.
- `StreamSession` is enabled and binds one gRPC stream to one session, emitting accepted envelopes in order.
- `WatchSignals` broadcasts ambient Signal envelopes to all subscribers in real time.

See `docs/README.md` and `docs/examples.md` for the updated local development and usage guidance.
