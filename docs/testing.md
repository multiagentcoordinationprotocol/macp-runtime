# Testing

The runtime has a layered testing strategy that covers unit tests, conformance fixtures, and a separate integration test crate that exercises the full gRPC boundary. This page explains each layer, how to run the tests, and how they fit into CI/CD.

## Unit tests and conformance

The core test suite runs with standard Cargo commands:

```bash
cargo test --all-targets          # Unit tests + Rust integration tests
make test-conformance             # JSON fixture-driven conformance suite
make test-all                     # fmt -> clippy -> test -> integration -> conformance
```

Unit tests live inside `src/` modules under `#[cfg(test)]` and cover mode state machines, policy evaluation algorithms, storage backends (including compaction, legacy-format migration, and crash-recovery paths), replay logic, metrics rendering, the extension registry, JWT auth including remote-JWKS fetch/caching/rotation, the auth resolver chain, and error handling. The conformance fixtures in `tests/conformance/` define mode lifecycles as JSON files and verify that each mode's happy path and reject paths produce the expected results.

## Integration test suite

A separate Rust crate at `integration_tests/` tests the runtime through the real gRPC transport boundary. It is not part of the main Cargo build -- `cargo build --release` ignores it entirely.

The crate starts the runtime binary as a subprocess on a free port, connects as a gRPC client, and runs test scenarios against the live server. This ensures that the transport layer, authentication, serialization, and kernel logic all work together correctly.

### Test architecture

```
integration_tests/
  src/
    config.rs             -- Test target configuration (local / CI / hosted)
    server_manager.rs     -- Start/stop runtime as subprocess on free port
    helpers.rs            -- Envelope builders, payload helpers, gRPC wrappers
    macp_tools/           -- Rig Tool implementations for MACP operations
  tests/
    tier1.rs -> tier1_protocol/    -- Scripted gRPC protocol tests
    tier2.rs -> tier2_agents/      -- Rig agent tool tests
    tier3.rs -> tier3_e2e/         -- Real LLM agent tests
```

### Three tiers

**Tier 1: Protocol tests** exercise every mode through scripted gRPC calls. These tests cover the full protocol surface: `Initialize` negotiation, happy-path flows for all five standard modes plus multi-round (consolidated in `test_mode_happy_paths.rs`), signals, deduplication, version binding, cancellation authorization, session suspend/resume, session lifecycle, `StreamSession` including RFC-MACP-0006-A1 passive subscribe (`test_passive_subscribe.rs` -- history replay with `after_sequence` offsets, unknown-session and non-participant rejection, late-joiner replay-then-live delivery), mode registry operations including `PromoteMode` and the `WatchModeRegistry`/`WatchPolicies`/`WatchRoots` observation streams, JWT bearer auth, TLS transport (self-signed round-trip plus a no-plaintext-fallback check), persistence and restart-replay against a real data directory, payload-size and per-sender rate limits, true concurrent senders racing into one session, and discovery RPCs. Most tests share one dev-mode server; tests that need special server configuration (TLS, limits, persistence, promotion) spawn their own on dynamic ports. They run in a few seconds with no external dependencies.

**Tier 2: Rig agent tools** (5 tests) validate the MACP operations implemented as Rig `Tool` trait objects. These are called through `ToolSet::call()`, the same interface an LLM agent would use. They cover all five standard modes and verify that the tool abstraction correctly maps to gRPC operations.

**Tier 3: End-to-end with real LLM** (3 tests, marked `#[ignore]`) use real OpenAI GPT-4o-mini agents coordinating through the runtime. The architecture follows the protocol's design: orchestrator operations are deterministic plain code, while specialist reasoning uses real LLM inference. Agents run in parallel and the runtime serializes their contributions by acceptance order. These tests demonstrate both the coordination plane and the ambient plane (signals) working simultaneously.

### Running integration tests

```bash
# Build the runtime first
cargo build

# Tier 1 + 2 (no API keys needed)
cd integration_tests
MACP_TEST_BINARY=../target/debug/macp-runtime cargo test -- --test-threads=1

# Individual tiers
MACP_TEST_BINARY=../target/debug/macp-runtime cargo test --test tier1 -- --test-threads=1
MACP_TEST_BINARY=../target/debug/macp-runtime cargo test --test tier2 -- --test-threads=1

# Tier 3 (requires OpenAI API key)
OPENAI_API_KEY=sk-... MACP_TEST_BINARY=../target/debug/macp-runtime \
  cargo test --test tier3 -- --ignored --test-threads=1

# Against a hosted runtime (no local server started)
MACP_TEST_ENDPOINT=host:50051 cargo test -- --test-threads=1
```

Or use the Makefile targets from the project root:

```bash
make test-integration-grpc      # Tier 1
make test-integration-agents    # Tier 2
make test-integration-e2e       # Tier 3
make test-integration-hosted    # All tiers against MACP_TEST_ENDPOINT
```

### Configuration

| Variable | Purpose | Default |
|----------|---------|---------|
| `MACP_TEST_BINARY` | Path to the runtime binary | Builds from parent crate |
| `MACP_TEST_ENDPOINT` | Hosted runtime to test against (skips local server) | Starts local server |
| `MACP_TEST_TLS` | Use TLS for hosted connection | `0` |
| `MACP_TEST_AUTH_TOKEN` | Bearer token for hosted runtime | Dev headers |
| `MACP_TEST_BACKEND` | Run the storage-backend smoke test on `rocksdb`/`redis`/`file` (binary must be built with the matching feature) | Test skips if unset |
| `MACP_TEST_REDIS_URL` | Redis endpoint for the redis backend smoke test | `redis://127.0.0.1:6379` |
| `OPENAI_API_KEY` | Required for Tier 3 tests | Tier 3 tests skip if unset |

## Policy tests

The policy engine has dedicated coverage across multiple test layers:

**Unit tests** in `crates/macp-policy/` include approximately 80 tests covering all six voting algorithms, quorum threshold calculations, veto logic, evaluation confidence requirements, registry CRUD operations, schema validation, default policy behavior, and rule deserialization.

**Mode unit tests** in `crates/macp-modes/src/mode/*.rs` exercise policy denial paths in all five standard modes, verifying that governance policies correctly block commitment when rules are not satisfied.

**Conformance fixtures** exercise mode lifecycles with policy version binding to ensure policies are resolved and applied correctly during replay.

**Integration tests** perform gRPC round-trips for all five policy RPCs (`RegisterPolicy`, `GetPolicy`, `ListPolicies`, `UnregisterPolicy`, `WatchPolicies`) and test end-to-end policy enforcement: registering a policy, starting a session bound to it, and verifying that commitment is blocked when rules are not met.

## CI/CD

Every pull request runs the full gate in `.github/workflows/ci.yml`: MSRV check (1.89.0), fmt, clippy, rustdoc (`-D warnings`), unit + conformance + policy tests, release build, crate dependency-isolation checks, a blocking `cargo audit` (ignore list in `.cargo/audit.toml`), feature-gated builds and tests for the rocksdb/redis/otel features (including backend smoke tests through the real gRPC binary against rocksdb and a live redis service), the Tier 1 + Tier 2 integration suites, the conformance oracle against the spec repo's canonical fixtures, and a build-only Docker gate. Coverage uploads to Codecov as advisory. All jobs run on the toolchain pinned in `rust-toolchain.toml`; a weekly scheduled audit (`scheduled-audit.yml`) catches new RUSTSEC advisories between PRs.

Tier 3 runs only via manual GitHub Actions dispatch:

```
Actions -> "Integration Tests" -> Run workflow -> check "Run Tier 3 E2E"
```

Tier 3 tests require the `OPENAI_API_KEY` repository secret. Tiers 1 and 2 run without any secrets.
