# PROGRESS — capabilities-instructions-and-clippy-scope-198-201

PR strategy: single PR, two phases. Both are small, unrelated, `Risk: simple` fixes
bundled into one `/drive` run at the user's request ("#198 and 201 as well", then
"wrap all open issues and /ship") — same "one story, one PR" rationale as the
`#169`/`#170` precedent. Verification: batched (both simple, ≤2 per gate per
`/implement` §2).

## Repo map

- `src/server.rs:793-858` — `initialize` RPC handler; `:857` is Phase 1's target string. `:1109-1134` — `get_manifest`, confirmed no separate `instructions` field. `:2335, :2349` — existing `initialize`-related tests, next to where Phase 1's new regression test goes.
- `crates/macp-auth/src/security.rs:103-148` — `dev_mode()`/`dev_authenticate`, the real dev-auth behavior. `:394-406` — `bearer_token` (accepts an unrelated `x-macp-token` alt header). `:534-535` — `#[cfg(test)] mod tests` opens; all other `x-macp-agent-id` hits (`:599,:970,:993,:1020,:1050`) are inside it.
- `.github/workflows/ci.yml:133-159` — the `clippy` job, `:159` is Phase 2's CI target. `:456,460,465` — the separate `features` job's narrower `-p`-scoped clippy calls (existing, unrelated, already covers `macp-storage`+flags and the root crate again).
- `Makefile:18` — same narrow clippy command as CI, feeds `make check` (`:37`)/`make test-all` (`:29`). Phase 2's second target.
- `crates/macp-modes/src/mode/multi_round.rs:976-1022` — `canonical_proto_exhaustively_round_trips_at_rev3`, `:1014` is the sole consumer of the array Phase 2 retypes.
- Root `Cargo.toml:1,15-16` — `[package]` + `[workspace]` with no `default-members`, the mechanism behind #201.

## Checkpoints

- **2026-09-28** — Plan drafted (`plans/capabilities-instructions-and-clippy-scope-198-201.md`). Both issues' code read directly (`src/server.rs`, `crates/macp-auth/src/security.rs`, `.github/workflows/ci.yml`, `crates/macp-modes/src/mode/multi_round.rs`, root `Cargo.toml`).
- **2026-09-28** — Plan review round 1 (fresh Opus subagent, including an empirical clean-cache `cargo clippy --workspace --all-targets -- -D warnings` run): verdict **REVISE**, 3 gaps. Applied: (1) added `Makefile:18` to Phase 2 (same narrow command, feeds local `make check`/`make test-all`); (2) corrected Context's overstated "zero coverage" claim — a separate `features` job already covers `macp-storage`+flags and the root crate; independently confirmed `multi_round.rs:1014` is the *only* finding at full `--workspace` scope, so Phase 2's fix is complete; (3) added a clean-build-cache requirement to Phase 2's acceptance criterion 3 (clippy's `-D warnings` args aren't part of Cargo's fingerprint — a stale cached unit can false-green). Also: fixed a line-citation drift, softened "no header involved at all" (an unrelated `x-macp-token` alt header exists), added a Phase 1 regression test. Round 2 not run — findings were small corrections, not structural disagreement.
- **2026-09-28 — Phase 1 executed.** `src/server.rs:857`'s `instructions` string corrected; new regression test `initialize_instructions_do_not_advertise_removed_header` added beside the two existing `initialize` tests.
  Measured:
  1. String no longer contains `x-macp-agent-id`. ✓
  2. Replacement accurately describes `dev_authenticate` (bearer-value-as-sender under `MACP_ALLOW_INSECURE`). ✓
  3. Repo-wide grep (all file types, excluding `target/`/`.git/`): the only non-test-module hits left are the new test's own literal string (asserting the value is *absent* from output) and this plan/PROGRESS prose — zero remaining in served/production code. ✓
  4. `cargo test --lib -- initialize`: 3/3 (both pre-existing tests unmodified, new one passing). ✓
  5. New regression test passes and fails correctly if the string reverts (checked positively for `MACP_ALLOW_INSECURE`, negatively for `x-macp-agent-id`). ✓
  Also: `cargo test -p macp-auth --lib`: 60/60.

- **2026-09-28 — Phase 2 executed.** `multi_round.rs:1014` retyped via a new `ContributeValueShape` alias; `.github/workflows/ci.yml:159` and `Makefile:18` both gained `--workspace`.
  Measured:
  1. `cargo test -p macp-modes --lib -- canonical_proto_exhaustively_round_trips_at_rev3`: 1/1. ✓
  2. Both `ci.yml:159` and `Makefile:18` read `cargo clippy --workspace --all-targets -- -D warnings`. ✓
  3. **Clean-cache re-run** (`cargo clean -p` all 7 crates, ~176.5MiB removed, then the exact new command): clean — all 7 crates checked, zero findings. This is the criterion the plan review added specifically to rule out a stale-fingerprint false-green; discharged for real, not skipped. ✓
  4. `actionlint .github/workflows/ci.yml`: clean. ✓
  5. `cargo test --workspace`: 0 failures across all binaries (root lib: 152 passed, up from 151 — the one new Phase 1 test). `cargo fmt --check`: clean. ✓

  **Verifier (fresh Opus subagent, batched gate, independently re-ran every measured command including a from-scratch clean-cache clippy run — 656.7 MiB removed, exit 0, all 7 crates actually recompiled, not a stale-fingerprint replay): Phase 1 PASS, Phase 2 PASS, round 1, no gaps.** Two cosmetic notes, neither a gap: the type alias sits inside the test fn immediately above `shapes` rather than above the whole test (tighter scoping, not worse); PROGRESS's "fails correctly if reverted" claim was verified by inspection, structurally guaranteed by the bidirectional assertions.
- Next: commit both phases, then `/ship`.
- **Committed:** `a243014` on `feat/capabilities-instructions-and-clippy-scope-198-201`.

## Finalization (`/implement` §4)

Two-phase plan, batched into one verification gate that already covered the full
cumulative diff (not a per-phase slice), so there's no separate "seam between phases" to
integration-test — the phases touch four completely disjoint files (`src/server.rs`;
`crates/macp-modes/src/mode/multi_round.rs` + `.github/workflows/ci.yml` + `Makefile`)
with zero interaction. Full `cargo test --workspace` (already run, 0 failures) and the
clean-cache `--workspace` clippy run (already run by both the executor and the verifier,
independently, both clean) together are the whole-feature proof: Phase 2's fix is
specifically that clippy now sees everything Phase 1's diff touches too, so this run is
also the first real confirmation that Phase 1's own code is clippy-clean under the
widened scope. No `ASSUMPTIONS.md` entries created by this plan. Proceeding straight to
`/ship`.
