# PLAN — Re-vendor `tests/parity/contract.json` at 1.2.0, bump `SPEC_REV` (#204)

**Written:** 2026-09-30 · **Base:** `183d0e3` on `main` (workspace `0.8.5`) · **Spec pin today:** `2f7557c72082d20c0809cb025c2ba72270a7b068` (`ci.yml:39`) · **Spec `main`:** `18f233229e6cb1156351c68614610a9d3bb40497` (confirmed live via `git fetch origin main` in the sibling checkout at plan-writing time; working tree clean; this is also the exact revision `spec-drift.yml`'s bot named as "Spec default branch" in its own auto-filed issue #205, independently corroborating the target) · **Planner:** Opus, with the full spec diff, the current vendored file, and `tests/parity_contract.rs` read directly, not from memory.

**Models for execution:** Opus, single phase, **Risk: simple**. No production code changes and no test-assertion changes — this is test-fixture vendoring plus one CI config line. No Fable: no one-way door, no trust boundary, no cross-repo write (the spec repo is read-only here, same as the `#169`/`#170` and `#197`/`#199` precedents).

---

## Context

Issue #204 asks for exactly one thing: re-vendor `tests/parity/contract.json` at the spec repo's `contract_version` 1.2.0 and catch `SPEC_REV` up to a revision at or after `45406dd`. Unlike the `#197`/`#199` precedent (`plans/spec-drift-catchup-197-199.md`, PR #200), this bump requires **no** change to `tests/parity_contract.rs` — verified directly, not assumed (see below). It is the smallest possible instance of this repo's re-vendor pattern.

**The exact upstream diff** (`git diff` on `schemas/parity/contract.json` between the current pin `2f7557c` and spec commit `45406dd`, confirmed byte-for-byte against the sibling checkout — matches issue #204's own description exactly, no discrepancy found):

1. `contract_version`: `"1.1.1"` → `"1.2.0"` (today: `tests/parity/contract.json:3`).
2. `sections.projection_anomaly.kinds` (today: `tests/parity/contract.json:99-101`) grows from 2 to 4 entries, appended in this order:
   ```json
   "kinds": [
     "duplicate_vote",
     "duplicate_ballot",
     "duplicate_task_accept",
     "settled_handoff"
   ]
   ```
3. `sections.projection_anomaly.source` (today: `tests/parity/contract.json:98`) is rewritten — same underlying claim, corrected and expanded: fixes a stale "closed 2-value union" reference to macp-sdk-typescript's `ProjectionAnomalyKind` (now 4-member), cites the two SDK PRs that landed the new kinds (macp-sdk-python #95, macp-sdk-typescript #134), and replaces an "open question" sentence with what actually settled. This is why the file must be re-vendored byte-for-byte rather than hand-patched — the prose changed independently of the structural fields.

`sections.projection_anomaly.applies_to` is **unchanged**: still exactly `["macp-sdk-python", "macp-sdk-typescript"]` (confirmed by reading the full section before and after — the `applies_to` array is untouched by this commit). No field, section, or `applies_to` list changes anywhere in the file; `wc -l` shows the file grows from 214 to 216 lines (net +2, from the two new array entries), everything else byte-identical.

**Why no Rust code or test change is needed — verified, not asserted:**
- `grep -ri anomaly` over `src/`, `crates/`, and `tests/` returns exactly two hits, both inside `tests/parity_contract.rs`'s own doc comment and its `ALL_SECTIONS` constant (`:20`, `:67`, `:81`) — there is no anomaly concept anywhere in this runtime's production code.
- `tests/parity_contract.rs`'s `HANDLED` constant (`:55-63`) does **not** include `"projection_anomaly"` — only `ALL_SECTIONS` (`:72-82`) does, and that list only ever checks the section *name* exists, never its contents. `projection_anomaly` was SDK-only before this bump and stays SDK-only after it (its `applies_to` didn't move) — the coverage guard `every_macp_runtime_section_is_handled` (`:178-194`) is unaffected either way.
- No test anywhere reads `sections.projection_anomaly.kinds`, `.source`, or `.applies_to` — confirmed by grepping `tests/parity_contract.rs` for `"kinds"` and `"projection_anomaly"`: the only three matches are the two doc/constant lines above.
- The one assertion that does read `contract_version` is `contract_version_is_1_x` (`tests/parity_contract.rs:166-176`), which asserts `starts_with("1.")` — `"1.2.0"` satisfies this unchanged, exactly as the issue states.
- Ran the current suite locally against the pre-bump vendored file as a baseline: `cargo test --test parity_contract` → **17 passed, 0 failed** (all 17 names read out; this is the baseline this phase's acceptance criteria compare against — the count must stay 17 after the bump, since nothing new is being asserted).

**Confirmed: `schemas/json/policy/**` and `schemas/conformance/**` did not change in this range** (CLAUDE.md §9 explicitly asks this to be stated, not silently skipped): `git diff --stat 2f7557c72082d20c0809cb025c2ba72270a7b068..18f233229e6cb1156351c68614610a9d3bb40497 -- schemas/json/policy/ schemas/conformance/` in the sibling spec repo produces **empty output** — zero files differ in either tree across the *entire* range to the target revision, not just up to `45406dd`. `crates/macp-policy/src/registry.rs`'s `validate_conditional_constraints` and the `enum_lists_match_the_canonical_schemas` test therefore need **no re-check beyond confirming this emptiness** — there is nothing to re-check them against. This mirror needs no changes.

Path-filtering the same range to the three trees `conformance-oracle` actually consumes turns up exactly three commits, matching `spec-drift.yml`'s own auto-filed issue #205 verbatim ("Commits since the pin"):
- `45406dd` `spec(parity): mirror the settled anomaly-kind agreement at contract 1.2.0 (#158)` — the only one touching `contract.json` itself.
- `b9a9781` `tooling(prose-check): wire prose-check-selftest into CI, add parity held-value enumeration check (#157) (#164)` — touches only `schemas/parity/README.md`.
- `4992342` `spec(docs): record the SDK naming-divergence decision rule (#135) (#166)` — touches only `schemas/parity/README.md`.

`schemas/parity/README.md` is out of scope for this repo: it is not vendored (`tests/parity/` has no `README.md` copy — only `SOURCE.md`, a different, this-repo-specific provenance doc) and sits outside `check_dir`'s `*.json` glob (`ci.yml:611` note, `:623` glob loop) — a README-only upstream change has no local artifact to update. This is also why the pin target below is spec `main` rather than stopping exactly at `45406dd`: nothing between `45406dd` and `main` touches any tree this repo consumes.

Six more paths outside the three consumed trees also changed in the full `2f7557c..18f2332` range (`macp-envelope.schema.json`, RFC prose, `agent-bootstrap` fixtures, `check-prose.py`, etc., per `git diff --stat`) — none is vendored anywhere in `macp-runtime` (this repo has no `schemas/` directory per CLAUDE.md's Policy evaluation section) and none is referenced by any test here. No action on any of them.

**Why the pin target is spec `main` (`18f2332`), not `45406dd` itself.** Between those two commits, nothing under any of the three consumed trees changes (shown above), so pinning at `main` costs nothing beyond `45406dd` and genuinely closes the drift-watcher's concern (`spec-drift.yml` diffs against spec `main`, not a fixed commit — pinning short of `main` would just have the daily cron refile the same gap under a new issue). This is the same reasoning `plans/spec-drift-catchup-197-199.md`'s Context section used for its own pin choice, and here it's additionally corroborated by `spec-drift.yml`'s bot-filed issue #205 (closed, per issue #204's own acceptance box, "in favour of #204") independently naming this exact commit as "Spec default branch."

**Why this lands as one phase, not two** (same coupling `plans/spec-drift-catchup-197-199.md` already established): `tests/parity/SOURCE.md` states the vendored copy "must stay equal to `SPEC_REV`," and `conformance-oracle`'s `check_dir` step (`ci.yml:632-635`) byte-compares `tests/parity/*.json` against `spec-repo/schemas/parity/*.json` **checked out at `SPEC_REV`**. Re-vendoring without bumping the pin reports `DRIFT`; bumping the pin without re-vendoring reports `DRIFT` in the other direction. No intermediate state is independently green.

**Scope.** One phase, entirely in `macp-runtime`. No cross-repo writes. Closes **#204**. No further action needed on #205 — the issue body itself already records it as closed in #204's favor.

---

## Phase 1 — Re-vendor `contract.json`, bump `SPEC_REV`

- **Status:** DONE (2026-09-30) — implemented exactly as planned, no divergence. All 10 acceptance criteria verified: byte-identity against a clean `git archive` export at `18f2332` (not the sibling working tree), `SOURCE.md`/`ci.yml` pin updated, `cargo test --test parity_contract` 17/17 both against the vendored copy and against the canonical export via `MACP_PARITY_CONTRACT`, `conformance_loader` 35/35 and the policy-registry enum-parity test both green against the canonical export (confirming the "zero diff in `schemas/conformance/`/`schemas/json/policy/`" analysis empirically, not just by inspection), the two prove-then-restore negative checks behaved as specified (byte-flip → DRIFT, bad `contract_version` → targeted failure on `contract_version_is_1_x` only), `git diff --name-only` matched the predicted 3-file set exactly, full workspace suite green (36/36 binaries, 0 failures), `cargo fmt --check`/`cargo clippy --workspace --all-targets -- -D warnings`/`actionlint` all clean.
- **Risk:** simple — test-data vendoring plus one CI config line, no production code, no test-assertion change, fully reversible in a commit.
- **Delivers:** `tests/parity/contract.json` matches spec `main` byte-for-byte at `contract_version` 1.2.0; `.github/workflows/ci.yml`'s `SPEC_REV` pin catches up to it; `conformance-oracle` stays green at the new pin, proven locally before push. Closes **#204**.
- **Depends on:** nothing.
- **Files:**
  - `tests/parity/contract.json` — replaced byte-for-byte from the spec repo (see Approach). **Not hand-edited** — the `source` field's prose changed independently of the structural fields (see Context), so a value-by-value patch risks leaving stale prose even if the structural diff looks complete.
  - `tests/parity/SOURCE.md` — pinned commit/date (`:9-11`) updated from `2f7557c72082d20c0809cb025c2ba72270a7b068` / 2026-09-28 to `18f233229e6cb1156351c68614610a9d3bb40497` / 2026-09-30.
  - `.github/workflows/ci.yml:39` — `SPEC_REV` updated to the same commit.
  - **`tests/parity_contract.rs` — deliberately NOT touched.** No existing assertion reads `projection_anomaly.kinds`, `.source`, or `.applies_to` (verified in Context above), and `contract_version_is_1_x`'s `starts_with("1.")` check needs no update for `"1.2.0"`. Stating this explicitly rather than silently — the issue's own "No new assertion to wire" section is the reason, not an oversight.
  - `crates/macp-policy/src/registry.rs` — **NOT touched.** `schemas/json/policy/**` has zero diff across the whole range (Context above); nothing to re-check the mirrors against.
- **Approach:** Copy, don't hand-edit — `tests/parity/SOURCE.md`'s own documented procedure: `cp ../multiagentcoordinationprotocol/schemas/parity/contract.json tests/parity/contract.json`. The sibling checkout's `HEAD` already equals the target pin (`18f233229e6cb1156351c68614610a9d3bb40497`, confirmed via `git fetch` immediately before writing this plan) and its working tree is clean, so a direct `cp` is faithful.

  **Verification must not reuse that same sibling working tree.** Per `CLAUDE.md`'s Policy evaluation section ("point it at a clean `git archive` export of the spec commit CI reads, never at the sibling working tree — a dirty or ahead checkout produces failures that do not exist in CI") and this plan's own instruction: build a clean export before running any of the acceptance criteria below, e.g.
  ```
  D=$(mktemp -d)
  git -C ../multiagentcoordinationprotocol archive 18f233229e6cb1156351c68614610a9d3bb40497 | tar -x -C "$D"
  ```
  and point every env var (`MACP_PARITY_CONTRACT`, `MACP_CONFORMANCE_FIXTURES_DIR`, `MACP_POLICY_SCHEMAS_DIR`) at paths under `$D`, not at `../multiagentcoordinationprotocol/...` directly. This is exactly what `conformance-oracle`'s own `actions/checkout@v7` step does in CI (a fresh checkout at `SPEC_REV`, never the runner's ambient state).

  **Rejected: splitting into a "vendor" phase and a "bump pin" phase.** Same reasoning as `#197`/`#199` — the two are byte-coupled through `check_dir`; no independently-green intermediate state exists. **Rejected: pinning at `45406dd` instead of spec `main`.** Costs nothing to go further (zero tree changes between them, shown in Context) and stopping short leaves `spec-drift.yml`'s daily cron to refile the same gap.
- **Edge cases & failure modes:**
  - The CI job's "at least 17 `parity_contract` tests passed" guard (`ci.yml:672`) is unaffected — no test count changes, since no test function is added or removed.
  - `root_and_sections_have_no_unexpected_keys` (`tests/parity_contract.rs:147-164`) uses `ALL_SECTIONS`, which already lists `"projection_anomaly"` as a bare key name (`:81`) — unaffected by a content change inside that section.
  - `every_macp_runtime_section_is_handled` / `every_handled_section_still_exists` (`:178-208`) key off `applies_to`, which this bump does not touch — unaffected.
  - `defaults.policy_builder_schema_version` (`tests/parity/contract.json:42`, value `3`) is untouched by this diff — `defaults_policy_builder_schema_version_matches_documented_value` and `..._is_accepted_by_the_registry` need no attention.
  - If spec `main` has moved again by implementation time, re-diff the same three trees (`schemas/parity/`, `schemas/conformance/`, `schemas/json/policy/`) before assuming this plan's "no other action needed" conclusion still holds — don't just extend the pin without re-checking.
  - **Local environment note** (confirmed live this session, still reproducing): a bare `cargo build`/`test`/`clippy` in this checkout fails linking against the default SDK (`MacOSX27.0.sdk` — `tapi error: malformed file`, `unknown architecture arm64e.x1-macos`). `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk` unblocks every local Rust invocation — same workaround `plans/spec-drift-catchup-197-199.md` already documented, re-confirmed working on this machine today (`cargo test --test parity_contract` succeeded with it: 17/17).
- **Acceptance criteria:**
  1. `tests/parity/contract.json` is byte-identical to a fresh `git archive` export (per Approach) of `schemas/parity/contract.json` at `18f233229e6cb1156351c68614610a9d3bb40497` (`diff` exits 0).
  2. `tests/parity/SOURCE.md`'s pinned commit and date read `18f233229e6cb1156351c68614610a9d3bb40497` / 2026-09-30.
  3. `.github/workflows/ci.yml` contains `18f233229e6cb1156351c68614610a9d3bb40497` exactly once, as `SPEC_REV`, still a quoted 40-character lowercase hex string (this exact shape is what `spec-drift.yml`'s own pin-parsing regex expects: `.../SPEC_REV:[[:space:]]*"?([0-9a-f]+)"?.../`, `spec-drift.yml:64-65`).
  4. `cargo test --test parity_contract` passes at **17 passed, 0 failed** against the newly vendored copy (default path, no env var) — same count as the pre-bump baseline measured during planning, confirming criterion "no new assertion to wire" held.
  5. Reproduce `conformance-oracle`'s own steps against the clean `git archive` export `$D` from Approach (not the sibling working tree):
     - `check_dir`-equivalent byte identity, both directions, for `tests/parity` ↔ `$D/schemas/parity`: 0 MISSING / 0 DRIFT / 0 EXTRA.
     - `MACP_PARITY_CONTRACT=$D/schemas/parity/contract.json cargo test --test parity_contract` — 17 passed, 0 failed (this is what actually exercises the runner against canonical content, not just the vendored copy's internal consistency).
     - `MACP_CONFORMANCE_FIXTURES_DIR=$D/schemas/conformance cargo test --test conformance_loader` — same pass count as baseline (confirms, doesn't just assume, the "zero `schemas/conformance/` diff" finding).
     - `MACP_POLICY_SCHEMAS_DIR=$D/schemas/json/policy cargo test -p macp-policy --lib -- --exact registry::tests::enum_lists_match_the_canonical_schemas` — `1 passed` (confirms the "zero `schemas/json/policy/` diff" finding).
  6. `git diff --name-only` for this phase's commit, excluding `plans/`, is exactly: `.github/workflows/ci.yml`, `tests/parity/contract.json`, `tests/parity/SOURCE.md`. Any other file touched (in particular `tests/parity_contract.rs`) means the "no test-assertion change needed" analysis in Context was wrong and needs revisiting, not patching around.
  7. **Prove-then-restore, on scratch copies — not the tracked files.** Since no Rust assertion reads the changed `kinds`/`source` fields (the entire point of "no new assertion to wire"), the two meaningful negative-path checks are the byte-identity gate and the one assertion that *does* read a changed field (`contract_version`):
     - Copy the newly-vendored `tests/parity/contract.json` to a scratch path, flip one byte inside the new `kinds` array, point the `check_dir` logic (replayed per criterion 5) at the scratch copy instead of the real one → reports `DRIFT`, not silence.
     - In a second scratch copy, change `contract_version` to a value that does not start with `"1."` (e.g. `"2.0.0"`) → `cargo test --test parity_contract` against that scratch copy fails specifically on `contract_version_is_1_x`, not a generic or unrelated failure.
     Both scratch copies are discarded, never committed — this criterion is discharged by the run output recorded in `PROGRESS.md`, not by a file in the diff.
  8. Full workspace suite green, matching CI's actual invocations: `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (`ci.yml:159` — already `--workspace`-scoped repo-wide since #201/#203; confirmed clean against the current tree during planning, 0 warnings). All three run with the `RUSTC_WRAPPER`/`SDKROOT` workaround above.
  9. `actionlint .github/workflows/ci.yml` clean (confirmed clean on the pre-change file during planning; re-run post-edit since this phase touches one line of it).
  10. After merge: #204 closed (via PR body `Closes #204`) with an evidence comment citing the merged commit and criterion 5's measured results — same pattern as the `#169`/`#170` and `#197`/`#199` precedents. No action needed on #205 (already closed, per the issue body, in favor of #204).
- **Tests:** criterion 5 *is* `conformance-oracle` reproduced locally — the load-bearing test here, since there is no new runtime logic and no new assertion to unit-test. `cargo test --test parity_contract` (criteria 4 and 5) is the whole test surface this phase touches.
- **Docs:** none beyond `tests/parity/SOURCE.md`'s own commit/date fields (already in Files above). `docs/testing.md`'s description of the oracle mechanism is generic (no literal `SPEC_REV` value cited there — confirmed by grep) and needs no edit. `CHANGELOG.md` needs no hand edit — `[Unreleased]` is empty and release-plz generates entries from conventional-commit history on the next release PR, same convention `plans/parity-contract-176.md`'s Context established and `plans/spec-drift-catchup-197-199.md` followed.

---

## Long-term posture

Not a one-way door. Vendored test data and a CI pin, both trivially revertible. No public API, schema, or wire-format change — `CLAUDE.md`'s `#[non_exhaustive]` mode-state sealing and `cargo semver-checks` are unaffected (nothing under `crates/` changes). No new assertion is introduced, so there is no future removal cost either — this phase adds zero surface area, only moves a pin and a vendored file forward.

## Enterprise concerns

None beyond what `conformance-oracle` already guards: this phase's whole job is proving that guard stays green at the new pin *before* the pin moves in CI, not after. No new failure domain — the change makes an existing quarterly maintenance mechanism ordinary work. No observability, migration, or rollback story needed — nothing touches persisted state, the wire protocol, or a deployed artifact.

## Open questions

None requiring escalation. The one judgment call — pinning at spec `main` (`18f2332`) rather than stopping exactly at the commit issue #204 names (`45406dd`) — is decided and recorded in Context above (reversible, no defensible reason to prefer the older pin once the diff between them is confirmed empty for every tree this repo consumes, and independently corroborated by `spec-drift.yml`'s own bot-filed issue #205 naming the same target).

## Repo map

See `plans/parity-contract-204-PROGRESS.md`.

## Plan review

**Round 1 (self-review against live code and the sibling spec-repo checkout, not from memory):** Every citation below was re-read directly during this planning session, not carried over from the `#176`/`#197`/`#199` precedents' text:
- The exact `contract.json` diff (`contract_version`, `kinds`, `source`) was diffed live between the current vendored file, the current pin `2f7557c`, and the target `18f2332` — all three agree and match issue #204's own description exactly, no discrepancy found.
- `schemas/json/policy/**` and `schemas/conformance/**` were diffed live across the *entire* `2f7557c..18f2332` range (not just to `45406dd`) — confirmed empty, stated explicitly per the task's instruction rather than silently skipped.
- `tests/parity_contract.rs` was read in full; confirmed no assertion reads the changed fields, and the current baseline (`cargo test --test parity_contract`, 17/17) was actually run against the pre-bump file, not assumed.
- `.github/workflows/ci.yml`'s `SPEC_REV` line, `check_dir` calls, and the "Run parity-contract suite against canonical manifest" guard were all read at their current line numbers (`:39`, `:632-634`, `:663-673`) rather than cited from an older plan.
- `.github/workflows/spec-drift.yml`'s pin-parsing regex, watched-tree loop, and remediation checklist were confirmed to already include `schemas/parity` end-to-end (landed by `#176`) — no further edit needed there, which this plan states explicitly rather than silently omitting a `spec-drift.yml` entry from Files.
- The local `SDKROOT` build workaround was re-confirmed live on this machine today (current default SDK is `MacOSX27.0.sdk`, one version ahead of what `#197`/`#199`'s plan recorded) — `cargo test`, `cargo fmt --check`, and `cargo clippy --workspace --all-targets -- -D warnings` were all run with the workaround during planning and are clean, so (unlike `#197`/`#199`) there is no pre-existing-failure caveat to carry into this plan's acceptance criteria.
- Confirmed live: `git fetch origin main` in the sibling checkout immediately before finalizing this plan, and independently cross-checked against `spec-drift.yml`'s own auto-filed issue #205's title/body, which names the identical target commit.

No blocking or non-blocking findings changed the plan's shape from a single, simple phase — this is a smaller, more mechanical instance of the `#197`/`#199` pattern (one fewer file touched, since no test assertion needs updating), so a second adversarial round would be re-verifying citations this round already pinned down directly against live sources. Considered ready for `/implement`.
