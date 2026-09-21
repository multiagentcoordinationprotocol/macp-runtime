# Plan: proof-of-concept consumer of `schemas/parity/contract.json` (issue #176)

## Context

Spec-repo issue #134 proposed `schemas/parity/contract.json` — a small, hand-maintained,
explicitly non-normative manifest pinning values that already agree by convention across
`macp-runtime`, `macp-sdk-python`, and `macp-sdk-typescript` (protocol version, mode-id
sets, a few defaults, the canonical error-code set, the commitment-hash format, and the
`Contribute` payload byte encoding). That spec PR is merged: `schemas/parity/contract.json`
exists at spec-repo `main`, commit `4f15b96cac6e39d62925a5baa1ef80a42c2f818d` (that commit
*is* current spec `HEAD`, two commits ahead of this repo's currently pinned `SPEC_REV`
`0de1fab20bc396fdc5f1412e61fdc3d7baf0a64d`; the only change in that range touching
`schemas/parity/` is the commit that introduced the file itself — no drift since).

Issue #176 asks this repo to be the proof-of-concept consumer: vendor the manifest,
write a test runner that asserts it against this runtime's *real* code paths (not
reimplementations), and wire CI so the vendored copy can never silently drift from the
spec-repo original.

**This plan went through one full round of adversarial re-verification** (a fresh Opus
agent checking every citation against live code, plus a direct read of three companion
spec-repo artifacts this session's first pass had missed: `schemas/parity/README.md`,
`schemas/json/macp-parity-contract.schema.json`, and the SDKs' existing `SOURCE.md`
precedent at `macp-sdk-python/tests/vectors/cmt-hash/SOURCE.md`). That round found real
errors, which are corrected in place below rather than left as a separate erratum. See
"Plan review" at the bottom for the full round-1 verdict.

**Two claims from the issue's own inlined plan are corrected here** (verified false this
session, not just "unverified" as the issue's own caveat warned):

1. **The issue claims `defaults` is a section "not naming this repo"** (grouping it with
   `retry`/`projection_anomaly` as skippable). False: `schemas/parity/contract.json`
   `sections.defaults.applies_to` is `["macp-runtime", "macp-sdk-python",
   "macp-sdk-typescript"]`, read directly from the file. `defaults` is a **handled**
   section. Per `schemas/parity/README.md`'s own "Sections" table and its "Every section
   carries `applies_to` — which... **MUST** assert this section" language, this isn't
   optional: a consumer named in a section's `applies_to` is expected to actually assert
   it, and the manifest's own CI story is designed around exactly this becoming visible
   drift when a consumer doesn't. See Phase 2.
2. **The issue claims the "run the runner against canonical directly" idea is "the same
   split this repo already applies to the commitment-hash vector pack."** Not true:
   `tests/cmt_hash_vectors.rs`'s `vectors_dir()` has no env-var override and CI never runs
   that suite against the spec-repo checkout — only `check_dir` byte-diffs it. The actual
   existing precedent for "run the same suite against both the vendored copy and
   canonical" is `tests/conformance_loader.rs`'s `fixtures_dir()` /
   `MACP_CONFORMANCE_FIXTURES_DIR` pattern. Phase 3 models on that.

**The issue's Files table listing a `CHANGELOG.md` entry is dropped from scope.**
`CHANGELOG.md` history since release-plz adoption (`6172d55`, 2026-07-09) shows exactly
two non-bot-authored commits touching it — `c8ff39b` (`chore(release): 0.6.0 (#94)`,
2026-07-10, immediately after release-plz.yml itself landed) and `8c92ff4`
(`docs(changelog): record the 0.7.0 release (#122)`) — both release-record repairs, not
routine feature PRs; neither PR #171 nor #175 touched it themselves, and both entries
only appeared later in the release-plz bot's own single squash commit. In the current
era, a contributor PR adding a hand-written `## [Unreleased]` entry would be redundant at
best.

Everything else the issue asked for held up and is planned below, with exact mechanics
(file:line, function signatures, CI hooks) filled in from investigation plus the round-1
review's corrections.

## Phases

### Phase 1 — Expose the real predicates; introduce real version/default constants

**Status:** DONE

**Divergences from plan (noted by the Phase 1 verifier, PASS with caveats):**
- The `cargo semver-checks` acceptance criterion below could not actually be executed
  locally: `cargo-semver-checks` 0.45.0 (the version installed on this machine) crashes
  before evaluating any lints against rustc 1.96.1's rustdoc JSON (`error: unsupported
  rustdoc format v57 for file... (supported formats are v53, v55, v56)`) — the empty
  `grep` output this produces is indistinguishable from "no failures" but actually means
  "no check ran." This is a local-toolchain limitation, not a code issue — same precedent
  as PR #173 (documented there as "cargo-semver-checks is unrunnable on this machine...
  deferred to CI"). The verifier independently hand-inspected the API delta (all ten
  `"1.0"` replacements, both `#[doc(hidden)] pub fn` conversions, the four new
  constants) and found it purely additive/hidden — no removals, renames, signature
  changes, field additions, or visibility narrowings — so the change is very likely
  semver-clean, but this is **not a verified automated pass** and must not be
  represented as one in the eventual PR body. Deferred to CI, which runs a matched
  rustc/cargo-semver-checks pair.
- The test-count acceptance criterion below ("same total test count as baseline... must
  be identical") was wrong as originally written — it contradicts this same phase's Tests
  section, which requires 2 new unit tests. Corrected here: baseline was 871, Phase 1
  added exactly 2 (`macp_version_value` in `crates/macp-core/src/lib.rs`,
  `default_mode_and_configuration_version_values` in `crates/macp-core/src/session.rs`),
  total after Phase 1 is 873. `cargo test --workspace` passes at 873/873, 0 failed.
- Three benign wording-level deviations the verifier flagged as not worth changing:
  constant placement, the re-export path style in `common.rs`, and an off-by-one line
  citation (`src/server.rs:810` in this doc vs. `:814` in the actual current file —
  harmless drift from other code moving in the same region).
- The pre-existing `clippy::assertions_on_constants` failure at `crates/macp-core/src/
  session.rs` (now line ~1402, `assert!(CURRENT_SEMANTICS_REV >= 2)`) is confirmed
  pre-existing and unrelated to this phase (verified via `git stash` reproducing the
  identical failure on unmodified code) — out of scope, not fixed here.

**Delivers:** The two governance predicates the parity runner must exercise become
callable from outside `macp-modes` (marked as not a stability promise, matching the
issue's own framing). The protocol-version literal and the two `defaults`-section literals
that have a real production concept behind them each get one real, used constant instead
of scattered/absent string literals — so the future parity runner asserts against actual
runtime values, not reimplementations or bare duplicated literals.

**Depends on:** nothing.

**Files:**
- `crates/macp-modes/src/mode/util.rs` — `is_canonical_commitment_hash` (currently
  private `fn`, lines 80-90) → `#[doc(hidden)] pub fn`, with a doc comment stating it's
  exposed only for `tests/parity_contract.rs` and is explicitly not a stability promise.
  **`#[doc(hidden)]`, not plain `pub`** — see Approach for why this is the correct choice,
  not just the safer one. No module-tree change needed: `crates/macp-modes/src/lib.rs:11`
  already has `pub mod mode;` and `mode/mod.rs:8` already has `pub mod util;`; only the
  `fn` keyword blocks external visibility today. This crate has no `pub(crate)` tier
  anywhere (confirmed by a crate-wide grep) — `pub` is the correct next step, not an
  intermediate visibility to invent.
- `crates/macp-modes/src/mode/multi_round.rs` — `parse_contribute_value` (currently
  private `fn`, lines 57-72) → `#[doc(hidden)] pub fn`, same doc-comment treatment. Same
  story: `pub mod multi_round;` already at `mode/mod.rs:3`.
- `crates/macp-core/src/lib.rs` — add `pub const MACP_VERSION: &str = "1.0";` near the
  top (no `mod constants` exists in this crate; the established convention is hanging a
  constant off the file/module that owns the concept — nothing currently owns "protocol
  version," so the crate root is the right home for a genuinely crate-wide, wire-level
  concept).
- `crates/macp-core/src/session.rs` — add `#[doc(hidden)] pub const DEFAULT_MODE_VERSION:
  &str = "1.0.0";` and `#[doc(hidden)] pub const DEFAULT_CONFIGURATION_VERSION: &str =
  "config.default";` alongside the existing `CURRENT_SEMANTICS_REV` (`:86`) — specifically
  `SessionStart`-bound version fields (CLAUDE.md's "Session bootstrap" section), so
  `session.rs` is the concept owner, not `lib.rs`. `#[doc(hidden)]` here for a different
  reason than `MACP_VERSION`'s: nothing in the kernel enforces either of these two values
  against anything (unlike `MACP_VERSION`, which the wire-level gate genuinely checks) —
  they're conventional defaults being promoted purely so the parity runner and the example
  client share one source instead of two independent literals, not a claim about enforced
  runtime behavior worth a permanent public commitment.
- `src/bin/support/common.rs` — its existing `MODE_VERSION`/`CONFIG_VERSION`/
  `POLICY_VERSION` (:19-21) become re-exports instead of independent literals:
  `pub use macp_core::session::{DEFAULT_MODE_VERSION as MODE_VERSION,
  DEFAULT_CONFIGURATION_VERSION as CONFIG_VERSION};` and `pub use
  macp_policy::defaults::DEFAULT_POLICY_ID as POLICY_VERSION;` — **names preserved
  exactly** (`CONFIG_VERSION`, not `CONFIGURATION_VERSION` — round-2 review caught that
  the original draft of this fix renamed it, which would have broken the two existing call
  sites in this same file, `:44` and `:68`, that reference it by its current name). This
  removes all three pieces of duplication — including `POLICY_VERSION`, which duplicates
  `macp_policy::defaults::DEFAULT_POLICY_ID`, the same constant the parity runner already
  asserts against directly — giving the parity runner genuinely single-sourced values, not
  literals that merely happen to agree today.
- Ten production `"1.0"` call sites replaced with `macp_core::MACP_VERSION` (the complete
  production-code list, verified directly against each file's `#[cfg(test)]` boundary —
  ~50 additional sites are test-code fixture builders and are deliberately **not**
  touched, see Approach):
  - `src/server.rs:115` (`validate_envelope_shape`'s wire-level version gate)
  - `src/server.rs:803` (`Initialize` negotiation check)
  - `src/server.rs:810` (`Initialize` response's `selected_protocol_version`)
  - `src/runtime.rs:210` (`get_session_envelopes_after`'s empty-`macp_version` fallback —
    **not** a replay path; the actual replay-path fallbacks are the next two items)
  - `src/runtime.rs:298` (synthetic/internal entry construction)
  - `src/replay.rs:95` (`replay_entry`'s empty-`macp_version` fallback)
  - `src/replay.rs:329` (`replay_from_start`'s session-start replay path)
  - `crates/macp-modes/src/mode/handoff.rs:446` (synthetic implicit-accept envelope)
  - `src/bin/support/common.rs:84` (example-client envelope builder)
  - `src/bin/support/common.rs:100` (example-client `Initialize` call)

**Approach:**

*Why `#[doc(hidden)] pub fn`, not plain `pub fn`.* Both crates share one
`version_group` and `semver_check = true` (CLAUDE.md §8a); a plain `pub fn` added here
becomes a real, tracked, permanent public-API commitment of the *published* `macp-modes`
crate — un-exposing it later would be a major break across all seven crates, exactly the
kind of one-way-door cost CLAUDE.md §8a describes paying deliberately when a real
consumer needs it (`ApprovalRequestRecord::new`). The difference here is that the issue's
own framing explicitly asks for "not a stability promise" — `#[doc(hidden)]` is the
standard Rust idiom for exactly that, and `cargo-semver-checks` (the tool `semver_check =
true` runs) skips `doc(hidden)` items, so a future removal doesn't force a major bump the
way a plain `pub` would. This doesn't make the function *unreachable* by other external
code — Rust doesn't enforce `doc(hidden)` as a compiler boundary — so it's honest to call
this "reduces, not eliminates" the exposure, but it is still the more accurate choice
given the issue's own stated intent, and it is what `tests/parity_contract.rs` (a
consumer *of this crate itself*, via the root package's re-export) needs regardless of
which form is chosen.

*Why promote `MODE_VERSION`/`CONFIG_VERSION` too, but leave `policy_builder_schema_version`
alone (Phase 2).* Round-1 review flagged that Phase 1 promoted `MACP_VERSION` but left
`mode_version`/`configuration_version` as bare literals in the runner with no real backing
constant, calling that inconsistent — correct. Neither literal has a call site where the
*kernel* compares against it (unlike `MACP_VERSION`, which the wire-level gate at
`server.rs:115`/`:803` genuinely checks) — they're conventional default values only the
example client currently names — but promoting them into `macp-core` and having
`common.rs` re-export rather than duplicate them still closes the actual gap: one real,
single-sourced symbol instead of two independent literals that merely happen to agree.
`policy_builder_schema_version` is different in kind, not degree — see Phase 2's Approach
for why it gets a documentation-grade literal pin *plus* a real assertion against
`PolicyRegistry`'s admission check, rather than a promoted constant like the other two
(there is nothing in the kernel a `POLICY_BUILDER_SCHEMA_VERSION` constant could
meaningfully back — the value is a documented RFC recommendation, not a comparison the
runtime performs anywhere).

*Why the ~50 test-code `"1.0"` literals stay untouched.* Those tests hardcoding `"1.0"`
independently pin the wire format's literal value — if `MACP_VERSION` ever changed value
by mistake, those tests fail specifically *because* they don't reference the constant,
which is a second, independent guard the production-only replacement doesn't have on its
own. (Phase 1's own new unit test for `MACP_VERSION`, below, also covers part of this, but
doesn't fully substitute for the ~50 sites' independent-value pinning.) Rewriting them to
use the constant would remove that redundancy for no benefit.

**Edge cases & failure modes:**
- A future call site being added with a literal `"1.0"` instead of `MACP_VERSION` would
  silently reintroduce drift risk; no lint currently forbids this. Accepted as a known gap
  for a proof-of-concept scope (see Long-term posture), not fixed here.
- Both predicates are pure functions (`&str -> bool` and `&[u8] -> Result<String,
  MacpError>`, confirmed by reading both bodies in full) — no concurrency or state-machine
  edge case is introduced by widening their visibility.

**Acceptance criteria:**
- `cargo build --workspace` succeeds.
- Each of the ten listed production call sites, re-read directly (not grepped — a
  file-level grep cannot distinguish a `#[cfg(test)]` block from production code within
  the same file, since several of these files mix both), now reads `macp_core::
  MACP_VERSION` instead of the literal `"1.0"`.
- `RUSTC_WRAPPER="" cargo semver-checks check-release --workspace --baseline-version 0.8.0
  2>&1 | grep -E '^--- failure |^  (field|struct) '` (CLAUDE.md §8a's exact invocation —
  read the failure list, never the exit code) reports no output — this catches an
  accidental breaking change elsewhere in the same diff, which is its real job here; it
  does **not** verify the `#[doc(hidden)]` decision itself (`cargo-semver-checks` doesn't
  distinguish hidden from non-hidden additions in its output, so no tool-based criterion
  can stand in for that — see the next bullet instead). `0.8.0` is confirmed already
  published to crates.io (this session verified all seven crates live at `0.8.0` after PR
  #172). **Actually unverifiable locally** — see the divergence note above; deferred to
  CI, confirmed manually-additive by the Phase 1 verifier in the meantime.
- Each of the two functions and the two new `session.rs` constants carries `#[doc(hidden)]`
  directly above its `pub` — verified by reading the diff, not by a tool (no existing tool
  in this repo's CI distinguishes hidden vs. non-hidden public items either way).
- `cargo test --workspace` passes at baseline + 2 (871 → 873) — the 2 new unit tests
  listed in the Tests section below, and no other change; any other delta means a test
  was accidentally dropped or duplicated. (Originally written as "must be identical to
  baseline" — corrected here; that contradicted this phase's own Tests section. See the
  divergence note above.)

**Tests:**
- A new unit test in `crates/macp-core` asserting `MACP_VERSION == "1.0"`.
- New unit tests in `crates/macp-core/src/session.rs` asserting `DEFAULT_MODE_VERSION ==
  "1.0.0"` and `DEFAULT_CONFIGURATION_VERSION == "config.default"`.
- No new tests for the two visibility changes themselves (behavior is unchanged; existing
  unit tests in `util.rs`/`multi_round.rs` continue to cover them) — Phase 2 is what
  proves the new surface is usable from outside the crate.
- `src/bin/support/common.rs`'s existing usage sites continue to compile and behave
  identically under the re-export — no behavior change, confirmed by the existing example
  client tests (if any) or a manual `cargo build --bins` check.

**Docs:** None — no repo doc currently describes the visibility of these two functions or
enumerates `"1.0"` call sites.

---

### Phase 2 — Vendor the manifest; write the parity-contract runner

**Status:** DONE

**Divergences from plan (noted by the Phase 2 verifier, PASS with one note):**
- The runner ended up with 17 `#[test]` functions rather than the roughly
  "one per `HANDLED` entry plus two structural guards" (≈10) the plan
  described — `defaults` split into three tests (mode/configuration/policy
  version match, the documentation-grade `policy_builder_schema_version`
  literal pin, and its separate real registry-acceptance assertion) and
  `contribute_payload` split into two (the decode/encode round trip, and the
  `first_byte` marker check), plus three tests for the hand-rolled hex
  helpers (a self-test and the two required-rejection cases). This is finer
  granularity than the plan's approximate count, not a different design —
  every section the plan named is still asserted against real code exactly
  as specified.
- `tests/parity/SOURCE.md` and `docs/testing.md`'s new subsection both
  describe Phase 3's CI wiring (the `check_dir` gate, `MACP_PARITY_CONTRACT`
  set in CI) as already active. This is true only once Phase 3 lands — by
  design, per this plan's single-PR strategy (all three phases ship as one
  PR with three phase-commits) — but a reviewer diffing only this phase's
  commit in isolation would find that stated invariant momentarily false
  against `ci.yml`'s still-unbumped `SPEC_REV`. Noted here so it isn't
  mistaken for a defect; the PR body should say so too.

**Delivers:** `tests/parity/contract.json` (byte-identical vendor of the spec repo's
manifest at the commit Phase 3 pins `SPEC_REV` to), `tests/parity/SOURCE.md`
(provenance, modeled directly on the real precedent at
`macp-sdk-python/tests/vectors/cmt-hash/SOURCE.md`, which `schemas/parity/README.md`
explicitly names as the pattern to match), and `tests/parity_contract.rs` — a runner
asserting every macp-runtime-relevant section against this runtime's real code.

**Depends on:** Phase 1 (`MACP_VERSION`, `DEFAULT_MODE_VERSION`,
`DEFAULT_CONFIGURATION_VERSION`, the two now-`#[doc(hidden)] pub` predicates).

**Files:**
- `tests/parity/contract.json` (new) — exact byte copy of
  `../multiagentcoordinationprotocol/schemas/parity/contract.json` at the pinned commit.
  Never hand-edited (enforced by Phase 3's `check_dir`).
- `tests/parity/SOURCE.md` (new) — modeled on
  `macp-sdk-python/tests/vectors/cmt-hash/SOURCE.md`'s exact shape (read in full this
  session): a `# Source` section naming the exact spec-repo path
  (`schemas/parity/contract.json`), the pinned commit hash (kept equal to Phase 3's
  `SPEC_REV`), and the import date; a short section explaining how this directory is
  gated (Phase 3's `check_dir "tests/parity" "spec-repo/schemas/parity"` call, run on
  every PR) and the one-line re-vendor command. Unlike the SDK precedent, there's no
  "why does this live outside the normal fixture dir" story to tell — `tests/parity/`
  is already its own tree — so that section is omitted rather than manufactured.
- `tests/parity_contract.rs` (new) — the runner, one `#[test]` function per section this
  repo is named in plus two structural guards, modeled on `tests/cmt_hash_vectors.rs`'s
  shape (module doc, no macro, no build.rs codegen) but borrowing
  `tests/conformance_loader.rs`'s `fixtures_dir()`-with-env-override idiom for the
  manifest locator — adapted for a single **file**, not a directory:
  `fn parity_contract_path() -> PathBuf` reads `MACP_PARITY_CONTRACT` (non-empty) and uses
  it directly as the file path, else defaults to `tests/parity/contract.json`. This exact
  env var name and file-path shape is load-bearing, not incidental: Phase 3's
  canonical-run CI step sets `MACP_PARITY_CONTRACT` to the spec-repo checkout's
  `contract.json` path directly (a file, unlike `fixtures_dir()`'s directory), so the two
  phases must agree on both the name and the shape — flagged here because round-2 review
  found the original draft never stated either explicitly, leaving this seam
  underspecified between phases.

**Approach:**

*Deserialization shape — corrected against the actual canonical schema.* Round-1 review
caught that the original design (`#[serde(deny_unknown_fields)]` on a `Contract` struct
with a required `$comment: String` field) directly conflicts with
`schemas/json/macp-parity-contract.schema.json`, read in full this session: every object
in the manifest — root, the `sections` map, and *every individual section* — carries
`"additionalProperties": false` paired with `"patternProperties": {"^(_|\\$comment)":
{}}`, and root's `required` is only `["contract_version", "sections"]` ($comment is
optional). Serde's `deny_unknown_fields` attribute can't express "unknown key rejected
*unless* it matches this pattern," so the runner does the check manually instead: parse
the whole file first as `serde_json::Value`, then a small `fn check_no_unexpected_keys
(obj: &Map<String, Value>, allowed: &[&str], context: &str)` helper walks an object's keys
and fails loudly (naming both the object and the offending key) on anything not in
`allowed` and not starting with `_` or with the literal string `$comment` — a plain
`str::starts_with` check, **not a regex**: the upstream pattern `^(_|\$comment)` is
exactly this prefix rule and nothing more, and `regex` is confirmed only a *transitive*
dependency of this workspace today (not a direct one) — adding it directly would trigger
the same `integration_tests/Cargo.lock` two-lockfile regeneration cost this plan already
avoids for the hex helpers below, for no benefit, since a prefix check needs no regex
engine.

Applied at two levels, each with its own allowlist and **not to be conflated** (round-2
review caught the original draft of this fix doing exactly that, which would have made
the coverage guard's own sections-level check fail on the real, byte-identical, correctly-
formed manifest — see the immediate next paragraph):
- **Root** — `allowed = ["contract_version", "sections"]`.
- **`sections` map** — `allowed = ALL_SECTIONS`, a full nine-entry constant (below) listing
  every section name the canonical schema currently defines, **not** the narrower
  seven-entry `HANDLED` constant used by the coverage guard. The manifest legitimately
  contains `retry` and `projection_anomaly` (SDK-only sections) alongside the seven
  macp-runtime-relevant ones — checking the sections map's keys against `HANDLED` instead
  of `ALL_SECTIONS` would reject those two as "unexpected" on the very first run against
  the real, correctly-vendored file. `ALL_SECTIONS` answers "is this a key the canonical
  schema actually defines"; `HANDLED` separately answers "does macp-runtime assert every
  section naming it" — two different, both-necessary checks operating on the same map, not
  one check reused for two purposes.

`contract_version` deserializes as a plain `String`; `sections` deserializes as
`HashMap<String, Value>` (not a fixed struct) — per-section fields are still pulled out ad
hoc, loosely, per assertion (this was always the original design's intent at the
per-section level; the fix above is scoped to the root and sections-map levels only).

Note for context, not new work: the canonical schema's `sections`-level
`additionalProperties: false` combined with `required` listing exactly the current nine
section names means a genuinely *new* section name can't appear without the upstream
schema changing too — so the coverage guard's real, live job is catching an *existing*
section (`retry`, `projection_anomaly`) gaining `"macp-runtime"` in its `applies_to`, which
`schemas/parity/README.md`'s own Versioning section names as an explicit MINOR-bump
scenario expected to redden a consumer's CI "until it actually wires the assertion — that
is the mechanism working as designed." The `HashMap<String, Value>` shape (rather than a
schema-mirroring fixed struct) is still the right defensive choice — it doesn't assume the
peer schema never changes shape — but the practical drift scenario it guards is this one,
not an unbounded new-section case.

*Coverage guard, both directions* (per issue's explicit ask): `const HANDLED: &[&str] =
&["protocol", "modes", "defaults", "error_codes", "commitment_hash", "contribute_payload",
"contribute_acceptance"];` — seven entries, `defaults` **included** per the Context
correction above; `retry`/`projection_anomaly` genuinely absent (confirmed by reading the
manifest: neither names `"macp-runtime"` in `applies_to`). One test asserts every section
whose `applies_to` contains `"macp-runtime"` has a matching `HANDLED` entry, by name, on
failure. A second asserts the converse — every `HANDLED` name still exists as a
`sections` key (catches a renamed/removed section silently retiring a test). A separate
`const ALL_SECTIONS: &[&str]` (nine entries — `HANDLED`'s seven plus `"retry"` and
`"projection_anomaly"`) exists solely for the sections-map key-allowlist check above; it
is not used by either coverage-guard test.

*Per-section assertions:*
- **`protocol`** — `sections["protocol"]["macp_version"] == macp_core::MACP_VERSION`.
- **`modes`** — `sections["modes"]["standard"]` (order-insensitive compare — a `HashSet`
  or sorted-`Vec`) equals `macp_runtime::mode::STANDARD_MODE_NAMES`
  (`crates/macp-modes/src/mode/mod.rs:16-22`, already the five standard mode IDs); same
  for `"extension"` vs `EXTENSION_MODE_NAMES` (`mode/mod.rs:25`).
- **`defaults`** — four fields, four different treatments, stated explicitly rather than
  uniformly:
  - `policy_version` — assert equals `macp_policy::defaults::DEFAULT_POLICY_ID`
    (`crates/macp-policy/src/defaults.rs:23`, already `"policy.default"`, already a real
    `pub const`, reachable from `tests/*.rs` as a normal root dependency — confirmed, no
    `Cargo.toml` change needed).
  - `mode_version` — assert equals `macp_core::session::DEFAULT_MODE_VERSION` (Phase 1).
  - `configuration_version` — assert equals
    `macp_core::session::DEFAULT_CONFIGURATION_VERSION` (Phase 1).
  - `policy_builder_schema_version` — **two** assertions, not one. Round-2 review pushed
    back, fairly, on an earlier revision of this plan that kept this as a single literal
    comparison: comparing the manifest's own value to a hardcoded `3` in the test has zero
    detection power beyond what Phase 3's `check_dir` byte-diff already provides — a
    citation comment documents the value, it doesn't add rigor. Closing that for real:
    1. The literal assertion against `3` is kept, as documentation-grade pinning, cited to
       exactly what the manifest's own `source` field cites: "RFC-MACP-0012 Section 3's
       SHOULD-recommendation that new policies declare schema_version 3."
    2. **The assertion with actual detection power:** `macp_policy::registry
       ::PolicyRegistry`'s schema-version admission check (located this session at
       `crates/macp-policy/src/registry.rs:301`, currently `schema_version > 0`) accepts a
       minimally-shaped `PolicyDefinition` whose `schema_version` equals the manifest's
       value. This expresses the recommendation's actual claim — "a newly authored policy
       declaring this schema_version is valid here" — against real registry behavior,
       not just literal-to-literal equality. The exact API needed to construct and
       validate a minimal `PolicyDefinition` (whether `registry.rs:301` is reached via a
       public `validate_definition`-style method or only internally) is confirmed against
       the real current signature at implementation time — round-2 review located the
       enforcement line but this planning round did not independently re-read the full
       function, so the call shape here is a starting point, not assumed final.

    **Why this section is legitimately handled, not a spec-repo defect needing escalation**
    (this is the part that survives from the earlier revision): `schemas/parity/README.md`'s
    "Open items" section, read in full, lists **five** known gaps — not four, corrected
    from an earlier miscount this round — none of which is `policy_builder_schema_version`
    (the five are: Contribute-payload non-canonical-input disagreement, the `macp_version`
    literal having no RFC home, `contribute_acceptance` being runtime-only,
    `projection_anomaly.kind`'s type-width difference, and `field_case_rule` being new
    prescriptive text with no RFC/registry home — the last of which doesn't bear on this
    field either). The spec-repo authors already considered this value's sourcing and did
    not flag it as unsourced or inapplicable to `macp-runtime`. This runtime's built-in
    policies are grandfathered at `schema_version: 1`
    (`crates/macp-policy/src/defaults.rs:18,60,85,108`, pinned by
    `default_policy_schema_version_is_one`), which doesn't contradict the manifest's value
    of `3`, since the RFC's recommendation is forward-looking for *newly authored*
    policies, not a claim about the built-ins — assertion 2 above is what actually proves
    a newly-declared `schema_version: 3` policy is accepted today.
- **`error_codes`** — derive the runtime's produced set by calling the real
  `MacpError::error_code()` (`crates/macp-core/src/error.rs:55`) over a hand-enumerated
  list of one constructed value per variant (mirroring, via a code comment
  cross-reference, `error.rs`'s own `error_code_mapping_covers_all_variants` test at
  `:85-130` — neither list is compiler-enforced exhaustive since `MacpError` is
  `#[non_exhaustive]`, so the comment gives a reviewer adding a variant both places to
  update). Assert the resulting `HashSet<&str>` equals
  `sections["error_codes"]["permanent"]` (16 entries, confirmed), and assert
  `"UNAUTHORIZED"` (the manifest's one `deprecated` code) is not a member (confirmed true
  today — 20 variants produce exactly 16 distinct codes).
- **`commitment_hash`** — for each of the 12 vectors (1 `accept`, 11 `reject`), call the
  now-visible `macp_modes::mode::util::is_canonical_commitment_hash` directly and assert
  `true`/`false` per the vector's list membership.
- **`contribute_payload`** — for each of the 4 vectors, hex-decode `protobuf_hex` (using a
  small hand-rolled `fn hex_decode(s: &str) -> Vec<u8>` local to this test file — **no new
  crate dependency**; confirmed this session that no hex crate exists anywhere in
  `Cargo.lock`, and hand-rolling ~10 lines avoids both the dependency-review question and
  any risk of needing to regenerate `integration_tests/Cargo.lock` per CLAUDE.md's
  two-lockfile rule) and call the now-visible
  `macp_modes::mode::multi_round::parse_contribute_value`, asserting the result equals
  `value`; for the 3 vectors carrying `legacy_json_hex`, decode and assert the same. For
  the 3 non-`decode_only` vectors (`ascii_short`, `utf8_accent`,
  `two_byte_varint_boundary`), additionally *encode* — build a real
  `macp_pb::multi_round_pb::ContributePayload { value }`, encode via `prost::Message::
  encode`, and hex-encode the result (same hand-rolled helper, `fn hex_encode(bytes:
  &[u8]) -> String`) to assert a byte-exact match against `protobuf_hex` — a true
  round-trip through the real encode and the real decode. The `two_byte_varint_boundary`
  vector is 130 bytes (two-byte protobuf length prefix) versus `one_byte_varint_boundary`
  (the `decode_only` vector) at 127 bytes (one-byte prefix) — both boundary-adjacent
  payload sizes, correcting the earlier "127/128" mischaracterization; the round-trip
  assertion is what actually proves this runtime's `prost`-generated encode handles the
  boundary correctly, so this vector's handling must not be simplified away.

  Both hex helpers get a 3-line self-test against a known vector
  (`hex_encode(&[0x0a, 0xff]) == "0aff"`, `hex_decode("0aff") == vec![0x0a, 0xff]`), and
  `hex_decode` **rejects** (panics on) odd-length input and any non-`[0-9a-f]` character
  rather than silently skipping or truncating it. This is load-bearing, not incidental:
  round-2 review's own risk analysis found the round-trip design is self-checking against
  most bugs (a wrong decode almost always fails to parse as a valid `ContributePayload` at
  all; a wrong encode fails the string comparison against `protobuf_hex` directly) — the
  one silent-failure mode is specifically a *lenient* decoder that tolerates malformed hex,
  so strictness there is the one property worth pinning explicitly rather than leaving
  implicit in "hand-rolled, ~10 lines."

  Separately
  assert `first_byte.protobuf` (`0x0a`) and `first_byte.legacy_json` (`0x7b`) against the
  first byte of each vector's own hex strings.
- **`contribute_acceptance`** — call `parse_contribute_value(&[])` and assert `Err(_)`.
- **Version-floor guard** — `contract_version.starts_with("1.")`.

*Explicitly out of scope, stated in the runner's module doc so a future reader doesn't
wonder why it's missing:* validating `contract.json`'s own *shape* against
`schemas/json/macp-parity-contract.schema.json` (that's `scripts/check-parity-contract.py`
in the spec repo, plus the negative fixtures under `schemas/json/tests/invalid-parity-
contract/`) — this runner's job is asserting the manifest's *content* against
macp-runtime's real behavior, not re-validating the manifest's JSON shape, which is
upstream's job and already covered by that script and those fixtures.

**Edge cases & failure modes:**
- A future manifest revision adding a *new vector* to an existing, handled section is
  caught automatically — every assertion iterates the parsed JSON's actual vector/list
  directly, not a hardcoded count (contrast with `tests/cmt_hash_vectors.rs`'s
  `assert_eq!(vectors.len(), 5, ...)`, which guards a *file-count* across multiple vendored
  files — a different kind of drift than this single-file manifest has, so that specific
  guard doesn't apply here; the file-existence/content guard is Phase 3's `check_dir`).
- Hex-decoding a malformed vector string (shouldn't happen from a byte-identical vendored
  copy, but defensively) panics with a message naming the vector, not a bare `unwrap()`.
- `two_byte_varint_boundary`/`one_byte_varint_boundary` specifically exercise the protobuf
  varint-length-prefix boundary — don't simplify this vector's handling away (see above).

**Acceptance criteria:**
- `cargo test --test parity_contract` passes; every section assertion and both structural
  guards are individually named `#[test]` functions and appear individually in `cargo
  test`'s output (one test per `HANDLED` entry, plus the two coverage-guard tests, plus
  the version-floor test — count this precisely once written, Phase 3 needs the exact
  number).
- Prove, then restore (record each in the PR body):
  - Comment out one entry from `HANDLED` → the coverage guard fails, naming that section.
  - Corrupt one vector's expected value in a **local scratch copy** (not the committed
    file) → the corresponding specific assertion fails, not a generic panic.
- `cargo clippy --workspace --all-targets` and `cargo fmt --check` pass on the new file.

**Tests:** The runner itself, per the section list above — this phase's "delivers" *is*
its test surface.

**Docs:** `docs/testing.md` — a short new subsection under `## Unit tests and
conformance` naming `tests/parity/` and `tests/parity_contract.rs`, pointing at
`tests/parity/SOURCE.md` for re-vendoring — this repo's testing doc has no mention of this
tree today.

---

### Phase 3 — CI wiring: bump `SPEC_REV`, byte-diff the vendored copy, watch it for drift

**Status:** DONE

**Divergences from plan:** none of substance. The verifier flagged one cosmetic
note: `ci.yml:24-31`'s top-of-file comment describing what "alignment work"
belongs in the same PR as a `SPEC_REV` bump wasn't updated to name parity
re-vendoring explicitly — the detailed instruction correctly lives in
`spec-drift.yml`'s "What to do" checklist instead (the authoritative copy per
this phase's own file scoping; that comment was never in Phase 3's `Files`
list). Not a gap against the plan.

**Delivers:** The vendored `tests/parity/contract.json` can never silently diverge from
its spec-repo source, the daily drift watcher learns about the new tree (including its
remediation instructions, not just its escalation trigger), and the parity runner also
runs directly against the spec-repo checkout in CI, proving the runtime matches canonical
— not merely that the vendored copy matches canonical.

**Depends on:** Phase 2 (the vendored file and runner must exist first).

**Files:**
- `.github/workflows/ci.yml`:
  - `SPEC_REV` (line 39) bumped from `0de1fab20bc396fdc5f1412e61fdc3d7baf0a64d` to
    `4f15b96cac6e39d62925a5baa1ef80a42c2f818d` (current spec `main` HEAD as of this
    plan) — **re-check at implementation time that spec `main` hasn't moved further**,
    and if it has, re-verify the same way this session did: confirm nothing under
    `schemas/conformance/` or `schemas/json/policy/` changed in the extended range before
    adopting a later pin (this session confirmed the `0de1fab2..4f15b96c` range only adds
    `schemas/conformance/README.md` — outside `check_dir`'s `*.json` glob — and files
    under `schemas/json/` but not `schemas/json/policy/`, so `enum_lists_match_the_
    canonical_schemas` is unaffected; re-derive the equivalent evidence for whatever exact
    commit is actually pinned).
  - A third `check_dir` call: `check_dir "tests/parity" "spec-repo/schemas/parity"`, added
    at the existing pair (`ci.yml:624-625`). Confirmed this session: `check_dir` globs
    `*.json` directly under each directory in both directions, correctly ignoring
    `README.md` (canonical side) and `SOURCE.md` (vendored side) — a single-`.json`-file
    directory works identically to a multi-file one, no adaptation of the shared function
    needed.
  - A new step (modeled on "Run conformance suite against canonical fixtures",
    `ci.yml:645-647`) setting `MACP_PARITY_CONTRACT: ${{ github.workspace
    }}/spec-repo/schemas/parity/contract.json` and running `cargo test --test
    parity_contract` against the already-checked-out spec-repo tree (`ci.yml:580-585`, no
    new checkout needed). **Include a collection guard**, mirroring `ci.yml:670-673`'s
    `grep -q '^test result: ok\. 1 passed'` pattern but for a multi-test file: capture the
    step's output and assert the reported passed-count is at least the number of test
    functions `tests/parity_contract.rs` actually defines (fill in the exact number once
    Phase 2 lands — do not skip this guard; the issue's own acceptance criteria explicitly
    call for "confirm in CI output, don't assume collection," and `cargo test` exits 0 on
    a silently-renamed-or-`#[ignore]`d test just as easily as on a passing one).
- `.github/workflows/spec-drift.yml`:
  - `schemas/parity` added to the watched-tree loop (`spec-drift.yml:128`):
    `for tree in schemas/conformance schemas/json/policy schemas/parity; do`.
  - The header comment (`spec-drift.yml:16-19`) and both hardcoded tree-name mentions in
    the closing-issue comment text (`spec-drift.yml:110`, `:323`) updated to include
    `schemas/parity` — these render into the auto-filed/auto-closed GitHub issue, so
    leaving them saying "two trees" would be a real, user-visible inaccuracy.
  - **The "commits since the pin" enumeration** (`spec-drift.yml:347`, `git -C spec-head
    log --oneline "$PINNED..$head_sha" -- schemas/conformance schemas/json/policy`) gets
    `schemas/parity` added to its path list too. Missed in the original draft of this
    plan: without this, a parity-only spec commit would still correctly escalate via the
    `:128` loop, but the filed issue's "Commits since the pin" section would show an empty
    list for a change that clearly exists — a real, user-visible inaccuracy on the issue
    the watcher actually files, not a cosmetic gap.
  - **The "### What to do" remediation checklist** (`spec-drift.yml:352-358`, today:
    item 1 vendor `schemas/conformance/` fixtures, item 2 re-check policy mirrors, item 3
    bump `SPEC_REV`) gets a new item **3** covering re-vendoring `tests/parity/
    contract.json` and re-running the prove/restore checks, with the existing "bump
    `SPEC_REV`" item renumbered to **4** so it stays last. **The back-reference at `:358`
    must change too** — it currently reads "Bumping `SPEC_REV` without doing 1 and 2 turns
    `conformance-oracle` red on every PR," and needs to become "without doing 1, 2, and 3."
    Round-2 review caught that an earlier draft of this same fix added the new checklist
    item without updating this back-reference, which would have shipped exactly the kind
    of incomplete remediation text this bullet exists to prevent. This is the literal text
    a contributor reads on the filed issue, so getting the numbering right matters as much
    as adding the item.
  - The case-statement classification (`spec-drift.yml:210-245`) is **not** given a new
    `schemas/parity/*.json)` branch — left to the catch-all `*)` at `:242-244`, which
    reports every `schemas/parity/*.json` diff as unconditionally `actionable`. This is
    the fail-closed, conservative default, and correct for a first pass with no real
    annotation-only-edit example yet to design a suppression rule against (same as the
    `-x 'README.md'` exclusion at `:137` already applying uniformly, so
    `schemas/parity/README.md` changes are already invisible to the watcher exactly like
    the other two trees' READMEs — not a gap, just noted for precision). If a real
    annotation-only parity-manifest edit later creates false-positive noise, that's the
    trigger to add a branch then.

**Approach:** `SPEC_REV`'s bump and the `check_dir` call are one indivisible unit of work
within this phase — bumping the pin before the vendored file exists makes Phase 2
unverifiable against real canonical content; adding the `check_dir` call before the pin
moves fails immediately with `MISSING: canonical directory ... does not exist` (the
literal first branch of `check_dir`'s body). `ci.yml:30-31` states the actual
constraint precisely: the bump and the alignment work it requires must land in the **same
PR** (not, as an earlier draft of this plan paraphrased it, "the same commit" — squash-merge
means the distinction doesn't matter in practice, but the file's own wording is the one to
cite).

**Edge cases & failure modes:**
- If spec `main` has moved again by implementation time in a way that touches
  `schemas/conformance/` or `schemas/json/policy/`, bumping `SPEC_REV` would turn the
  *existing* conformance-oracle checks red too, not just the new parity one — re-verify
  zero drift on both existing `check_dir` calls at whatever exact commit gets pinned.
- The new "run against canonical" step is a second, slower `cargo test --test
  parity_contract` invocation in the same job — acceptable (the conformance suite already
  runs twice per PR under the same precedent) but worth noting for job-duration awareness.

**Acceptance criteria:**
- Prove, then restore (record in the PR body):
  - Delete `tests/parity/contract.json` on a scratch branch → `check_dir` fails with a
    `MISSING` line naming it.
  - Flip one byte in the vendored copy on a scratch branch → `check_dir` fails with a
    `DRIFT` line.
- At the *old* pin (`0de1fab2`), `schemas/parity/` legitimately does not exist in that
  spec-repo checkout (confirmed this session — no history before `4f15b96c`); re-confirm
  at implementation time if the exact pin target changed.
- `spec-drift.yml`'s live file, read after the change (not the diff), lists
  `schemas/parity` in the watched-tree loop, in the commits-since-pin path list, and in
  the "What to do" checklist.
- The full `conformance-oracle` job (and every other required check) is green on the PR.
- The new "run against canonical" step's collection guard actually fails when a test
  function is renamed on a scratch branch (prove this once, alongside the other
  prove/restore checks, then restore).

**Tests:** No new Rust tests — CI configuration. The "tests" are the prove/restore
CI-behavior demonstrations above, run on a scratch branch and recorded, matching how PR
#173 proved `spec-drift.yml`'s #169/#170 hardening.

**Docs:** None beyond Phase 2's `docs/testing.md` addition.

## Long-term posture

- **Not a one-way door for this repo's own contract**, but not entirely free either: the
  two predicates becoming `#[doc(hidden)] pub` is a genuine, intentional addition to
  `macp-modes`'s externally-callable surface, consistent with CLAUDE.md §8a's own
  precedent of exposing exactly what a real consumer needs (`ApprovalRequestRecord::new`).
  `#[doc(hidden)]` keeps `cargo-semver-checks` from treating a future removal as a major
  break, matching the issue's "not a stability promise" framing — but it doesn't make the
  functions uncallable by other external code that finds them anyway, so "reversible" is
  the honest word, not "free."
- **No deliberate debt remains in the `defaults` section** after this revision —
  `policy_version`/`mode_version`/`configuration_version` all assert against real named
  constants, and `policy_builder_schema_version` gets both a documentation-grade literal
  pin and a real behavioral assertion against `PolicyRegistry`'s admission check (see
  Phase 2's Approach) — not a gap to close later.
- **The `spec-drift.yml` catch-all being fail-closed for `schemas/parity`** (every diff
  escalates, nothing auto-suppresses) is the safe default for a first pass at a brand-new
  watched tree with no real-world false-positive example yet. The fix, if ever needed, is
  one small `case` branch.

## Enterprise concerns

- **Reliability of the oracle itself:** the prove/restore acceptance criteria across Phase
  2 and Phase 3, plus Phase 3's new test-collection guard, are what actually establish this
  mechanism *works* as a safety net — not optional documentation, the only evidence this
  oracle would catch real drift.
- **Observability:** no new runtime telemetry — this feature is CI/test-time only.
- **No migration/rollback story needed** — nothing touches persisted state, the wire
  protocol, or a deployed artifact.

## Open questions

All resolved as consequential-but-decidable calls or corrected via direct evidence, none
escalated:

- **`policy_builder_schema_version`'s scope** — round-1 review initially suggested this
  might be a spec-repo defect needing a cross-repo issue (macp-runtime has no "policy
  builder" concept). Resolved by reading `schemas/parity/README.md`'s "Open items" section
  directly: it enumerates **five** known manifest gaps and does not include this one,
  meaning the spec-repo authors already considered and intentionally sourced this value
  (RFC-MACP-0012 §3's SHOULD-recommendation) as applicable to `macp-runtime`. No
  cross-repo issue needed. Round-2 review separately pushed back on the *rigor* of a
  literal-only assertion (not the scope question) — closed by adding a second, real
  behavioral assertion; see Phase 2's `defaults` bullet.
- **Exact `SPEC_REV` target commit** — `4f15b96c` (spec HEAD as of this planning round),
  with an explicit instruction to re-verify spec HEAD hasn't moved again before Phase 3
  executes.
- **Whether to add a `spec-drift.yml` suppression branch for `schemas/parity`** — no,
  fail-closed catch-all is correct for a first pass. See Long-term posture.

Nothing here rises to the "critical" tier (no public contract, schema shape, auth model,
irreversible migration, external dependency, or cross-repo write) — this entire feature is
internal test/CI tooling, and the one item that looked critical on first pass
(`policy_builder_schema_version`) resolved cleanly on closer reading rather than requiring
Fable consultation or a cross-repo write.

**PR strategy:** all three phases ship as **one PR**, three phase-commits. They're tightly
coupled — Phase 2 needs Phase 1's visible surface to compile; Phase 3's `check_dir`/
`SPEC_REV` bump needs Phase 2's vendored file to exist to be meaningfully green — with no
natural mid-feature shippable seam. Matches how PR #173 bundled the related #169/#170
spec-drift work into one PR.

## Repo map

See `plans/parity-contract-176-PROGRESS.md`.

## Plan review

**Round 1:** `REVISE`. A fresh Opus agent checked every file:line citation against live
code and the raw manifest, and separately read three companion spec-repo artifacts this
plan's first draft hadn't consulted (`schemas/parity/README.md`,
`schemas/json/macp-parity-contract.schema.json`, and the SDK's existing `SOURCE.md`
precedent). Findings and how each was closed:

1. The `CHANGELOG.md`-is-bot-only claim was overstated (one hand-authored repair commit,
   `8c92ff4`, exists) — narrowed to the accurate claim; conclusion (drop from scope)
   unchanged. (Round 2 found this fix was itself off by one — see below.)
2. "Not a one-way door" was wrong for the two predicates' visibility change — corrected to
   `#[doc(hidden)] pub fn` plus an honest "reversible, not free" framing.
3. `spec-drift.yml`'s edit list was missing two functional pieces (the commits-since-pin
   path list, the remediation checklist) — added.
4. The `defaults` section's `mode_version`/`configuration_version`/
   `policy_builder_schema_version` assertions were under-designed — the first two now get
   real promoted constants (Phase 1); the third was investigated further (not just
   patched) and found to be a correct, intentional literal assertion once
   `schemas/parity/README.md`'s "Open items" section was actually read — the suggested
   cross-repo escalation turned out to be unnecessary.
5. Two implementation blockers were surfaced and closed: no hex codec exists in the
   workspace (resolved by hand-rolling ~10 lines locally rather than adding a dependency)
   and the CI "run against canonical" step needed a test-collection guard (added).
6. `deny_unknown_fields` was found to directly conflict with the canonical schema's
   `patternProperties` escape hatch for `_`/`$comment` keys — replaced with a manual,
   pattern-aware key check.
7. Several smaller citation errors were corrected in place (the `two_byte_varint_boundary`
   vector's actual byte count, `src/runtime.rs:210`'s actual function context, the
   "same commit" vs. "same PR" wording in `ci.yml`'s comment).
8. Two acceptance criteria were unfalsifiable as written (a grep that can't distinguish
   test blocks from production code; a self-contradicting parenthetical) — both rewritten
   to be checkable by a fresh reviewer without re-deriving intent.

**Round 2:** `REVISE`. A second fresh Opus agent checked this revision specifically
against the round-1 gap list (not cold) and confirmed every round-1 *citation* fix (byte
counts, function labels, exact wording, error-code set, `spec-drift.yml` line numbers) is
now accurate against live code. It found the round-1 *design* fixes introduced their own
problems, three of them hard blockers that would have broken the build or turned the new
test red on first run:

1. The sections-map `deny_unknown_fields` replacement checked keys against `HANDLED`
   (7 entries) instead of the full 9 real section names — would have rejected the
   legitimate `retry`/`projection_anomaly` keys on the byte-identical vendored file.
   **Fixed**: added a separate `ALL_SECTIONS` (9 entries) for that check; `HANDLED` stays
   scoped to the coverage guard alone.
2. The `common.rs` re-export renamed `CONFIG_VERSION` to `CONFIGURATION_VERSION`, breaking
   two existing call sites in that file. **Fixed**: re-export preserves the exact existing
   name.
3. The `^(_|\$comment)` key check was described in a way that implied `regex` — not a
   direct dependency of this workspace, and adding one would trigger the exact
   `integration_tests/Cargo.lock` cost round 1's hex fix specifically avoided. **Fixed**:
   restated as a plain `str::starts_with` prefix check, which is all the upstream pattern
   actually requires.
4. `policy_builder_schema_version`'s round-1 fix (a cited literal) was correctly scoped
   (no cross-repo issue needed) but round 2 judged the literal-only assertion still had
   "zero detection power beyond `check_dir`," a fair restatement of the original critique.
   **Fixed**: added a second, real assertion against `PolicyRegistry`'s schema-version
   admission check (`registry.rs:301`), keeping the literal as documentation only.
5. Round 1's own CHANGELOG fix was off by one (two hand-authored commits since release-plz
   adoption, not one: `c8ff39b` and `8c92ff4`), and `PROGRESS.md` still carried the
   original, fully-false claim verbatim, unsynced with the plan. **Fixed**: both corrected.
6. The `spec-drift.yml` checklist fix added a new remediation item without updating the
   back-reference naming which items are load-bearing (`"without doing 1 and 2"`).
   **Fixed**: explicit renumbering plus the back-reference text.
7. Two rigor gaps: `schemas/parity/README.md`'s "Open items" section has five bullets, not
   four as round 1 counted (**fixed**, corrected in three places); the hex hand-rolling
   had no self-test or explicit malformed-input behavior stated (**fixed**, added a 3-line
   self-test plus an explicit reject-not-tolerate requirement).
8. Two consistency gaps: the two `DEFAULT_MODE_VERSION`/`DEFAULT_CONFIGURATION_VERSION`
   constants were plain `pub const` while the plan simultaneously argued for `#[doc(hidden)]`
   on the two predicate functions for the same underlying reason (**fixed**, both
   constants now `#[doc(hidden)]` too, with the distinction from `MACP_VERSION` — which
   stays plain `pub`, being genuinely kernel-enforced — stated explicitly); Phase 1's
   semver-checks acceptance criterion claimed to verify the `doc(hidden)` decision, which
   `cargo-semver-checks` cannot distinguish either way (**fixed**, criterion narrowed to
   its real job — catching an accidental break — plus a new, separate, diff-readable
   criterion for the `doc(hidden)` attributes themselves).
9. Phase 2 never named the exact env var / file-vs-directory shape its locator function
   needed to share with Phase 3's CI step. **Fixed**, stated explicitly in both phases.

All nine items above are mechanical, unambiguous fixes — not open design questions — so
per `/plan`'s two-round cap, they were applied directly rather than triggering a third
adversarial round. This plan is now considered ready for `/implement`.
