# Contributing to macp-runtime

Thanks for contributing! This runtime is the reference implementation of the
Multi-Agent Coordination Protocol (MACP); the RFCs in the spec repository are
normative and this codebase follows them even where older docs disagree.

## Getting started

Prerequisites: stable Rust (MSRV 1.89), `protoc`.

```bash
cargo build
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets   # CI enforces -D warnings
```

Integration tests (real gRPC boundary):

```bash
cargo build
cd integration_tests
MACP_TEST_BINARY=../target/debug/macp-runtime cargo test --test tier1 --test tier1_jwt -- --test-threads=1
```

Feature-gated backends (CI runs these too):

```bash
cargo test -p macp-storage --features rocksdb-backend
MACP_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test -p macp-storage --features redis-backend
```

## Ground rules

- **Read `CLAUDE.md`** for the architecture, layering invariants (enforced by
  the `deps-isolation` CI job), and the freeze-profile invariants.
- **Every behavior change lands with a test.** Changes affecting message
  acceptance or replay need a regression test, and — if they change semantics
  of persisted histories — a legacy-log fixture proving old logs still replay
  under their original semantics (see `Session::semantics_rev`).
- **Never weaken these invariants**: rejected messages don't consume dedup
  slots or mutate history; authenticated identity derives `sender`; log
  append is the commit point (acked implies durable on file/RocksDB);
  signals never touch session state.
- **Construct `Session` via `Session::builder`** — core public types are
  `#[non_exhaustive]`.
- Policy decisions are fail-closed: only an explicit `Allow` proceeds.
- Update `CHANGELOG.md` for anything user-visible, and keep `README.md`,
  `docs/`, and the example clients in `src/bin` in step when you change
  SDK-facing behavior.

## Planning docs

`plans/IMPROVEMENT_PLAN.md` is the audited backlog (with evidence);
`plans/current/` holds the phased execution plans; `plans/BUILD_STATUS.md`
tracks live task status. If you pick up a planned task, mark it in
BUILD_STATUS.md.

## Pull requests

CI must be green: fmt, clippy (-D warnings), workspace tests, MSRV check,
release build, dependency-isolation, feature-gated backend tests, and the
tier-1 integration suite. `cargo audit` runs as advisory.
