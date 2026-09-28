# PLAN — re-vendor the parity contract (#197) and catch the `SPEC_REV` pin up to spec `main` (#199)

**Written:** 2026-09-28 · **Base:** `15c3ff1` on `main` (workspace `0.8.3`) · **Spec pin today:** `4f15b96cac6e39d62925a5baa1ef80a42c2f818d` · **Spec `main`:** `2f7557c72082d20c0809cb025c2ba72270a7b068` (confirmed live via `git fetch origin main` in the sibling checkout — matches `origin/main` exactly, no further drift since #199 was filed) · **Planner:** Opus, with the full spec diff and the runtime's `parse_contribute_value` fix read directly.

**Models for execution:** Opus, single phase, not critical. No production code changes — this is test-fixture vendoring plus one CI config line, and the analysis below is the proof that nothing else is needed. No Fable: no one-way door, no trust boundary, no cross-repo write (the spec repo is read-only here, same as the `#169`/`#170` precedent).

---

## Context

Two issues (#197, #199), one spec commit range, and — same shape as the `#169`/`#170` precedent (`plans/spec-drift-catchup-169-170.md`) — **the runtime's code is already correct**. The fix issue #192 shipped (`crates/macp-modes/src/mode/multi_round.rs`'s `parse_contribute_value`, the `trust_proto = semantics_rev >= 3 && …` tie-break) already produces exactly the decode behavior every new vector in the spec's updated manifest pins. This plan is a vendoring chore plus a CI pin bump, not a bugfix.

**What moved upstream, measured directly** (`git diff --stat 4f15b96..2f7557c` in the sibling spec repo, restricted to the three trees `conformance-oracle` and this repo's tests actually consume):

| Tree | Changed? | Detail |
|---|---|---|
| `schemas/conformance/**` (incl. `cmt-hash/`) | **No** | Zero files differ. #199 step 1 ("vendor changed/added fixtures") is a no-op for this range. |
| `schemas/json/policy/**` | **No** | Zero files differ. #199 step 2 ("re-check the hand-written mirrors in `registry.rs`") is a no-op for this range. |
| `schemas/parity/contract.json` | **Yes** | `contract_version` `1.0.0` → `1.1.1`; `contribute_payload.vectors` grows from 4 to 8 (four new collision vectors, byte-lengths 10/13/32/123); four `source` fields gain citations (`protocol`: RFC-MACP-0001 §6; `projection_anomaly`: mirror-direction clarification; `contribute_payload`: the tie-break mechanism; `contribute_acceptance`: its rationale) — **prose only, no other value changes**. `schemas/parity/README.md` also changed (+172/−27 lines) but is outside this plan's scope: `tests/parity/SOURCE.md:17-19` and `check_dir`'s `*.json` glob (`ci.yml:611,623`) both exclude it — only `contract.json` is byte-compared or consumed. |

Six more files outside those three trees also changed in the range: `schemas/json/macp-agent-bootstrap.schema.json` plus five new fixtures under `schemas/json/tests/invalid-agent-bootstrap/` (from spec commit `2f7557c`) — grepped for across this repo (`agent.bootstrap|agent_bootstrap`, all extensions): **zero matches**, not vendored or consumed anywhere in `macp-runtime`. Also three copies of `multi_round.proto` (`schemas/proto/`, `packages/proto-rust/`, `packages/proto-npm/`) gain an 8-line comment (no wire-format change — confirmed by plan review); this repo consumes that message type via the published `macp-proto` crate (`Cargo.toml`'s `[workspace.dependencies]`, pinned by version from crates.io, not a vendored copy of this file), so a comment-only upstream edit has no local artifact to update. No action on any of these nine files.

`rfcs/RFC-MACP-0001-core.md` also changed (§6 gains a normative statement that the MACP Core protocol version is `1.0`), but its own changelog says it plainly: *"No behavior changes: this pins, for the first time, the value every conformant implementation already sends."* `macp_core::MACP_VERSION` is already `"1.0"` and already asserted by `tests/parity_contract.rs::protocol_macp_version_matches_runtime` — this is exactly the citation upgrade reflected in `contract.json`'s `protocol.source` field, not a behavior change.

**Why #197 and #199 land as one phase, not two.** `tests/parity/SOURCE.md` states the coupling explicitly: *"this must stay equal to `SPEC_REV`"*. `conformance-oracle`'s `check_dir` step byte-compares `tests/parity/*.json` against `spec-repo/schemas/parity/*.json` **checked out at `SPEC_REV`**. Re-vendoring `contract.json` without bumping `SPEC_REV` makes `check_dir` report `DRIFT` (the vendored copy no longer matches the still-old pin). Bumping `SPEC_REV` without re-vendoring does the same in the other direction. There is no intermediate state where only one half lands and CI stays green — so, unlike the `#169`/`#170` precedent's three independent phases, this genuinely is one atomic unit of work.

**Why the pin target is `2f7557c`, not `99756f8`** (the commit #197's own issue comment names). `99756f8` is where `contract.json` itself last changed; `2f7557c` is one commit later and the actual current spec `main`. Between those two commits nothing under any of the three consumed trees changes (confirmed above), so pinning at `2f7557c` costs nothing beyond `99756f8` and actually closes #199, which tracks the pin against spec `main` and self-refreshes daily — pinning short of `main` would just regenerate a new drift issue the next time the watcher runs.

**Scope.** One phase, entirely in `macp-runtime`. No cross-repo writes (spec repo is read-only, already at the commit this plan pins). Closes **#197** and **#199**.

---

## Phase 1 — re-vendor `contract.json`, bump `SPEC_REV`, update the vector-count assertion

- **Status:** DONE (2026-09-28). Implemented exactly as planned, no divergence. All 10
  acceptance criteria measured (see `PROGRESS.md`), including the prove-then-restore
  negative checks (criterion 8) and the full untruncated `cargo test --workspace` run
  (criterion 9: 36 test binaries, 0 failures, 951 tests total across unit/integration/doc
  tests). One out-of-scope finding surfaced during verification, not fixed here (see
  `PROGRESS.md`): CI's actual `Run clippy` step (`ci.yml:158`, `cargo clippy --all-targets
  -- -D warnings`, no `--workspace`) only lints the root `macp-runtime` package by Cargo's
  default-members rule, so it never reaches `crates/*` — a pre-existing
  `clippy::type_complexity` failure already on `main` at
  `crates/macp-modes/src/mode/multi_round.rs:1014` (from already-merged PR #196, well
  before this branch) is invisible to CI and untouched by this diff.
- **Risk:** simple — test-data vendoring + one CI config line, no production code, no new boundary crossed, fully reversible in a commit.
- **Delivers:** `tests/parity/contract.json` matches spec `main` byte-for-byte; `.github/workflows/ci.yml`'s `SPEC_REV` pin catches up to it; `tests/parity_contract.rs` asserts the new vector count. `conformance-oracle` stays green against the new pin, proven locally before push. Closes **#197** and **#199**.
- **Depends on:** nothing.
- **Files:**
  - `tests/parity/contract.json` — replaced byte-for-byte from the spec repo (see Approach).
  - `tests/parity/SOURCE.md` — pinned commit/date updated to `2f7557c72082d20c0809cb025c2ba72270a7b068` / 2026-09-28.
  - `.github/workflows/ci.yml:39` — `SPEC_REV` updated to the same commit.
  - `tests/parity_contract.rs:458-463` — `vectors.len(), 4` (and its panic message) → `8`.
- **Approach:** Copy, don't hand-edit — `tests/parity/SOURCE.md`'s own documented procedure: `cp ../multiagentcoordinationprotocol/schemas/parity/contract.json tests/parity/contract.json`. The sibling checkout's `HEAD` already equals the target pin (`2f7557c72082d20c0809cb025c2ba72270a7b068`, confirmed via `git fetch` above) and its working tree is clean, so a direct `cp` is faithful — but verification (below) still builds a separate `git archive` export rather than pointing tests at the sibling tree directly, per `CLAUDE.md`'s "Policy evaluation" section: *"never at the sibling working tree — a dirty or ahead checkout produces failures that do not exist in CI"*. That guidance is about the verification step's reproducibility, not the vendoring copy itself (which `SOURCE.md` already specifies as a direct `cp`).

  **Rejected: splitting into a "vendor" phase and a "bump pin" phase.** See Context — the two are byte-coupled through `check_dir`; no independently-green intermediate state exists. **Rejected: pinning at `99756f8` instead of `2f7557c`.** Costs nothing to go further (no additional tree changes between them) and pinning short of `main` leaves #199 (which diffs against spec `main`, not a fixed commit) to refile itself on the next scheduled run.
- **Edge cases & failure modes:** The CI job's "at least 17 `parity_contract` tests passed" guard (`ci.yml:667-674`) is unaffected — the new vectors are data inside two existing `#[test]` functions (`contribute_payload_vectors_round_trip_through_the_real_codec` and `contribute_payload_first_byte_markers_match_vectors`, the latter also iterating all 8 vectors and worth naming explicitly since `collision_no_leading_brace_123`'s `protobuf_hex` starts `0a7b` — its length-varint byte is literally the `legacy_json` first-byte marker `0x7b`, the exact collision the vector exists to pin), not new test functions, so the count stays exactly 17. **None** of the four new vectors carry a `decode_only` field (defaults `false` per `#[serde(default)] decode_only: bool`) — only the pre-existing `one_byte_varint_boundary` has it — so all four new vectors go through the full decode-both-paths-and-re-encode assertion, not just a decode check. Independently hand-verified byte-level during plan review: `collision_leading_brace_13` (`0a0d` = tag byte + LF/CR, both insignificant JSON whitespace) and `collision_no_leading_brace_123` (`0a7b`, the length-varint byte *is* `{`) both parse as JSON and are correctly rescued by `trust_proto` at `semantics_rev >= 3`; `collision_foreign_key_10` never reaches the tie-break at all, because `ContributeJson` (`multi_round.rs:43-45`) requires a `value` key and `{"a":"xx"}` has none, so it falls through to the proto decode unconditionally regardless of `semantics_rev`. The `contribute_acceptance` section's value (`empty_payload: "reject"`) is unchanged by the spec bump (only its `source` prose changed), so `contribute_acceptance_empty_payload_is_rejected` needs no attention. **Local verification requires `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk`** (confirmed during planning: a bare `cargo build` fails in this checkout against the default SDK with a `tapi error: malformed file … arm64e.x1-macos`-style link failure, same environment quirk `plans/spec-drift-catchup-169-170.md`'s Phase 2 already documented; the workaround unblocks `cargo build -p macp-modes` cleanly). If a future re-run of this plan finds spec `main` has moved again past `2f7557c`, re-diff the same three trees before assuming this plan's "no production code change" conclusion still holds — don't just extend the pin.
- **Acceptance criteria:**
  1. `tests/parity/contract.json` is byte-identical to a fresh `git archive` export of `schemas/parity/contract.json` at `2f7557c72082d20c0809cb025c2ba72270a7b068` (`diff` exits 0).
  2. `tests/parity/SOURCE.md`'s pinned commit and date read `2f7557c72082d20c0809cb025c2ba72270a7b068` / 2026-09-28.
  3. `.github/workflows/ci.yml` contains `2f7557c72082d20c0809cb025c2ba72270a7b068` exactly once, as `SPEC_REV`, still a quoted 40-character lowercase hex string — this exact shape (unabbreviated, lowercase) is what `spec-drift.yml`'s own pin-parsing `sed`/regex expects, per the `#169`/`#170` precedent's Phase 2 edge-case note; not merely a style preference.
  4. `tests/parity_contract.rs`'s `contribute_payload_vectors_round_trip_through_the_real_codec` asserts `vectors.len() == 8` (both the comparison and its panic message text).
  5. `cargo test --test parity_contract` passes (17 passed, 0 failed) against the vendored copy (default path, no env var).
  6. Reproduce CI's own steps against a clean `git archive` export `$D` of `2f7557c72082d20c0809cb025c2ba72270a7b068` (not the sibling working tree):
     - `check_dir`-equivalent byte identity, both directions, for `tests/parity` ↔ `$D/schemas/parity`: 0 MISSING / 0 DRIFT / 0 EXTRA.
     - `MACP_PARITY_CONTRACT=$D/schemas/parity/contract.json cargo test --test parity_contract` — 17 passed, 0 failed (this is the criterion that actually exercises the new vectors against the real `parse_contribute_value`/`prost` round-trip, not just the JSON's own internal consistency).
     - `MACP_CONFORMANCE_FIXTURES_DIR=$D/schemas/conformance cargo test --test conformance_loader` — same pass count as baseline (proves the "no conformance fixture drift" finding, not just assumes it).
     - `MACP_POLICY_SCHEMAS_DIR=$D/schemas/json/policy cargo test -p macp-policy --lib -- --exact registry::tests::enum_lists_match_the_canonical_schemas` — `1 passed` (proves the "no policy schema drift" finding).
  7. `git diff --name-only` for this phase's commit, excluding `plans/`, is exactly: `.github/workflows/ci.yml`, `tests/parity/contract.json`, `tests/parity/SOURCE.md`, `tests/parity_contract.rs`. Any other file touched means the "no production code change" analysis in Context was wrong and needs revisiting, not patching around.
  8. **Prove-then-restore, on a scratch copy — not the tracked files.** Issue #199's own checklist (step 3, "re-run its prove-then-restore checks") and `plans/parity-contract-176.md`'s established discipline both require demonstrating each gate actually fails when it should, not just that it passes once re-vendored:
     - Copy the newly-vendored `tests/parity/contract.json` to a scratch path, flip one byte in it, point `check_dir`'s logic (replayed per criterion 6) at the scratch copy instead of the real one → reports `DRIFT`, not silence.
     - In a second scratch copy, corrupt one **new** vector's `value` field (e.g. `collision_leading_brace_13`'s `value`) so it no longer matches what its `protobuf_hex` actually decodes to → `cargo test --test parity_contract` against that scratch copy fails on `contribute_payload_vectors_round_trip_through_the_real_codec` with that vector's own name in the panic message, not a generic or unrelated failure.
     Both scratch copies are discarded, never committed — this criterion is discharged by the run output in `PROGRESS.md`, not by a file in the diff.
  9. Full workspace suite green: `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk cargo test --workspace` (plus `cargo clippy -- -D warnings`, `cargo fmt --check`), unaffected by this change but run as the standard `/ship` gate.
  10. After merge: #197 and #199 both closed (via PR body `Closes #197`, `Closes #199`, so merge auto-closes them) with an evidence comment on each citing the merged commit and criterion 6's measured results — same pattern as the `#169`/`#170` precedent's Phase 2 criterion 6. Don't close silently; #199 self-refreshes daily and the close is the record of why the bump needed nothing else.
- **Tests:** criterion 6 *is* `conformance-oracle` reproduced locally, which is the load-bearing test here — there's no new runtime logic to unit-test. `tests/parity_contract.rs` itself is the test file being updated (criterion 4) and is exercised by criteria 5 and 6.
- **Docs:** none. No behavior, interface, or environment-variable change; `docs/testing.md`'s description of the oracle mechanism (already updated by the `#169`/`#170` plan) needs no further edit — the pin value it describes generically, not by literal SHA.

---

## Long-term posture

Not a one-way door. Vendored test data and a CI pin, both trivially revertible. No public API, schema, or wire-format change — `CLAUDE.md`'s `#[non_exhaustive]` mode-state sealing and `cargo semver-checks` are unaffected (nothing under `crates/` changes).

## Enterprise concerns

None beyond what `conformance-oracle` already guards: this phase's whole job is proving that guard stays green at the new pin *before* the pin moves in CI, not after. No new failure domain — the change makes an existing quarterly maintenance mechanism ordinary work.

## Open questions

None requiring escalation. The one judgment call made — pinning at spec `main` (`2f7557c`) rather than the older commit #197's comment named (`99756f8`) — is decided and recorded in Context above (reversible, no defensible reason to prefer the older pin once the diff between them is confirmed empty for every tree this repo consumes).

## Repo map

- `tests/parity/contract.json` — vendored, byte-identical copy of the spec repo's parity manifest. Re-vendored by direct `cp` per `tests/parity/SOURCE.md`.
- `tests/parity/SOURCE.md` — provenance doc: pinned commit/date, re-vendoring procedure, and the note that it must track `SPEC_REV`.
- `tests/parity_contract.rs` — 17 `#[test]` fns asserting the manifest's `macp-runtime`-relevant sections against real runtime code (not the manifest's own JSON shape — that's the spec repo's own `check-parity-contract.py`). `MACP_PARITY_CONTRACT` env var swaps the loaded file to the canonical spec-repo copy in CI.
- `.github/workflows/ci.yml` — `SPEC_REV` (line 39) pins the spec-repo commit; `conformance-oracle` job (line ~580) checks it out, runs `check_dir` byte-identity (both directions, `*.json` glob only) against `tests/conformance/`, `tests/conformance/cmt-hash/`, and `tests/parity/`, then re-runs the conformance/parity/policy-parity suites directly against the canonical checkout.
- `crates/macp-modes/src/mode/multi_round.rs` — `parse_contribute_value`, the already-shipped issue #192 fix (`trust_proto = semantics_rev >= 3 && …` tie-break). Not touched by this plan; read to confirm the new vectors already pass against it.
- `crates/macp-policy/src/registry.rs::validate_conditional_constraints` + `enum_lists_match_the_canonical_schemas` test — the hand-written policy-schema mirrors #199 step 2 asks to re-check. Confirmed unaffected (no `schemas/json/policy/**` diff in range); re-verified live in criterion 6 rather than only asserted.
- `tests/conformance/` — vendored conformance fixtures, confirmed unaffected (no `schemas/conformance/**` diff in range); re-verified live in criterion 6.
- `plans/cross-repo/multiagentcoordinationprotocol-parse-contribute-value-192.md` — this repo's own prior cross-repo ask that produced the four new vectors; its acceptance criterion 4 is what this plan's Phase 1 discharges (with the correction, already known, that the count is 8 not 7).
- `plans/spec-drift-catchup-169-170.md` — sibling precedent for this exact class of work (spec-pin bump + parity re-check), used as the template for phase structure and the "clean `git archive` export, never the sibling working tree" verification discipline.

## Plan review

**Round 1 (fresh Opus subagent, independently re-running the diffs and a byte-level decode trace against the code, not just reading the prose):** Verdict **REVISE**, four items, no blocking defect. Two were real factual errors in the drafted plan despite every underlying command having been run live during drafting — proof that "I ran the command" is not the same guarantee as "I transcribed its output correctly," which is the whole reason this round is mandatory rather than skippable: (1) Context's source-field count said "three" when four sections' `source` text actually changed (missed `projection_anomaly`); (2) the edge-cases note said "two of the four new vectors" lack `decode_only` when in fact all four do (only the pre-existing `one_byte_varint_boundary` has it). Both corrected above, along with the reviewer's other findings: `schemas/parity/README.md`'s change is now named and explicitly scoped out; a prove-then-restore acceptance criterion (flip-a-byte / corrupt-a-vector negative-path checks) was added, since #199's own checklist requires it and the original draft only proved the green direction; the `SPEC_REV` format note and the `contribute_payload_first_byte_markers_match_vectors` test were called out explicitly; and a real local-environment blocker (default SDK breaks `cargo build` in this checkout) was surfaced and its workaround recorded, confirmed working. **Round 2:** not run — every reviewer finding was a small, checkable factual correction (a count, a missing table row, a missing criterion), not a structural or approach-level disagreement; re-running a full review round over a four-item fix list would be re-verifying corrections the reviewer's own citations already pin down, and `/implement`'s per-phase Opus verification gate (Phase 1's own acceptance criteria) re-checks the plan against the actual diff regardless.
