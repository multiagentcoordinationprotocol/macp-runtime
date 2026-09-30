# PROGRESS — parity-contract-204

PR strategy: single PR, single phase, single commit. The vendored `contract.json`
and the `SPEC_REV` pin are byte-coupled through `conformance-oracle`'s `check_dir`
step (see plan Context) — there is no independently-green intermediate state to
split across two PRs or two phases. Unlike the `#197`/`#199` precedent, no
`tests/parity_contract.rs` change is needed either (verified in the plan's
Context — no assertion reads the changed `projection_anomaly` fields), so this
phase touches exactly 3 tracked files.

## Repo map

(Also captured in the plan's own "Repo map" pointer — duplicated here per
convention so `/implement` doesn't have to cross-reference.)

- `tests/parity/contract.json` — vendored, byte-identical copy of the spec repo's
  parity manifest. Re-vendored by direct `cp` per `tests/parity/SOURCE.md`. This
  phase moves it `1.1.1` → `1.2.0`: `contract_version`, `sections.projection_anomaly.kinds`
  (2 → 4 entries), and that section's `source` prose — nothing else.
- `tests/parity/SOURCE.md` — provenance doc: pinned commit/date, re-vendoring
  procedure, and the note that it must track `SPEC_REV`.
- `tests/parity_contract.rs` — 17 `#[test]` fns asserting the manifest's
  `macp-runtime`-relevant sections against real runtime code. **Not touched by
  this phase** — confirmed no assertion reads `projection_anomaly.kinds`/`.source`;
  `HANDLED` doesn't include that section, `ALL_SECTIONS` only checks the key name
  exists. `MACP_PARITY_CONTRACT` env var swaps the loaded file to the canonical
  spec-repo copy in CI.
- `.github/workflows/ci.yml` — `SPEC_REV` (line 39) pins the spec-repo commit;
  `conformance-oracle` job checks it out at `ref: ${{ env.SPEC_REV }}` (`:592`),
  runs `check_dir` byte-identity (both directions, `*.json` glob only) against
  `tests/conformance/`, `tests/conformance/cmt-hash/`, and `tests/parity/`
  (`:632-634`), then re-runs the conformance/parity/policy-parity suites directly
  against the canonical checkout (`:645-680`), including a "≥17 parity tests
  passed" collection guard (`:672`).
- `.github/workflows/spec-drift.yml` — **not touched.** Already includes
  `schemas/parity` in its watched-tree loop, commits-since-pin path list, and
  remediation checklist (landed by `#176`). This is the workflow whose daily cron
  auto-filed issue #205 for this exact gap, independently naming the same target
  commit this plan pins to.
- `crates/macp-policy/src/registry.rs` — `validate_conditional_constraints` +
  `enum_lists_match_the_canonical_schemas` test — **not touched.** Confirmed
  `schemas/json/policy/**` has zero diff across the whole pinned-to-target range.
- `tests/conformance/` — vendored conformance fixtures — **not touched.**
  Confirmed `schemas/conformance/**` has zero diff across the whole range.
- Sibling spec repo: `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol`,
  confirmed at `origin/main` = `18f233229e6cb1156351c68614610a9d3bb40497` (fetched
  live during planning), working tree clean.
- **Local environment blocker (still reproducing):** a bare `cargo build`/`test`/
  `clippy` fails in this checkout against the default SDK (`MacOSX27.0.sdk` —
  `tapi error: malformed file`). Use
  `RUSTC_WRAPPER="" SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk`
  for every local `cargo` invocation in this plan. Confirmed working during
  planning: `cargo test --test parity_contract` (17/17), `cargo fmt --check`
  (clean), `cargo clippy --workspace --all-targets -- -D warnings` (clean, 0
  warnings — no pre-existing-failure caveat to carry, unlike `#197`/`#199`).
- Precedent plans this one models on: `plans/spec-drift-catchup-197-199.md`
  (PR #200 — the direct predecessor for this exact task shape: re-vendor +
  `SPEC_REV` bump, single phase) and `plans/parity-contract-176.md` (the runner's
  original build-out, for `HANDLED`/`ALL_SECTIONS` design context).
- Related issues: #204 (this plan), #205 (spec-drift.yml's own auto-filed drift
  issue for the same gap — already closed in favor of #204 per #204's own body;
  no separate action needed on it).

## Checkpoints

- **2026-09-30** — Plan drafted (`plans/parity-contract-204.md`). Live spec-repo
  diff measured (`2f7557c..18f2332`, restricted to the three consumed trees, then
  widened to confirm nothing else in the full range matters); `tests/parity_contract.rs`
  read in full and its 17-test baseline actually run against the pre-bump vendored
  file; `ci.yml`/`spec-drift.yml` read at current line numbers; local `SDKROOT`
  build workaround re-confirmed working; `origin/main` re-fetched immediately
  before finalizing, cross-checked against issue #205's auto-filed title/body
  (independent corroboration of the same target commit).
- Plan review: single self-review round, no blocking findings — every citation
  was checked directly against live sources during drafting, not carried over
  from precedent plans' text. Considered ready for `/implement`.
- **2026-09-30** — Phase 1 executed on branch `feat/parity-contract-204-1.2.0`.
  Verifier tier: none used — this is a single, already-scrutinized plan phase
  whose acceptance criteria are themselves the verification (byte-identity,
  canonical-export test reproduction, prove-then-restore negatives); every
  criterion in `plans/parity-contract-204.md` Phase 1 ran and passed, one round,
  no gaps. Files touched: `tests/parity/contract.json`,
  `tests/parity/SOURCE.md`, `.github/workflows/ci.yml` — exactly the predicted
  set, confirmed via `git diff --name-only`. No `ASSUMPTIONS.md` entries — the
  plan's one judgment call (pin at spec `main` rather than `45406dd`) was
  already decided and recorded in the plan itself before execution, not made
  ambiguously during it.
- pushed feat/parity-contract-204-1.2.0 819e6ed
- PR #210 opened: https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/210
- **Next:** watch CI, merge on green. No action needed on #205 (already closed
  in favor of #204).
