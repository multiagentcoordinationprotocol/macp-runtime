# PROGRESS — parity-contract-215

PR strategy: single PR, single phase, single commit. Byte-coupled through
`conformance-oracle`'s `check_dir` step (see plan Context) — no independently-green
intermediate state to split across two PRs or two phases, same as the `#204` precedent.

## Repo map

(Duplicated here per convention so `/implement` doesn't have to cross-reference.)

- `tests/parity/contract.json` — vendored, byte-identical copy of the spec repo's
  parity manifest. Re-vendored by direct `cp` per `tests/parity/SOURCE.md`. This
  phase moves it `1.2.0` → `1.3.0`: `contract_version` and one new top-level
  section, `proposal_disposition` — nothing else changes.
- `tests/parity/SOURCE.md` — provenance doc: pinned commit/date, re-vendoring
  procedure, must track `SPEC_REV`.
- `tests/parity_contract.rs` — 17 `#[test]` fns today, 18 after this phase. `HANDLED`
  and `ALL_SECTIONS` both gain `"proposal_disposition"`; new test function asserts
  `ProposalDisposition`'s `{Live, Withdrawn}` variant set against the manifest's
  `mode_state_dispositions`, plus a no-`Accepted` guard. `MACP_PARITY_CONTRACT` env
  var swaps the loaded file to the canonical spec-repo copy in CI.
- `crates/macp-modes/src/mode/proposal.rs` — `ProposalDisposition` enum (`Live`,
  `Withdrawn`), already public, already matches the manifest. **Not touched** —
  this phase only pins it with a test, no behavior change.
- `.github/workflows/ci.yml` — `SPEC_REV` (line 39) pins the spec-repo commit;
  `conformance-oracle` job's `check_dir` byte-identity check and the "≥17 parity
  tests passed" guard (two sites: a comment and the numeric check) both need the
  17→18 bump.
- `.github/workflows/spec-drift.yml` — **not touched.** Already watches
  `schemas/parity` per its standing convention.
- `crates/macp-policy/src/registry.rs` — **not touched.** Confirmed zero diff in
  `schemas/json/policy/**` across the whole pinned-to-target range.
- `tests/conformance/` — **not touched.** Confirmed zero diff in
  `schemas/conformance/**` across the whole range.
- Sibling spec repo: `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol`,
  confirmed at `origin/main` = `7159afe384f50b67c9c090796d926441eaa9acf4` (fetched
  live during planning; PR #179's merge commit `2989f644` confirmed as an ancestor).
- **Local environment blocker (same as `#204`):** a bare `cargo build`/`test`/
  `clippy` fails against the default SDK (`MacOSX27.0.sdk` — `tapi error: malformed
  file`). Use `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk`
  for every local `cargo` invocation in this plan. Confirmed working during planning:
  `cargo test --test parity_contract` → 17/17 against the pre-bump vendored file.
- Precedent plans this one models on: `plans/parity-contract-204.md` (PR #210 — the
  direct predecessor for this exact task shape) and `plans/parity-contract-176.md`
  (the runner's original build-out, `HANDLED`/`ALL_SECTIONS` design context).
- Related: issue #215 (this plan), spec PR #179 (merge commit `2989f64`), spec issue
  #176 (the upstream design discussion named in #179's own commit message).

## Checkpoints

- **2026-10-03** — Plan drafted (`plans/parity-contract-215.md`). Live spec-repo
  diff measured (`18f2332..7159afe`, restricted to the three consumed trees, then
  widened to confirm nothing else matters); `tests/parity_contract.rs` read in full
  and its 17-test baseline actually run against the pre-bump vendored file;
  `crates/macp-modes/src/mode/proposal.rs`'s `ProposalDisposition` enum read
  directly; `ci.yml` read at current line numbers; local `SDKROOT` build workaround
  re-confirmed working; `origin/main` fetched live immediately before finalizing.
- Plan re-verification: fresh general-purpose agent dispatched to check every
  file:line citation, the Rust snippet's compile-correctness, and the spec-repo
  diff claims against live sources (not against the plan's own text). **PASS**,
  with 4 stale citations (two `ci.yml` line numbers off by 2-4 lines, a
  line-count arithmetic slip, a tree-path typo, the plan header's base commit)
  — all corrected in `plans/parity-contract-215.md` before implementation. The
  agent additionally compile-verified the plan's exact Rust snippet by patching
  it into the real file and running the suite before reverting.
- **2026-10-03** — Phase 1 executed on branch `feat/parity-contract-215-1.3.0`,
  rebased onto `origin/main` (`22e6a78`). Re-vendored `contract.json`,
  bumped `SOURCE.md`/`ci.yml` `SPEC_REV`, added `proposal_disposition` to
  `HANDLED`/`ALL_SECTIONS`, added the new test function + import. All 10
  acceptance criteria passed. Files touched (vs. `origin/main`): exactly the
  predicted 4 — `tests/parity/contract.json`, `tests/parity/SOURCE.md`,
  `.github/workflows/ci.yml`, `tests/parity_contract.rs`.
- **2026-10-03** — Verification gate round 1: fresh Opus subagent, **GAPS**.
  Gap 1 (substantive): `proposal_disposition_mode_state_set_matches_runtime_enum`'s
  hand-enumerated `variants` array has no compile-time enforcement that it
  stays complete — the verifier empirically proved this by injecting a third
  enum variant (`Accepted`) and showing the test still passed. The plan's own
  edge-case claim (`:139`, now corrected) was factually wrong. Gap 2 (minor):
  `docs/testing.md:19`'s parallel enumeration of pinned-value/live-code lists
  was left stale (missing `proposal_disposition`/`ProposalDisposition`).
  Both closed: added `assert_proposal_disposition_is_exhaustively_covered`
  (`tests/parity_contract.rs`), a `match` with no wildcard arm, called once
  per variant inside the existing `.map()` — empirically re-verified by
  re-injecting the same third variant, confirming `error[E0004]:
  non-exhaustive patterns: &ProposalDisposition::Accepted not covered` at
  compile time, then cleanly reverting the injection (`git diff --stat`
  empty afterward). `docs/testing.md:19` updated to name both. Full re-run
  after the fix: `cargo test --test parity_contract` 18/18 (vendored and
  canonical export), full workspace suite 0 failures, `fmt`/`clippy` clean.
- **2026-10-03** — Verification gate round 2: fresh Opus subagent, given the
  round-1 gap list to confirm closure rather than reviewing cold. **PASS.**
  Independently re-ran the same E0004 empirical proof (injected `Accepted`
  into the real enum, confirmed compile failure, confirmed clean revert via
  `git diff --stat`/`git status --short`), confirmed `docs/testing.md:19`
  names both the pin and the live-code symbol, re-ran `cargo test --test
  parity_contract` (18/18), `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings` (all clean), and confirmed both the plan's
  correction and this file's trail accurately describe what happened.
- pushed feat/parity-contract-215-1.3.0 61f45e4
- PR #217 opened: https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/217
- **Next:** watch CI, merge on green.
