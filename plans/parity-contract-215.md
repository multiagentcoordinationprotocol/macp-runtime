# PLAN — Re-vendor `tests/parity/contract.json` at 1.3.0, wire up `proposal_disposition` (#215)

**Written:** 2026-10-03 · **Base:** `65b108f` on `main` (workspace release `v0.8.6`, local tree clean, 1 commit behind `origin/main` at `22e6a78`, a harmless `Cargo.lock` dep-bump touching nothing relevant here) · **Spec pin today:** `18f233229e6cb1156351c68614610a9d3bb40497` (`ci.yml:39`) · **Spec `main`:** `7159afe384f50b67c9c090796d926441eaa9acf4` (confirmed live via `git fetch origin` in the sibling checkout at plan-writing time, re-confirmed unchanged by a fresh-agent verification pass; working tree clean) · **Planner:** Opus, with the full spec diff, the current vendored file, `tests/parity_contract.rs`, and `crates/macp-modes/src/mode/proposal.rs` read directly, not from memory. Plan re-verified by a fresh general-purpose agent against live sources before implementation — PASS, four stale citations corrected below (ci.yml line numbers, a line-count arithmetic slip, a tree-path typo, this header's base commit).

**Models for execution:** Opus, single phase, **Risk: simple**. One new test assertion against an already-existing, already-public 2-variant enum; no new production code; no schema/policy changes. No Fable: no one-way door, no trust boundary, no cross-repo write (the spec repo is read-only here, same as the `#169`/`#170`, `#197`/`#199`, and `#204` precedents).

---

## Context

Issue #215 is the 4-part follow-up #204's own closeout predicted: the spec repo's `schemas/parity/contract.json` moved `1.2.0` → `1.3.0` (spec PR #179, merge commit `2989f64`, merged 2026-10-02), adding exactly one new top-level section, `proposal_disposition`. Unlike #204 (a pure content bump inside an already-`HANDLED` SDK-only section), this bump adds a **new** section whose `applies_to` names `macp-runtime` — so it requires a genuine new assertion, not just a re-vendor.

**The exact upstream diff** (`git diff` on `schemas/parity/contract.json` between the current pin `18f2332` and spec `main` `7159afe`, confirmed byte-for-byte against the sibling checkout):

1. `contract_version`: `"1.2.0"` → `"1.3.0"` (today: `tests/parity/contract.json:3`).
2. One new section appended at the end of `sections` (today: after `tests/parity/contract.json:213`):
   ```json
   "proposal_disposition": {
     "applies_to": ["macp-runtime", "macp-sdk-python", "macp-sdk-typescript"],
     "source": "...",
     "mode_state_dispositions": ["Live", "Withdrawn"],
     "projection_status_values": ["open", "rejected", "withdrawn"],
     "acceptance_tracking": "per_sender"
   }
   ```
   Nothing else in the file changes — confirmed by diffing the full file, not just the sections the issue names. `wc -l` grows from 216 to 234 lines (net +18, matching `git diff --stat`'s 20 insertions/2 deletions), all of it the new section; every other section is byte-identical.

**What `proposal_disposition` pins, and why it matches today's code without a code change:** `mode_state_dispositions` is `["Live", "Withdrawn"]` — exactly `crates/macp-modes/src/mode/proposal.rs`'s existing `ProposalDisposition` enum (`pub enum ProposalDisposition { Live, Withdrawn }`, no `#[non_exhaustive]`, no serde rename attributes — confirmed by reading the enum definition directly). This section's own `source` prose states the asymmetry deliberately: Proposal mode's disposition domain has no `Accepted` variant (unlike Handoff's `HandoffDisposition`, which does), because acceptance is tracked separately via a per-sender accepts map (`acceptance_tracking: "per_sender"`) rather than denormalized onto the disposition field. This is a pin of existing, shipped behavior — no runtime code changes, only a new test that holds the pin to the real enum.

`projection_status_values` (`["open", "rejected", "withdrawn"]`) and `acceptance_tracking` (`"per_sender"`) are convention-sourced against the two SDKs' own projections, not against anything in this runtime — `macp-runtime` has no `ProposalRecord`-style read-side projection distinct from `ProposalState`/`ProposalDisposition` to compare them to. Only `mode_state_dispositions` is this repo's to assert, same division of labor `commitment_hash`/`contribute_payload` etc. already use (assert exactly the sub-fields that project this runtime's own code, not the sibling SDKs' side of a shared section).

**The 4 wiring changes, confirmed independently against today's file (not just restating the issue):**

1. **`HANDLED`** (`tests/parity_contract.rs:55-63`) — add `"proposal_disposition"`. Required because its `applies_to` names `macp-runtime` (confirmed above) and `every_macp_runtime_section_is_handled` (`:178-194`) asserts every such section appears here.
2. **`ALL_SECTIONS`** (`tests/parity_contract.rs:72-82`) — add `"proposal_disposition"`. Required because `root_and_sections_have_no_unexpected_keys` (`:147-164`) allowlists the `sections` map's keys against this constant; without it, the new key is `unexpected key 'proposal_disposition'` the moment `SPEC_REV` bumps.
3. **The actual assertion** — new test function asserting `ProposalDisposition`'s variant set equals `proposal_disposition.mode_state_dispositions`, plus a guard that the set contains no `Accepted`-shaped value (the asymmetry the section's own `source` field documents, see Context above). `HANDLED` listing the section with no corresponding test would make `every_handled_section_still_exists` (`:196-208`) pass vacuously while asserting nothing — the same gap `every_macp_runtime_section_is_handled` is designed to catch, so the new test is not optional once (1) lands.
4. **Test count in `ci.yml`** — two sites, both read directly at current line numbers:
   - Comment at `ci.yml:658` ("`tests/parity_contract.rs` currently defines 17 `#[test]` functions") → 18.
   - The guard itself at `ci.yml:671` (`if [ -z "$passed" ] || [ "$passed" -lt 17 ]`) → `-lt 18`. Confirmed live: `cargo test --test parity_contract` today reports exactly **17 passed** (ran locally against the pre-bump vendored file as this phase's baseline); adding exactly one new `#[test]` fn makes 18 the new floor.

**Confirmed: `schemas/json/policy/**` and `schemas/conformance/**` have zero diff** across the whole pinned-to-target range (`git diff --stat 18f2332..origin/main -- schemas/json/policy/ schemas/conformance/` in the sibling checkout → empty output). `crates/macp-policy/src/registry.rs`'s mirrors need no re-check beyond this emptiness.

Path-filtering the full range to the three spec-repo trees `conformance-oracle` actually consumes (`schemas/conformance`, `schemas/conformance/cmt-hash`, `schemas/parity`) turns up exactly one commit: `2989f64` — the parity-contract bump itself. Three other commits in the full range (`7159afe`, `909cca8`, `a7272fa`) touch `docs/`, `Makefile`, `README.md`, envelope-coverage tooling, and other schema files — none vendored anywhere in this repo (confirmed via full `git diff --stat`), none referenced by any test here. No action on any of them. `schemas/parity/README.md` also changed in-range but is out of scope for the same reason #204's plan already established: not vendored here (`tests/parity/` has only `SOURCE.md`, no `README.md` copy) and outside `check_dir`'s `*.json` glob.

**Why the pin target is spec `main` (`7159afe`), not `2989f64` itself.** Nothing between those two commits touches any of the three consumed trees (shown above), so pinning at `main` costs nothing beyond `2989f64` and avoids `spec-drift.yml`'s daily cron refiling the same gap under a new issue the moment it next runs — same reasoning the `#204` and `#197`/`#199` plans already used.

**Why this lands as one phase, not split across "wire the assertion" and "bump the pin."** Same byte-coupling #204 and `#197`/`#199` established: `conformance-oracle`'s `check_dir` step byte-compares `tests/parity/*.json` against the spec repo checked out **at `SPEC_REV`**. Re-vendoring without bumping the pin reports `DRIFT`; bumping the pin without re-vendoring reports `DRIFT` the other direction; adding the `HANDLED`/`ALL_SECTIONS` entries without bumping the pin leaves `every_handled_section_still_exists` failing locally (section doesn't exist in the pre-bump manifest) even though CI would still be green until the pin moves. No independently-green intermediate state exists for any subset of the 4 changes plus the re-vendor.

**Scope.** One phase, entirely in `macp-runtime`. No cross-repo writes — the spec repo is read-only here. Closes **#215**.

---

## Phase 1 — Re-vendor `contract.json` to 1.3.0, wire up `proposal_disposition`, bump `SPEC_REV`

- **Status:** DONE (2026-10-03) — implemented exactly as planned, no divergence. All 10 acceptance criteria verified: byte-identity against a clean `git archive` export at `7159afe` (not the sibling working tree), `SOURCE.md`/`ci.yml` pin updated, `cargo test --test parity_contract` 18/18 both against the vendored copy and against the canonical export via `MACP_PARITY_CONTRACT`, `conformance_loader` 35/35 and `macp-policy`'s `enum_lists_match_the_canonical_schemas` both green against the same canonical export, two prove-then-restore negative checks behaved exactly as predicted on scratch copies (tracked file left untouched), full workspace suite green (`cargo test`, 0 failures across every binary), `cargo fmt --check`/`cargo clippy --workspace --all-targets -- -D warnings`/`actionlint` all clean. `git diff --name-only origin/main` shows exactly the predicted 4 files (`tests/parity/contract.json`, `tests/parity/SOURCE.md`, `.github/workflows/ci.yml`, `tests/parity_contract.rs`) — a 5th file, `Cargo.lock`, appears only when diffing against the local stale `main` because the feature branch was rebased onto `origin/main`'s tip (`22e6a78`, an unrelated pre-merged dependency bump), not because this phase touched it.
- **Risk:** simple — one new test function against an existing public enum, test-fixture vendoring, and two CI config lines. No production code touched. Fully reversible in a commit.
- **Delivers:** `tests/parity/contract.json` matches spec `main` byte-for-byte at `contract_version` 1.3.0; `tests/parity_contract.rs` asserts the new `proposal_disposition.mode_state_dispositions` against `ProposalDisposition`'s real variant set (plus the no-`Accepted` guard); `.github/workflows/ci.yml`'s `SPEC_REV` pin and 17→18 test-count guard both catch up; `conformance-oracle` stays green at the new pin, proven locally before push. Closes **#215**.
- **Depends on:** nothing.
- **Files:**
  - `tests/parity/contract.json` — replaced byte-for-byte from the spec repo via `cp`, per `tests/parity/SOURCE.md`'s documented procedure. **Not hand-edited** — same reasoning as #204: the new section's prose is substantial and must not be re-typed.
  - `tests/parity/SOURCE.md` — pinned commit/date (`:9-11`) updated from `18f233229e6cb1156351c68614610a9d3bb40497` / 2026-09-30 to `7159afe384f50b67c9c090796d926441eaa9acf4` / 2026-10-03.
  - `.github/workflows/ci.yml:39` — `SPEC_REV` updated to the same commit.
  - `.github/workflows/ci.yml` (two sites near `:654` and `:669`, exact lines confirmed at implementation time since the file shifts) — comment's "17" → "18"; guard's `-lt 17` → `-lt 18`.
  - `tests/parity_contract.rs`:
    - `HANDLED` (`:55-63`) — add `"proposal_disposition"`.
    - `ALL_SECTIONS` (`:72-82`) — add `"proposal_disposition"`.
    - New `use` of `macp_modes::mode::proposal::ProposalDisposition` (module and enum are already `pub`, confirmed — this is the first external reference to it, compiles with no visibility changes needed).
    - New section + test function, placed after the `contribute_acceptance` section (end of file), following the file's existing `// ─── name ───` section-comment convention:
      ```rust
      // ─── proposal_disposition ──────────────────────────────────────────────

      #[test]
      fn proposal_disposition_mode_state_set_matches_runtime_enum() {
          let contract = load_contract();
          let sec = section(&contract, "proposal_disposition");
          let dispositions: HashSet<&str> = str_array(sec, "mode_state_dispositions")
              .into_iter()
              .collect();

          // ProposalDisposition is a plain (non-#[non_exhaustive]) 2-variant
          // enum with no serde rename attributes, so its real Serialize impl
          // -- not a hand-written string match that could drift from it --
          // is what "real code" means here, same spirit as error_codes'
          // .error_code() calls.
          let variants = [ProposalDisposition::Live, ProposalDisposition::Withdrawn];
          let produced: HashSet<String> = variants
              .iter()
              .map(|d| {
                  serde_json::to_value(d)
                      .unwrap()
                      .as_str()
                      .unwrap_or_else(|| panic!("ProposalDisposition serializes to a non-string"))
                      .to_string()
              })
              .collect();
          let produced: HashSet<&str> = produced.iter().map(String::as_str).collect();

          assert_eq!(
              produced, dispositions,
              "runtime ProposalDisposition variants do not match the manifest's \
               proposal_disposition.mode_state_dispositions set"
          );

          // Proposal mode's disposition domain deliberately has no Accepted
          // value (unlike Handoff's HandoffDisposition) -- acceptance is
          // tracked separately via the per-sender accepts map, per this
          // section's own acceptance_tracking field. Guard the asymmetry
          // against silently regressing.
          assert!(
              !dispositions.contains("Accepted"),
              "proposal_disposition.mode_state_dispositions must not include an \
               acceptance-shaped value -- acceptance is tracked per-sender \
               (acceptance_tracking), not as a disposition"
          );
      }
      ```
      (The borrow-through-`String`-then-`&str` step avoids returning a reference into a temporary; written out in full here so implementation doesn't have to redesign it — a detail, not a design decision, so it's fixed now rather than left to be improvised mid-phase.)
  - `crates/macp-policy/src/registry.rs` — **NOT touched.** Confirmed zero diff in `schemas/json/policy/**` across the whole range (Context above).
  - `crates/macp-modes/src/mode/proposal.rs` — **NOT touched.** `ProposalDisposition` already matches the manifest; this phase only pins it with a test.
- **Approach:**
  1. Re-vendor: `cp ../multiagentcoordinationprotocol/schemas/parity/contract.json tests/parity/contract.json` (sibling checkout's `HEAD` already equals the target pin `7159afe384f50b67c9c090796d926441eaa9acf4`, confirmed via `git fetch origin` immediately before writing this plan; working tree clean).
  2. Update `tests/parity/SOURCE.md`'s commit/date and `.github/workflows/ci.yml:39`'s `SPEC_REV` to match.
  3. Add `"proposal_disposition"` to both `HANDLED` and `ALL_SECTIONS` in `tests/parity_contract.rs`.
  4. Add the `use` import and the new test function (above).
  5. Bump both `ci.yml` test-count sites from 17 to 18.
  6. Run `cargo test --test parity_contract` locally against the re-vendored file — expect **18 passed**, 0 failed.

  **Verification must not reuse the sibling working tree directly** — per `CLAUDE.md`'s Policy evaluation section and the `#204` precedent: build a clean export before running any acceptance criterion below:
  ```
  D=$(mktemp -d)
  git -C ../multiagentcoordinationprotocol archive 7159afe384f50b67c9c090796d926441eaa9acf4 | tar -x -C "$D"
  ```
  and point `MACP_PARITY_CONTRACT`, `MACP_CONFORMANCE_FIXTURES_DIR`, `MACP_POLICY_SCHEMAS_DIR` at paths under `$D`, matching exactly what `conformance-oracle`'s own fresh `actions/checkout@v7` does in CI.

  **Rejected: splitting into a "wire the assertion" phase and a "re-vendor + bump pin" phase.** Same byte-coupling reasoning as above — `HANDLED`/`ALL_SECTIONS` referencing a section absent from the still-1.2.0 vendored file fails `every_handled_section_still_exists` locally before the pin moves; there's no ordering of the 4 changes that is independently green partway through.
- **Edge cases & failure modes:**
  - `root_and_sections_have_no_unexpected_keys` (`:147-164`) will fail with `unexpected key 'proposal_disposition'` if the re-vendor lands before `ALL_SECTIONS` is updated — both must land in the same commit (they do; this is one phase).
  - `every_macp_runtime_section_is_handled` (`:178-194`) will fail with "section 'proposal_disposition' names macp-runtime in applies_to but has no matching assertion" if `HANDLED` is updated without the re-vendor (section doesn't exist yet to iterate) — order-independent in practice since all land together, but the two checks fail in opposite directions if partially applied, confirming there's no safe partial-apply order.
  - If `ProposalDisposition` ever gains a third variant, the hand-enumerated `variants` array in `proposal_disposition_mode_state_set_matches_runtime_enum` does **not**, by itself, fail on the new variant — it would simply never appear in `produced`, and the test would stay green as long as the manifest also didn't name it. **Verification-gate finding (2026-10-03): the `assert_eq!` alone does not enforce this** (empirically proven by injecting a third variant and re-running — the test still passed). Fixed by adding `assert_proposal_disposition_is_exhaustively_covered`, a separate `match` with no wildcard arm, called once per variant inside the `.map()` — this fails to *compile* (E0004) the moment a new variant exists, which is the actual enforcement this bullet originally claimed the `assert_eq!` provided.
  - The CI job's test-count guard uses `-lt 18` (at-least, not exact) deliberately, matching the existing `-lt 17` convention — a future phase adding more tests doesn't need to touch this guard's logic, only its floor.
- **Tests:**
  1. `cargo test --test parity_contract` against the re-vendored `tests/parity/contract.json` — expect 18 passed, 0 failed, including the new `proposal_disposition_mode_state_set_matches_runtime_enum`.
  2. Same suite against the clean `git archive` export (`MACP_PARITY_CONTRACT=$D/schemas/parity/contract.json`) — proves the runtime matches canonical directly, not merely the vendored copy, same as `conformance-oracle` does in CI.
  3. `cargo test --test conformance_loader` against the same clean export (`MACP_CONFORMANCE_FIXTURES_DIR=$D/schemas/conformance`) — confirms the zero-diff claim about `schemas/conformance/**` empirically, not just by inspection.
  4. `crates/macp-policy`'s `enum_lists_match_the_canonical_schemas` against the same export (`MACP_POLICY_SCHEMAS_DIR=$D/schemas/json/policy`) — confirms the zero-diff claim about `schemas/json/policy/**` empirically.
  5. Two prove-then-restore negative checks on scratch copies (never the tracked file): (a) flip one byte in the vendored `contract.json` → `check_dir`'s byte-identity logic would report `DRIFT` (reasoned through directly, since `check_dir` itself only runs in CI — not re-implemented locally); (b) remove `"Withdrawn"` from a scratch copy's `mode_state_dispositions` → `proposal_disposition_mode_state_set_matches_runtime_enum` fails with the expected assertion message, confirming the test actually has detection power and isn't vacuously true.
  6. Full workspace suite: `cargo test` (all binaries) — must stay green, confirming the new `use macp_modes::mode::proposal::ProposalDisposition` in a dev-dependency-only test crate doesn't perturb anything else.
  7. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` — clean.
  8. `actionlint` on `.github/workflows/ci.yml` — clean (catches any YAML/shell mistake in the two line-number edits).
- **Acceptance criteria:**
  1. `tests/parity/contract.json` byte-identical to spec commit `7159afe384f50b67c9c090796d926441eaa9acf4`'s `schemas/parity/contract.json` (verified against a clean `git archive` export, not the sibling working tree).
  2. `tests/parity/SOURCE.md` and `.github/workflows/ci.yml:39` both name that same commit.
  3. `tests/parity_contract.rs` has 18 `#[test]` functions; `cargo test --test parity_contract` reports 18 passed, 0 failed, both against the vendored copy and against the clean canonical export.
  4. `HANDLED` and `ALL_SECTIONS` both include `"proposal_disposition"`.
  5. The new test asserts `ProposalDisposition`'s variant set against `mode_state_dispositions` using the enum's real `Serialize` impl (not a hand-duplicated string list), and separately guards against an `Accepted`-shaped value.
  6. `.github/workflows/ci.yml`'s comment and guard both read 18, not 17.
  7. `conformance_loader` and `enum_lists_match_the_canonical_schemas` both still pass against the clean canonical export (proving the claimed zero-diff in `schemas/conformance/**` / `schemas/json/policy/**`).
  8. Full workspace suite, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `actionlint` all clean.
  9. Both negative checks (byte-flip, removed-variant) behave as predicted on scratch copies, with the tracked files left untouched by them.
  10. `git diff --name-only` against `main` shows exactly 4 files: `tests/parity/contract.json`, `tests/parity/SOURCE.md`, `.github/workflows/ci.yml`, `tests/parity_contract.rs`.
