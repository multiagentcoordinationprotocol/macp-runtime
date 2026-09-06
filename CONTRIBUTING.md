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

## Lockfiles

There are **two** checked-in lockfiles: the root `Cargo.lock`, and
`integration_tests/Cargo.lock` — `integration_tests/` is a separate cargo
workspace (the root manifest excludes it). CI enforces both with
`cargo metadata --locked`: the root one in the **Check (MSRV)** job, the
integration one in **Integration (tier 1 + 2, real gRPC boundary)**.

The second guard reaches wider than its name suggests.
`integration_tests/Cargo.lock` records the full dependency edges of the seven
`macp-*` path crates, so **adding or removing a dependency, or changing a
requirement the existing pin no longer satisfies, in _any_ workspace crate**
— `crates/macp-*/Cargo.toml` or the root `Cargo.toml`, not just
`integration_tests/Cargo.toml` — leaves that lock stale and reds the
integration job. No other CI step passes `--locked` against that workspace, and
cargo silently repairs a stale lock at build time, so this guard is the only
place the staleness surfaces.

Regenerate the affected lock in the same PR as the manifest change:

```bash
cargo metadata --format-version 1 > /dev/null                                                # root Cargo.lock
cargo metadata --manifest-path integration_tests/Cargo.toml --format-version 1 > /dev/null   # integration_tests/Cargo.lock
```

Then commit whichever lockfiles changed. Use `cargo metadata`, not
`cargo update` — `cargo update` would also refresh unrelated registry
dependencies to their newest permitted versions.

Version bumps are handled for you: release-plz regenerates the root lock, and
the `sync-integration-lock` job in `.github/workflows/release-plz.yml`
regenerates the integration one on the release PR (see below). There is no
longer a manual staleness check before pushing.

Dependabot is the one submitter that can't fix this itself: its
`/integration_tests` entry opens a separate PR and has no visibility into a
root-workspace bump. If a dependabot PR against the root workspace changes any
manifest, regenerate `integration_tests/Cargo.lock` and push it onto the
dependabot branch.

## Planning docs

`plans/` holds one phased plan per file (e.g.
`release-plz-integration-tests-lockfile-sync.md`), each with a matching
`-PROGRESS.md` once work starts; `plans/cross-repo/` holds plans spanning the
sibling repos, and `plans/defer/` holds deferred/follow-on work.
`plans/BUILD_STATUS.md` is the historical v0.5.0 build log — the
`plans/IMPROVEMENT_PLAN.md` and `plans/current/` it references were deleted
after that release shipped and live only in git history.

## Pull requests

CI must be green: fmt, clippy (-D warnings), workspace tests, MSRV check,
release build, dependency-isolation, feature-gated backend tests, the
tier-1 integration suite, and a blocking `cargo audit` (ignore list in
`.cargo/audit.toml`).

### Approving a release PR

release-plz opens the version-bump PR, and the separate `sync-integration-lock`
job in `.github/workflows/release-plz.yml` then regenerates
`integration_tests/Cargo.lock` on that same PR — release-plz only regenerates
the root lock, so the second one would otherwise be stale by construction after
every bump.

When that job commits, it **moves the PR's head SHA**, which produces a *second*
`action_required` workflow run on the PR. Branch protection on `main` requires
12 status contexts with `strict: true`, evaluated at the head SHA — so approving
only the earlier run leaves the new head with zero checks and the PR
unmergeable. Approve the run at the SHA the sync job reports: it emits a
`::notice` titled "Release PR head moved" and writes the new head SHA into its
step summary. Find that run with `gh run list --commit <sha>`, then approve it
from the Actions UI ("Approve and run" on the run's page) — the workflow run
list at that SHA gets you there fastest. If the lock was already in sync the
job commits nothing, the head does not move, and there is only the one run to
approve.
