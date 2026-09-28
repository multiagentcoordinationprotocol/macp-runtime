# PROGRESS — spec-drift-catchup-197-199

PR strategy: single PR, single phase. The vendored `contract.json` and the
`SPEC_REV` pin are byte-coupled through `conformance-oracle`'s `check_dir`
step (see plan Context) — there is no independently-green intermediate state
to split across two PRs or two phases.

## Repo map

(Also captured in the plan's own "Repo map" section — duplicated here per
`/plan`'s convention so `/implement` doesn't have to cross-reference.)

- `tests/parity/contract.json` — vendored, byte-identical copy of the spec repo's parity manifest. Re-vendored by direct `cp` per `tests/parity/SOURCE.md`.
- `tests/parity/SOURCE.md` — provenance doc: pinned commit/date, re-vendoring procedure, and the note that it must track `SPEC_REV`.
- `tests/parity_contract.rs` — 17 `#[test]` fns asserting the manifest's `macp-runtime`-relevant sections against real runtime code. `MACP_PARITY_CONTRACT` env var swaps the loaded file to the canonical spec-repo copy in CI.
- `.github/workflows/ci.yml` — `SPEC_REV` (line 39) pins the spec-repo commit; `conformance-oracle` job (~line 580) checks it out, runs `check_dir` byte-identity (both directions, `*.json` glob only) against `tests/conformance/`, `tests/conformance/cmt-hash/`, and `tests/parity/`, then re-runs the conformance/parity/policy-parity suites directly against the canonical checkout.
- `crates/macp-modes/src/mode/multi_round.rs` — `parse_contribute_value`, the already-shipped issue #192 fix. Not touched by this plan.
- `crates/macp-policy/src/registry.rs` — policy-schema mirrors; confirmed unaffected (no `schemas/json/policy/**` diff in range).
- `tests/conformance/` — vendored conformance fixtures; confirmed unaffected (no `schemas/conformance/**` diff in range).
- Sibling spec repo: `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol`, currently at `origin/main` = `2f7557c72082d20c0809cb025c2ba72270a7b068`, working tree clean.
- **Local environment blocker:** a bare `cargo build` fails in this checkout (default SDK). Use `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk` for every local `cargo` invocation in this plan. Confirmed working (`cargo build -p macp-modes` succeeds with it, 2026-09-28).

## Checkpoints

- **2026-09-28** — Plan drafted (`plans/spec-drift-catchup-197-199.md`). Live spec-repo diff measured (`4f15b96..2f7557c`, three consumed trees), `parse_contribute_value` read, current vendored state and CI job read. Sibling repo confirmed at true current `origin/main` via `git fetch`.
- **2026-09-28** — Plan review round 1 (fresh Opus subagent, independent re-diff + byte-level decode trace): verdict **REVISE**, 4 items. Applied to the plan: (1) source-field count corrected 3→4 (missed `projection_anomaly`); (2) `decode_only` claim corrected — all four new vectors lack it, not two; (3) `schemas/parity/README.md`'s change now named and scoped out explicitly; (4) added acceptance criterion 8, prove-then-restore (flip-a-byte / corrupt-a-vector negative checks), per #199's own checklist step 3. Also added: `SPEC_REV` format note, explicit mention of `contribute_payload_first_byte_markers_match_vectors`, and the local SDK-workaround note (confirmed working). Round 2 not run — remaining findings were narrow factual corrections, not structural disagreement; `/implement`'s per-phase verify gate re-checks against the real diff regardless.
- **2026-09-28 — Phase 1 executed.** All file changes made exactly per plan:
  `tests/parity/contract.json` (re-vendored, byte-identical to spec commit `2f7557c`),
  `tests/parity/SOURCE.md` (commit/date updated), `.github/workflows/ci.yml:39`
  (`SPEC_REV` updated), `tests/parity_contract.rs:459-461` (`4` → `8`, comparison and
  panic message both).

  Acceptance criteria measured:
  1. `diff` of vendored copy vs. `git archive` export at `2f7557c` — identical, exit 0.
  2. `tests/parity/SOURCE.md` reads the new commit + 2026-09-28.
  3. `SPEC_REV` appears once in `ci.yml`, 40-char lowercase hex, quoted.
  4. `vectors.len() == 8`, message text updated.
  5. `cargo test --test parity_contract` (vendored copy, default path): **17 passed, 0 failed**.
  6. Reproduced CI's `conformance-oracle` steps against a clean `git archive` export of
     `2f7557c` (not the sibling working tree, per `CLAUDE.md`'s caution):
     - `check_dir` logic replayed for `tests/parity` ↔ export: 0 MISSING/DRIFT/EXTRA.
     - `MACP_PARITY_CONTRACT=<export>/schemas/parity/contract.json cargo test --test parity_contract`: **17/17**.
     - `MACP_CONFORMANCE_FIXTURES_DIR=<export>/schemas/conformance cargo test --test conformance_loader`: **35/35** (baseline-matching — confirms no fixture drift).
     - `MACP_POLICY_SCHEMAS_DIR=<export>/schemas/json/policy cargo test -p macp-policy --lib -- --exact registry::tests::enum_lists_match_the_canonical_schemas`: **1 passed** (confirms no policy-schema drift).
  7. `git status --short` shows exactly the 4 planned files modified (+ the new plan/PROGRESS docs).
  8. **Prove-then-restore**, both directions, on scratch copies (discarded, never committed):
     - Flipped `contract_version` in a scratch vendored copy → `diff` against the export reports the change (`check_dir` would report `DRIFT`).
     - Corrupted `collision_leading_brace_13`'s `value` in a scratch manifest → `cargo test --test parity_contract contribute_payload_vectors_round_trip_through_the_real_codec` **FAILED**, panic message: `vector collision_leading_brace_13: protobuf decode mismatch` — names the corrupted vector, not a generic failure.
  9. Full workspace suite, untruncated: `cargo test --workspace` — **36 test binaries, 0 failures** (unit + integration + doctests across all 7 crates + root). `cargo fmt --check`: clean.
  10. Not yet done — post-merge issue closure (deferred to `/ship`'s PR flow).

  **Environment note (not in original plan, discovered during execution):** a bare `cargo
  build`/`cargo test`/`cargo clippy` fails in this checkout against the default SDK.
  `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk` is
  required for every local Rust invocation — same workaround
  `plans/spec-drift-catchup-169-170.md` documented, confirmed still valid.

  **Finding, out of scope, not fixed:** an initial `cargo clippy --workspace --all-targets
  -- -D warnings` run surfaced a `clippy::type_complexity` error at
  `crates/macp-modes/src/mode/multi_round.rs:1014` (`shapes: [(&str, fn(usize) -> String);
  5]` in `#[cfg(test)]` code). Traced via `git blame` to commit `ab64d1b` (PR #196,
  2026-09-25) — pre-existing on `main`, untouched by this phase's diff. Confirmed CI's
  actual `Run clippy` step (`ci.yml:158`) is `cargo clippy --all-targets -- -D warnings`
  **without** `--workspace`; since the root `Cargo.toml` has its own `[package]` and no
  `default-members` is set, Cargo's default-members rule scopes that command to the root
  `macp-runtime` package only, never reaching `crates/*`. Reproduced locally: the exact CI
  invocation is clean (`Finished`, no errors); the `--workspace`-scoped run is not. So (a)
  this repo's `Clippy` required check has a real coverage gap — `crates/*` is effectively
  unlinted by CI — and (b) this phase's own `cargo clippy` gate (matching CI's actual
  invocation) is genuinely green. Not fixed here: unrelated to #197/#199, would be scope
  creep. Worth a follow-up issue; not filed yet — flagging in the final report for the
  user to decide.

  **Verifier (fresh Opus subagent, independently re-ran every measured command including a fresh `git archive` export + sha256 comparison): PASS, round 1, no gaps.** Confirmed all acceptance criteria, confirmed the diff touches only the 4 planned files, confirmed the clippy-scoping finding by independently reading `ci.yml:159` and running both the CI-matching and `--workspace` invocations itself, confirmed the pre-existing `multi_round.rs:1014` line via its own `git blame`. Two cosmetic nits, not gaps: the plan's Context table undercounted the files outside the three consumed trees ("two" vs. the actual six-plus-three-proto-copies) — corrected in the plan; and `PROGRESS.md` didn't log the clippy result inline (fixed by this entry).
- Next: commit the phase, then `/ship`.
- **Committed:** `fdef908` on `feat/spec-drift-catchup-197-199`; follow-up `caa294e` (PROGRESS.md verdict note).
- **Ship-gate verification (fresh Opus subagent, independent re-run of every measured command over the full `main...HEAD` diff): PASS, round 1, no gaps.** Confirmed byte-identity, `check_dir`, all cargo checks, `ASSUMPTIONS.md` has zero entries tagged to this plan, doc drift is genuinely none (`CLAUDE.md` never mentions `SPEC_REV`; `docs/testing.md` describes the mechanism generically), tracked-file consistency holds. One note: issue #197's title says "1.1.0" but spec `main` had already moved to 1.1.1 the same day it was filed — substance (8 vectors) unchanged between those two spec commits (verified: empty diff across all three consumed trees), addressed in the PR body.
- **pushed feat/spec-drift-catchup-197-199 caa294e**
- **PR #200 opened: https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/200**

## Finalization (`/implement` §4)

Single-phase plan, so the "cumulative diff" and "last phase's diff" are identical, and
there are no inter-phase seams to integration-test. Phase 1's own verification gate
already reviewed the diff against the plan as a whole (not a narrow slice) and included
the full untruncated `cargo test --workspace` run, fmt, and CI's exact clippy invocation
— re-running a second identical Opus pass over the same diff would check nothing new.
Treating §4's finalization pass as discharged by Phase 1's gate rather than duplicating
it (Autonomy ladder: not critical, reversible, decidable by Opus). No `ASSUMPTIONS.md`
entries were created by this plan (the one judgment call — pinning at spec `main` rather
than the older commit #197's comment named — was decided with full evidence in the plan
text itself, not a guess needing later reconciliation), so `/reconcile` has nothing to do
here; proceeding straight to `/ship`.
