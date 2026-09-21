# Progress: parity-contract-176

PR strategy: **one PR**, three phase-commits (see plan's Open Questions section for why).

## Repo map

Files this plan's phases touch or read, one line each:

- `crates/macp-modes/src/mode/util.rs` — `is_canonical_commitment_hash` (private, :80-90) →
  `#[doc(hidden)] pub fn` in Phase 1 (not plain `pub` — see plan's Phase 1 Approach); sole
  call site today is `validate_commitment_payload_for_session` (:60) in the same file;
  crate has no `pub(crate)` tier, only `pub`/private.
- `crates/macp-modes/src/mode/multi_round.rs` — `parse_contribute_value` (private, :57-72)
  → `#[doc(hidden)] pub fn` in Phase 1; sole call site is `handle_contribute` (:168); decode
  order is JSON-then-protobuf, documented and load-bearing at :50-56.
- `crates/macp-modes/src/mode/mod.rs` — `pub mod util;` (:8), `pub mod multi_round;` (:3),
  `STANDARD_MODE_NAMES` (:16-22, the 5 standard mode IDs), `EXTENSION_MODE_NAMES` (:25,
  `ext.multi_round.v1`) — both arrays already match the manifest's `modes` section exactly,
  reused as-is by Phase 2, no new constant needed here.
- `crates/macp-core/src/lib.rs` — module-declaration file, no top-level `pub const` today;
  gets `MACP_VERSION` added in Phase 1.
- `crates/macp-core/src/error.rs` — `MacpError` enum (`#[non_exhaustive]`, :7-51),
  `error_code()` (:55-78, exhaustive match over current variants), existing
  `error_code_mapping_covers_all_variants` test (:85-130) — cross-referenced, not
  duplicated, by Phase 2's runner.
- `crates/macp-policy/src/defaults.rs` — `DEFAULT_POLICY_ID` (:23, `"policy.default"`,
  already `pub const`), reused as-is by Phase 2 for `defaults.policy_version`.
- `src/server.rs` — three production `"1.0"` literals (:115 envelope-shape gate, :803
  `Initialize` negotiation, :810 `Initialize` response) → `MACP_VERSION` in Phase 1.
- `src/runtime.rs` — two production `"1.0"` literals (:210, :298) → `MACP_VERSION`.
- `src/replay.rs` — two production `"1.0"` literals (:95, :329) → `MACP_VERSION`.
- `crates/macp-modes/src/mode/handoff.rs` — one production `"1.0"` literal (:446,
  synthetic implicit-accept envelope) → `MACP_VERSION`.
- `src/bin/support/common.rs` — two production `"1.0"` literals (:84, :100), example-client
  code → `MACP_VERSION`. `MODE_VERSION`/`CONFIG_VERSION`/`POLICY_VERSION` (:19-21) all
  become re-exports of Phase 1's new `macp-core`/`macp-policy` constants in Phase 1 itself
  (names preserved exactly — `CONFIG_VERSION`, not renamed — since :44 and :68 in this same
  file reference it by that name).
- `src/lib.rs` — re-export surface: `pub use macp_modes::{mode, mode_registry};` (:31),
  `pub use macp_policy as policy;` (:32), `pub use macp_core;` (:18) — confirms
  `macp_core::MACP_VERSION`, `macp_modes::mode::util::is_canonical_commitment_hash`,
  `macp_modes::mode::multi_round::parse_contribute_value`, and
  `macp_policy::defaults::DEFAULT_POLICY_ID` are all reachable from `tests/*.rs` with zero
  `Cargo.toml` changes (all four crates are normal, non-dev `[dependencies]` of the root
  package: `Cargo.toml:79,81,82`).
- `tests/cmt_hash_vectors.rs` — structural model for `tests/parity_contract.rs`: module doc
  explaining its relation to `conformance_loader.rs` (:1-12), one `#[test]` that loops with
  an up-front count assertion (:98-139), a second targeted `#[test]` (:147-174), no macro,
  no env override on its own `vectors_dir()` (:74-76, unlike `conformance_loader.rs`).
- `tests/conformance_loader.rs` — source of the env-override idiom actually reused:
  `fixtures_dir()` (:524-535, exact pattern to mirror) and the `conformance_test!` macro
  (:537-545, NOT reused — parity runner has no per-fixture-file registration need since it
  vendors one file). Structural-guard precedent: `every_fixture_is_registered` (:696-720).
- `.github/workflows/ci.yml` — `conformance-oracle` job (:572-673); `check_dir()` function
  body (:595-623, bidirectional `*.json` byte-diff, glob-by-basename, no recursion); two
  existing calls (:624-625); spec-repo checkout, once, reused (:580-585, `ref:
  ${{ env.SPEC_REV }}`, `path: spec-repo`); `SPEC_REV` definition (:39) plus its one other
  reference (:584) — confirmed the *only* two lines in the file mentioning it; env-var
  wiring pattern for a "run against canonical" step (`MACP_CONFORMANCE_FIXTURES_DIR`
  :645-646, `MACP_POLICY_SCHEMAS_DIR` :663-664).
- `.github/workflows/spec-drift.yml` — watched-tree loop (:128, currently
  `schemas/conformance schemas/json/policy`); case-statement classifier (:210-245,
  `*)` catch-all at :242-244 defaults to `actionable`); header comment (:16-19) and two
  hardcoded tree-name mentions in user-facing issue text (:110, :323).
- `docs/testing.md` — 132 lines, section headings noted in investigation; no existing
  vendoring/provenance/SPEC_REV-bump how-to for contributors; gets a new short subsection
  under `## Unit tests and conformance` in Phase 2.
- `CHANGELOG.md` — since release-plz adoption (`6172d55`, 2026-07-09), exactly two
  non-bot commits touch it (`c8ff39b`, `8c92ff4`, both release-record repairs, not
  feature PRs); #171 and #175 never touched it themselves — **out of scope**, corrects
  the issue's own Files table.
- No `tests/*SOURCE*`/`*PROVENANCE*` file exists anywhere in the repo today (confirmed via
  repo-wide search) — Phase 2's `tests/parity/SOURCE.md` is a new, minimal convention, not
  an extension of one.
- Spec repo (`../multiagentcoordinationprotocol`): `schemas/parity/contract.json` +
  `schemas/parity/README.md` exist at current `main` HEAD, commit `4f15b96c...` (also the
  commit that introduced the file — no prior history, no changes since). Ancestry:
  `0de1fab2` (currently pinned `SPEC_REV`) → `768592b` → `4f15b96c` (HEAD). Two commits
  behind; only `4f15b96c` touches `schemas/parity/`.

## Round-1 review additions to the repo map

- `crates/macp-core/src/session.rs:86` — `CURRENT_SEMANTICS_REV`, the precedent for where
  `DEFAULT_MODE_VERSION`/`DEFAULT_CONFIGURATION_VERSION` now go (Phase 1).
- `schemas/parity/README.md` (spec repo) — the real normative doc for this manifest:
  Sections table, `applies_to`-is-a-MUST language, Versioning rules (PATCH/MINOR/MAJOR),
  and an "Open items" section listing exactly **five** known gaps (not four — corrected in
  round 2) — `policy_builder_schema_version` is not one of them, which resolved Phase 2's
  `defaults` design without a cross-repo issue (though round 2 still required adding a real
  behavioral assertion alongside the literal one — see the plan's Phase 2 Approach).
- `schemas/json/macp-parity-contract.schema.json` (spec repo) — the canonical JSON Schema
  for the manifest's *shape*. Every object (root, `sections`, and each individual section)
  has `additionalProperties: false` + `patternProperties: {"^(_|\$comment)": {}}` — this is
  what broke the original `deny_unknown_fields` design; now handled by a manual key check
  (plain `str::starts_with`, not `regex` — round 2 caught that `regex` is only a transitive
  dependency here) applied at the root and `sections`-map levels using **two distinct
  allowlists**: `ALL_SECTIONS` (9 names) for the sections-map key check, `HANDLED` (7
  names) for the separate coverage guard — round 2 caught an earlier draft conflating the
  two, which would have rejected the manifest's legitimate `retry`/`projection_anomaly`
  keys.
- `macp-sdk-python/tests/vectors/cmt-hash/SOURCE.md` (sibling SDK repo, present locally at
  `/Users/Shared/multiagentcoordinationprotocol/macp-sdk-python/`) — the real provenance
  template `tests/parity/SOURCE.md` is now modeled on directly (source path + pinned
  commit + date, then a short gating explanation).
- No hex crate anywhere in `Cargo.lock` (confirmed via `grep -c 'name = "hex"'`) — Phase 2
  hand-rolls `hex_encode`/`hex_decode` locally in `tests/parity_contract.rs` instead of
  adding a dependency.
- `ci.yml:670-673` — the exact `grep -q '^test result: ok\. 1 passed'` collection-guard
  pattern Phase 3's new "run against canonical" step's guard is modeled on (generalized to
  "at least N passed" rather than exactly 1, since `parity_contract.rs` has multiple
  tests).

## Phase checkpoint log

### Phase 1 — Expose the real predicates; introduce real version/default constants
- **Date:** 2026-09-20
- **Verdict:** PASS (1 round, fresh Opus verifier, no Fable — not a one-way door: both
  predicates get `#[doc(hidden)]`, not plain `pub`, which is the whole point)
- **Gap summary:** none required fixing (PASS on round 1). Verifier surfaced two items
  needing inline correction rather than code changes, both applied to
  `plans/parity-contract-176.md`'s Phase 1 section directly: (1) the plan's test-count
  acceptance criterion was self-contradictory (claimed "identical to baseline" while the
  same phase's Tests section required 2 new tests) — corrected to "baseline + 2
  (871 → 873)"; (2) the `cargo semver-checks` acceptance criterion's local run was a
  silent tool crash (rustdoc format v57 unsupported by local `cargo-semver-checks`
  0.45.0), not a genuine clean pass — noted inline as deferred to CI, per PR #173's
  precedent; verifier independently hand-confirmed the API delta is additive-only.
- **Files touched:** `crates/macp-core/src/lib.rs`, `crates/macp-core/src/session.rs`,
  `crates/macp-modes/src/mode/handoff.rs`, `crates/macp-modes/src/mode/multi_round.rs`,
  `crates/macp-modes/src/mode/util.rs`, `src/bin/support/common.rs`, `src/replay.rs`,
  `src/runtime.rs`, `src/server.rs`. No repo docs touched (Phase 1's own `Docs: None` was
  correct — confirmed, nothing describes these functions' visibility today).
- **Test evidence:** `cargo test --workspace` 873 passed, 0 failed (baseline 871 + 2 new:
  `macp_version_value`, `default_mode_and_configuration_version_values`). `cargo fmt
  --check` clean. `cargo doc --workspace -D warnings` clean. `cargo clippy --workspace
  --all-targets -- -D warnings` has one failure, confirmed pre-existing via `git stash`
  (identical failure on unmodified code) and out of scope for this phase.
- **What's next:** Phase 2 — vendor `tests/parity/contract.json` + `tests/parity/
  SOURCE.md`, write `tests/parity_contract.rs`.
