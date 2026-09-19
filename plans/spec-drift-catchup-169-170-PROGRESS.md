# PROGRESS — spec-drift catch-up (#170) and drift-watcher hardening (#169)

**Plan:** `plans/spec-drift-catchup-169-170.md` · **Base:** `5e95c4a` on `main` (workspace `0.7.6`) · **Spec pin today:** `c137f735358a046d677b607315006bb1c03baabd` · **Spec `main`:** `0de1fab20bc396fdc5f1412e61fdc3d7baf0a64d` · **Built:** 2026-09-19 by Opus, with the full spec diff read, the annotation-strip proven by `jq`, and all three `conformance-oracle` steps executed locally against the proposed new pin.

All line numbers are against **`main` = `5e95c4a`**. Re-check any cite before editing; `evaluator.rs` is 3900+ lines and moves under new tests.

## The one-paragraph version

Nothing in the runtime is wrong. The spec moved four commits; exactly two files changed in the two trees this repo mirrors, and **neither requires any action here** — one is not a mirrored artifact, the other is an annotation-only schema edit. The pin bump is a single line and all three oracle gates are already green against the new revision, measured. `spec-drift.yml` filed #170 anyway because it classifies by raw byte diff with no filter at all; Phase 1 fixes that without making the suppressed change invisible. Separately, the spec change (#122) makes normative a rule this runtime already implements and has **zero** tests for; Phase 3 pins it.

## Repo map — read this instead of re-scanning

### The two workflows (Phases 1 and 2)

| Path | What matters |
|---|---|
| `.github/workflows/spec-drift.yml` | 191 lines, scheduled daily 07:00 UTC + `workflow_dispatch` (`:21-26`), `contents: read` + `issues: write`, gates no PR · `:16-19` the header's own "deliberately narrow … reporting it would train everyone to ignore this issue" — the principle Phase 1 must honour, not overturn · `:44-57` parses `SPEC_REV` back out of `ci.yml` with a `sed` expecting optionally-quoted lowercase hex (so Phase 2's new value must be full 40-char lowercase) · `:59-70` two checkouts, `spec-pinned/` and `spec-head/` · **`:72-140` the `id: diff` step — all of Phase 1's work is here** · `:76-78` the header note that single-quoted backticks trip shellcheck SC2016 and **actionlint exits non-zero on it** · `:79` `set -uo pipefail`, on top of GitHub's default `bash -e {0}` — so `-e` **is** live · `:89-95` the two-tree loop, `:94` `diff -rq -x 'README.md' … 2>&1 \|\| true` (no extension filter — this is why `lint_fixtures.py` counted) · `:96-100` the prefix-relabelling `sed`, deliberately relabelling rather than stripping so `Only in` keeps its direction · `:102-106` `drift=prose-only`, which fires **only** when the entire report is empty · `:108` `drift=yes` · `:110-138` the body build, `:124-132` the `MARKDOWN` heredoc checklist that becomes #170's "What to do" · **`:143` the file/update condition `== 'yes'`** · **`:167` the close condition `!= 'yes'` — fires for `none` AND `prose-only`, and will fire for the new `non-actionable` too; that is existing correct behaviour being extended, not worked around** · **`:176` the close comment text, which asserts the trees "no longer differ" — false in the new case, Phase 1 fixes it** · `:182-190` the Summary step |
| `.github/workflows/ci.yml` | **`:39` `SPEC_REV` — Phase 2's one line** · `:42-56` the `actionlint` job, **`:47` `continue-on-error: true`** so workflow lint is advisory and cannot catch a Phase 1 mistake for you · `:572` `conformance-oracle` · `:584` the spec checkout at `ref: ${{ env.SPEC_REV }}` · `:587-626` step 1, byte identity: `:595-623` `check_dir()`, **`:603` and `:615` the `*.json` globs in both directions** (this is why a `.py` change is out of scope), `:624-625` the two invocations, `:626` `exit $status` · `:644-647` step 2, `conformance_loader` against `MACP_CONFORMANCE_FIXTURES_DIR` · `:662-671` step 3, the parity test against `MACP_POLICY_SCHEMAS_DIR`, with `--exact` **and** a `grep` for `1 passed` because libtest exits 0 on a filter matching nothing |

**Steps fail fast.** `check_dir` is the oracle's first step and ends in `exit $status`, so while it is red the parity step never runs at all. Not a risk for Phase 2 (all three are green at the new pin, measured), but it is why the local replay must run all three rather than stopping at the first green.

### Policy evaluation (Phase 3)

| Path | What matters |
|---|---|
| `crates/macp-policy/src/evaluator.rs` | **`:195` `pub fn evaluate_decision_commitment_outcome`** — public, so `participants: &[]` is library-reachable even though it is not wire-reachable · `:333` `total_voters = count_unique_voters(&state.votes)` (deliberately **not** weight-aware) · `:334` `participant_count = participants.len()` · `:347-352` the `check_quorum` call · **`:353` `if rules.commitment.require_vote_quorum && !quorum_met && !objection_authorized_decline` — one of only two decision-affecting reads of the flag, and the only one live at `schema_version >= 3`** · `:354-360` the deny reason (`"vote quorum not met: {} voters of {} participants (quorum: {} {})"`) · `:440-478` the `NoVotes` arm · **`:456` `if policy.schema_version >= 3`** · **`:463` `else if rules.commitment.require_vote_quorum` — the second read, the legacy `v <= 2` arm, and the reason Phase 3's equivalence is v3-only** · `:464` its exact reason string `"no votes cast"` · `:553-561` `count_unique_voters` · **`:565-583` `check_quorum` — private, no unwrap, no integer division, `:574-575` the `total_participants == 0` guard returning `value <= 0.0`. The divide-by-zero #169 worried about does not exist** · `:572` the `"count" \| "n_of_m"` arm, `:581` the `_` fallback (same comparison) |
| `crates/macp-policy/src/evaluator.rs` `mod tests` | `:1097-1098` one flat `mod tests`, `use super::*`, so private fns are callable · **`:1102-1110` `make_policy`, `:1108` `schema_version: 1` — the trap; override it in every new test** · `:1112-1143` `make_state_with_votes(Vec<(proposal, voter, vote)>)` · `:1145-1151` `participants()` → three `agent://` ids · `:1841` `zero_participant_unanimous_is_no_votes_not_a_pass` (the existing zero-roster precedent, v3) · `:1958` `empty_tally_under_schema_version_2_still_follows_require_vote_quorum` (covers the **absent** spelling at v2) · `:2164-2225` the `// ── Quorum checking ──` section: `:2167` `quorum_count_requirement`, `:2189` `quorum_percentage_requirement` (**unmet only**), `:2208` `quorum_not_required_by_default` — **Phase 3's new section goes after this block** · `:3478` the **only** direct `check_quorum` call in tests, and it is a precondition assertion, not a test of the function · `:3614` `split_votes(approve, reject)` → `(DecisionState, Vec<String>)` with generated `agent://a{i}` / `agent://r{i}` rosters · `:3630` `approves(policy, state, people) -> bool` · `:3744` `voting_quorum_is_inert_without_require_vote_quorum` · `:3762` `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum` · `:3782` `no_decisive_votes_always_blocks_a_negative_commitment` (already sweeps `schema_version` 1/2/3 — the pattern to copy) |
| `crates/macp-policy/src/registry.rs` | `:29` `DECISION_VOTING_QUORUM_TYPES = ["count", "percentage"]` — `n_of_m` is **not** admissible for Decision though `check_quorum` accepts it · `:291-313` `validate_definition` — validates `policy_id`, reserved namespace, `schema_version == 0`, `rules.is_object()`, mode rules, conditional constraints, and **never `description`** (relevant to spec #120, Open question 3) · `:611-623` the quorum checks inside `validate_decision_voting`, **`:618` `value < 0.0 \|\| is_nan()` — the only rejection, so `0` registers** · `:1206-1214` `decision_policy`, **`:1212` `schema_version: 1` — same trap** · `:1226` `refuse`, `:1235` `accept` · **`:1445-1449` `register_zero_voting_quorum_value_succeeds` — the one existing test, `percentage` only, at v1** · `:1828-1845` `canonical_schema_dir` (env var first; a set-but-**missing** dir `assert!`s by design at `:1831-1839`; `:1842-1844` falls back to the sibling checkout when unset) · **`:1871-2002` `enum_lists_match_the_canonical_schemas`, fourteen assertions, every one a schema keyword — `enum`, `exclusiveMinimum`, `maximum`, `minimum`, `minProperties`, `required`, `type`, `const`. It names `$comment` and `description` nowhere and `commitment` nowhere. `:1900-1902` is the `voting.quorum.properties.value.minimum == 0` assertion, a sibling of the pointer #122 edited and the best mutation target for Phase 1 criterion 2** |
| `crates/macp-policy/src/defaults.rs` | `:13` `policy.default` ships `"quorum": {"type": "count", "value": 0}` with `require_vote_quorum: false` — so the zero floor is the *default* shape, not an exotic one · `:56`, `:81`, `:104` the three `policy.std.*` profiles, all non-zero floors (`count` 1 / 2 / 1) at `schema_version: 1`, all `require_vote_quorum: true`. None is affected by Phase 3 |
| `crates/macp-core/src/policy/rules.rs` | `:49-66` `QuorumRules` — **`:52-53` `quorum_type` defaults via `default_quorum_type()`, `:55` `value` is `#[serde(default)]` so it defaults to `0.0`, `:67` the default type is `"count"`.** Together: an absent `quorum` key, an absent `type`, and an absent `value` all land on a zero `count` floor (Open question 5) |
| `crates/macp-core/src/policy/mod.rs` | `:65` `CommitmentRules.require_vote_quorum` (definition only — nothing outside `macp-policy` reads it as an input) · **`:170-184` `PolicyEvaluator::evaluate_decision_commitment_outcome` — **`#[deprecated]` at `:170`**, delegates to `evaluate_commitment`. The live public routes to `check_quorum`'s zero-participant branch are `macp_policy::evaluator::evaluate_decision_commitment_outcome` and `DefaultPolicyEvaluator::evaluate_commitment`; cite those** |

**Reach:** `require_vote_quorum` has exactly **two** decision-affecting reads in the whole workspace, `evaluator.rs:353` and `:463`. Verified by grep across `crates/` and `src/`: every other hit is a test literal, a struct field definition, or a default. That two-site fact is the entire proof of the v3 equivalence, and it is what makes Phase 3 tests rather than a fix.

### Why the zero-participant branch is not wire-reachable (read before writing Phase 3 test 1's roster axis)

| Path | What matters |
|---|---|
| `crates/macp-modes/src/mode/decision.rs` | `:90-92` `commitment_ready(state) = !state.proposals.is_empty()` · `:101-111` `authorize_sender` — `Proposal`/`Evaluation`/`Objection`/`Vote` go through `is_declared_participant`, **false over an empty roster for every sender, initiator included** · `:113-141` the doc comment and `on_session_start` that make a zero-participant Decision session legal · **`:249-267` the `Commitment` arm: `:251` the `commitment_ready` gate returns `InvalidPayload` BEFORE `:256` `enforce_commitment_policy`.** So in a zero-participant session no `Proposal` is ever accepted, `commitment_ready` is never true, and the evaluator is never entered |
| `crates/macp-modes/src/mode/util.rs` | `:119-144` `enforce_commitment_policy` · `:125-127` an absent `policy_definition` returns `Ok(())` early · **`:130` `participants: &session.participants`** — the only wire source of the slice, so an empty slice requires a zero-participant session |
| `crates/macp-core/src/session.rs` | `:597-610` `validate_canonical_start`, **`:607` `if payload.participants.is_empty() && !allow_empty_participants`** — the mode-scoped carve-out that made zero-participant Decision sessions legal (shipped in PR #165) |

**Consequence for the plan:** `check_quorum(…, total_participants: 0)` is **library-reachable, not wire-reachable**. Test it, and say so in the test comment. Do not repeat the briefing's claim that it is live in production.

### Canonical source (read-only)

| Path | What matters |
|---|---|
| `/Users/Shared/multiagentcoordinationprotocol/multiagentcoordinationprotocol` | Sibling spec checkout. **`origin/main` = `0de1fab2`, but HEAD sits on branch `fix/issue-128-129-negative-fixture-isolation` with six modified and three untracked files.** None currently under the two mirrored trees — which is luck, not a property. **Always `git archive` a clean export; never point `MACP_POLICY_SCHEMAS_DIR` or `MACP_CONFORMANCE_FIXTURES_DIR` at this tree.** Read-only, and issues only: no commits, no PRs, no edits |
| Commits in the pin range | `958448c` prose tooling · `6f300c8` **spec #120** — `description` required on `macp-policy-descriptor.schema.json` **and** on inline fixture policies in `lint_fixtures.py` · `18a38a7` **spec #122** — the vacuous participation floor, RFC-MACP-0012 → `1.6.0-draft` · `0de1fab` prose tooling |
| `rfcs/RFC-MACP-0012-policy.md` §4.1 at `0de1fab` | The new **"Vacuous participation floor (schema_version ≥ 3)"** paragraph: "the flag gates nothing: `require_vote_quorum: true` is equivalent to `require_vote_quorum: false`, and a runtime MUST evaluate it as such. The combination is an authoring smell, not an error: a runtime MUST NOT reject the descriptor at admission for it and MUST NOT substitute a floor the policy did not declare, though tooling MAY warn. This equivalence is scoped to `schema_version ≥ 3`." Followed immediately by the unchanged **"Legacy empty-tally rule (schema_version ≤ 2)"** paragraph, which is why the divergence is deliberate |
| `schemas/json/macp-policy-descriptor.schema.json` at `0de1fab` | `required` is now `["policy_id", "mode", "schema_version", "rules", "description"]`. **Watched by neither `conformance-oracle` nor `spec-drift.yml`** — Long-term posture, Open question 3 |

## Measured baseline — 2026-09-19, at `5e95c4a`, before any change

**The spec diff, `c137f735..0de1fab2`, restricted to the two mirrored trees:**

| Check | Result |
|---|---|
| `git diff --stat` on both trees | 2 files, 4 insertions, 3 deletions |
| `git diff --name-status --diff-filter=ACDR` on both trees | **empty** — nothing added, deleted or renamed |
| Recursive `jq` strip of `description`/`$comment`/`title`, key-sorted, on `decision-rules.schema.json` at both revisions | **byte-identical** — genuinely annotation-only |
| Canonical fixtures carrying an inline `policy` object | 17 of the 33 fixtures in the top directory (the corpus is 34 top-level `.json` including `schema.json`, plus 6 under `cmt-hash/`), and **all 17 already carry `description`**, so `lint_fixtures.py`'s new requirement implies no upcoming fixture churn |

**The three `conformance-oracle` steps against a clean `git archive` export of `0de1fab2`:**

| Step | Result |
|---|---|
| `check_dir` replay, `tests/conformance` + `tests/conformance/cmt-hash` | **0 MISSING / 0 DRIFT / 0 EXTRA**, exit 0. 34 canonical `.json` files in the top directory |
| `cargo test --test conformance_loader` with `MACP_CONFORMANCE_FIXTURES_DIR` | **35 passed / 0 failed** |
| `cargo test -p macp-policy --lib -- --exact registry::tests::enum_lists_match_the_canonical_schemas` with `MACP_POLICY_SCHEMAS_DIR` | **1 passed** |

**Conclusion carried into Phase 2:** the pin bump needs no vendoring, no mirror edit, and no behaviour change. If the executor's diff for Phase 2 contains anything but `.github/workflows/ci.yml:39`, the diagnosis was wrong — stop and re-plan rather than patching.

**Also measured:** `register_zero_voting_quorum_value_succeeds` passes today, confirming `{"type": "percentage", "value": 0}` registers cleanly; `actionlint` is clean on `spec-drift.yml` at `5e95c4a`, so any Phase 1 finding is new.

## Environment constraints (every phase)

- **The local linker is broken and every `cargo` command fails without a workaround.** The default SDK (`MacOSX27.0.sdk`) is newer than `/usr/bin/ld`, producing `tapi error: malformed file … unknown architecture arm64e.x1-macos`. Prefix every cargo invocation with **both**:
  ```
  SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk RUSTC_WRAPPER="" cargo …
  ```
  Verified working 2026-09-19. This is a machine issue, not a repo one — CI is unaffected — but a phase that hits it will read it as a code regression.
- **`cargo-semver-checks` does not run on this machine.** `cargo-semver-checks 0.45.0` rejects the pinned toolchain's rustdoc output (`error: unsupported rustdoc format v57 … supported v53, v55, v56`). Phase 3 criterion 9 must therefore be reported as deferred-to-CI with that reason, never as a pass that was not measured. When it *does* run, use `CLAUDE.md` §8a's differential form and compare the failure list against `5e95c4a` — the absolute "no semver update required" form is already false at HEAD because `7c652b6` carries the two `0.8.0`-forcing breaks.
- **Phase 1's harness needs `git clone`, not `git archive`.** The step's first statement is `git -C spec-head rev-parse HEAD` (`spec-drift.yml:80`) and `git log` follows at `:119`; an archive export is not a repository, so under the live `-e` the whole script aborts silently. Use `git clone --no-hardlinks "$SPEC" spec-head && git -C spec-head checkout <rev>` for both trees. Phase 2's export is unaffected — it needs no history.
- **`shellcheck` cannot lint a workflow file directly** (measured: `SC1083` on `${{ }}` at `:63`, `SC1073` on the `MARKDOWN` heredoc at `:124`, even at `5e95c4a`). Use `actionlint`, which invokes shellcheck per `run:` block. And note that shellcheck **is** jq-aware: a single-quoted `jq` program containing `$comment` raises no SC2016.
- `timeout(1)` is **not** on this machine's `PATH`, and the default shell here is `zsh`: `grep --include=*.rs` needs the pattern quoted, and `local`/`readonly` collisions bite in inline scripts. Write non-trivial shell through `bash -c '…'`.
- Agent bash calls reset cwd between invocations — use **absolute** paths everywhere, including the spec export.
- **`MACP_POLICY_SCHEMAS_DIR` must point at a clean `git archive` export**, never at the sibling working tree (which is on an unrelated branch with uncommitted work). A set-but-missing directory `assert!`s by design (`registry.rs:1831-1839`); a *dirty or ahead* one fails quietly wrong. Running `cargo test -p macp-policy` with the var **unset** is likewise not neutral: `canonical_schema_dir` falls back to the sibling tree (`:1842-1844`).
- `tests/conformance/` is vendored and byte-diffed in **both** directions by `check_dir`. Never hand-edit, never reformat. No phase in this plan vendors anything.
- `CLAUDE.md` is gitignored (`.gitignore:20`) — its edits never appear in a PR diff. The tracked docs are `README.md`, `docs/policy.md`, `docs/API.md`, `docs/deployment.md`, `docs/modes.md`, `docs/testing.md`.
- No dependency is added or removed by any phase, so **neither** `Cargo.lock` needs regenerating. If that ever changes, regenerate `integration_tests/Cargo.lock` in the same PR (`cargo metadata --manifest-path integration_tests/Cargo.toml --format-version 1 > /dev/null`) — the `integration` job enforces it with `--locked`.
- Branch from `main` (`5e95c4a`). Never edit `CHANGELOG.md` (release-plz owns it). Never force-push. Do not touch branch protection.
- Spec repo: **read and issues only.** No PRs, no commits, no file edits.

## Phase status

| Phase | Title | Status |
|---|---|---|
| 1 | Classify a drift report before escalating it, and keep what is suppressed visible | **DONE** |
| 2 | Bump `SPEC_REV` to `0de1fab2` | **DONE** |
| 3 | Pin the vacuous participation floor with tests, and document it | **TODO** |

**PR strategy:** one PR, three commits (per the plan's own recommendation — three disjoint
file sets, no shared symbol, any phase droppable independently). Branch:
`ci/spec-drift-catchup-169-170`, off `main` at `1990c9b` (which itself sits on `5e95c4a` plus
one unrelated pre-existing docs commit correcting stale phase-status tables in
`spec-99-schema-version-3*.md`, committed separately before this branch was cut).

**Dependencies:** none between them. Three disjoint file sets, three independently verifiable phases. One PR with three commits is the recommended shape; any phase can be dropped or landed alone.

**Ordering hazard, not a dependency:** once Phase 2 lands, `spec-drift.yml:82-86`'s `head_sha == PINNED` short-circuit returns `drift=none`, so Phase 1's classification can no longer be exercised by a scheduled run or a `workflow_dispatch`. Phase 1's acceptance is deliberately a **local harness over the real `c137f73 → 0de1fab2` pair**, which is unaffected by landing order.

**Issues:** **#170** closes on Phase 2 (and would also close on Phase 1 alone, since a `non-actionable` verdict triggers the existing `!= 'yes'` close step — they are alternative closers, both wanted). **#169** needs all three: ask 1 is Phase 2, ask 2 is Phase 1, and the "Separately, and worth more than the above" section is Phase 3.

## Log

- 2026-09-19 (Phase 2 executed and verified, branch `ci/spec-drift-catchup-169-170`):
  `.github/workflows/ci.yml:39` `SPEC_REV` bumped from `c137f735358a046d677b607315006bb1c03baabd`
  to `0de1fab20bc396fdc5f1412e61fdc3d7baf0a64d`. Only that one line changed — `git diff
  --name-only` is exactly `.github/workflows/ci.yml`. Verified against a freshly-built clean
  `git archive` export of the new pin (not the sibling's dirty working tree, which sits on an
  unrelated branch): the new SHA's content spot-checked directly (the `$comment`/`description`
  annotation edits at the two #122 pointers, confirmed present and confirmed to be the only
  diff at those pointers). All 6 acceptance criteria: (1) new SHA appears exactly once, old SHA
  gone; (2) `check_dir` replayed verbatim against both mirrored trees, 0 MISSING/0 DRIFT/0
  EXTRA, exit 0; (3) `enum_lists_match_the_canonical_schemas` against the export: 1 passed; (4)
  `conformance_loader` against the export: 35 passed/0 failed; (5) diff scope confirmed exactly
  one file; (6) deferred to post-merge (#170 evidence comment) per the criterion's own text.
  Also ran both cargo suites *without* the env vars (vendored-directory path): 198 passed
  (`macp-policy --lib`) and 35 passed (`conformance_loader`), confirming the fallback path is
  unaffected. `actionlint .github/workflows/ci.yml` clean (exit 0).

  **Fresh Opus verify: PASS, first round.** Independently rebuilt its own clean archive export,
  independently re-ran all four measurable criteria (matching results exactly), independently
  read `enum_lists_match_the_canonical_schemas` and confirmed its 14 assertions never touch
  `$comment`/`description`, independently diffed the schema file between the two pins and
  confirmed the only two hunks are the two annotation pointers the plan names, and confirmed
  spec #120's descriptor-schema change sits outside both gates' scope (Open question 3,
  correctly left unimplemented here). No gaps found. No `ASSUMPTIONS.md` entries — fully
  prescriptive phase, everything measured rather than assumed.

- 2026-09-19 (Phase 1 executed, branch `ci/spec-drift-catchup-169-170` off `1990c9b`):
  `.github/workflows/spec-drift.yml` rewritten exactly per the plan's Approach — classifier
  inserted between the raw `diff -rq` report and the verdict, the in-place `sed -i` moved to
  after classification, `fetch-depth: 0` added to the `spec-head` checkout, `LC_ALL=C` added
  to the diff, three-way verdict (`yes`/`non-actionable`/`prose-only`/`none`), visibility
  (step summary + capped `::notice::`s + literal annotation diff) emitted before any exit,
  close-comment text computed in the `diff` step as a new `close_comment` output and consumed
  by the close step. `docs/testing.md:121` gained one paragraph documenting the pin +
  watcher + classification, per Phase 1's Docs field.

  **Verified with a local harness** (`git clone`, not `git archive`, per the plan's own
  correction — the step's first statement needs a real repo) against real checkouts of
  `c137f735` (pinned) and `0de1fab2` (head), all 8 acceptance criteria:
  1. Real event → `drift=non-actionable`, exactly 2 suppressed lines
     (`lint_fixtures.py`, `decision-rules.schema.json`), 0 actionable. Matches #170 exactly.
  2. Mutating `voting.quorum.value.minimum` (a pointer the parity test reads) from `0` to `1`
     → `drift=yes`, that file actionable, `lint_fixtures.py` still correctly suppressed.
  3. Byte flip in a top-level fixture and in a `cmt-hash/` fixture → both `drift=yes`,
     both actionable — confirms the `case` glob crosses `/` into `cmt-hash/`.
  4. File added under `spec-head/schemas/json/policy/` → `Only in upstream:` actionable;
     file deleted from `spec-head/schemas/conformance/` → `Only in pinned:` actionable.
     Both directions confirmed distinguishable and both block.
  5. Malformed JSON in the policy schema → `jq` fails → fails closed to actionable, not a
     crash, not a silent suppression.
  6. Visibility (step summary with both suppressed paths + reasons + the literal annotation
     diff, `::notice::`s) present in the criterion-1 run; issue-filing condition (`== 'yes'`)
     correctly evaluates false.
  7. Close-comment text correct in all three non-`yes` cases — verified `none` (pin current),
     `prose-only` (head moved to `958448c`, which touches neither mirrored tree), and
     `non-actionable` (criterion 1) each independently; only `non-actionable` uses the new
     "differ but not actionable, pin remains behind" wording, the other two keep the original
     "no longer differ" text, correctly.
  8. `actionlint .github/workflows/spec-drift.yml` clean (exit 0), confirmed both before and
     after the mutation tests. Commit enumeration confirmed working (no more "could not
     enumerate" fallback) — it correctly lists the 2 commits that touch the two mirrored
     trees (`18a38a7`, `6f300c8`) under the path-filtered `git log` call, which is unchanged
     from the original step; the plan's "four-commit list" phrasing referred to the range
     total, not the path-filtered enumeration.

  No production Rust code touched. `git diff --stat`: `.github/workflows/spec-drift.yml`
  (+220/-12) and `docs/testing.md` (+1 paragraph). No new `ASSUMPTIONS.md` entries — the plan
  was fully prescriptive for this phase and every criterion was empirically validated rather
  than assumed.

  **Correction, logged after the gap-closing round below:** the "all 8 acceptance criteria"
  verification above ran the extracted step under plain `bash script.sh` — no `-e`. GitHub
  Actions actually runs a `run:` block with no `shell:` override as `bash -e {0}`, layered on
  top of the script's own `set -uo pipefail`. My harness never exercised that, so it could not
  have caught (and did not catch) the blocker the first verifier round found below. Criteria 1,
  3, 4, and the `non-actionable` sub-case of 7 are the ones that actually execute the line that
  turned out to crash under `-e`; those are the ones re-run and re-confirmed under real `bash -e`
  semantics in the entry below. Criteria 2, 5, 6, 8, and the `none`/`prose-only` sub-cases of 7
  exit before reaching that line regardless of `-e`, so the original run stands for those.

- 2026-09-19 (Phase 1 verify #1, fresh Opus): **GAPS** — 1 blocker, 3 minor.
  **Blocker:** `spec-drift.yml`'s suppressed-detail block runs
  `diff -u "spec-pinned/$rel" "spec-head/$rel" | sed 's/^/  /'` with no `|| true`. `diff -u`
  always exits 1 when its inputs differ — which is guaranteed here, since this code path is
  only reached after an earlier `diff -rq` already proved a difference — so under the shell's
  live `-e` (GitHub Actions' default `bash -e {0}`, undisturbed by the script's own
  `set -uo pipefail`) that pipeline aborts the entire step. `drift` is left unset in
  `$GITHUB_OUTPUT`, the step summary stays empty, and both downstream steps (file-issue,
  close-issue) silently no-op on their implicit `success() &&` guard. This is exactly the real
  #170 happy path — a `schemas/json/policy/*.json` file classified as annotation-only-suppressed
  — so as written, Phase 1 would have shipped a workflow that silently does nothing on the one
  scenario it exists to handle. Verifier caught it via a minimal repro plus a `bash -x` trace of
  the real extracted step showing execution stop immediately after that line. My own harness
  (`run.sh`) missed it only because it invoked the extracted script with plain `bash`, not
  `bash -e`.
  **Minor (3):** (a) the step's header comment still asserted the struck SC2016 claim (see
  the earlier plan-review log entry) as a blanket rule, and the new code contradicts it with
  clean single-quoted backticks — needed rewriting to correctly scope the claim and add the
  more load-bearing `bash -e` explanation; (b) the same `diff -u` call lacked the `LC_ALL=C`
  prefix used everywhere else for locale-stable output; (c) the verdict-computation `if/elif`'s
  theoretically-unreachable `else` (`verdict="prose-only"` safety net, distinct from the
  earlier-exiting `prose-only` case) never emitted a `close_comment`, which would break the
  close step's `gh issue close --comment ""` under that step's own `set -euo pipefail` if ever
  reached.

  **Fixes applied**, all to `.github/workflows/spec-drift.yml`: added `LC_ALL=C` and `|| true`
  to the `diff -u | sed` pipeline with a comment explaining why (closes the blocker + minor b);
  rewrote the step's header comment to correct the SC2016 scoping and explain the live `bash -e`
  semantics (closes minor a); added the same `close_comment` emission pattern to the safety-net
  `else` branch (closes minor c). `actionlint` re-run clean (exit 0) after the fix.

  **Re-verification under real `bash -e` semantics** (GNU-sed-compatible `PATH` shim used for
  local macOS testing only — the actual `ubuntu-latest` runner has GNU sed natively, so the
  shipped YAML is unaffected): extracted the updated step to `diffstep_v2.sh` and ran
  `bash -e diffstep_v2.sh` against six cases —
  1. Real event (`c137f735` → `0de1fab2`): exit 0, `drift=non-actionable`, correct
     `close_comment`, 5154-byte step summary, 2 notices. **This is the exact line the blocker
     was in — confirmed fixed.**
  2. Actionable mutation (`voting.quorum.value.minimum` 0→1): exit 0, `drift=yes`.
  3. Real event + a fixture byte flip added on top (both a suppressed annotation-only diff and
     an actionable fixture diff in the same run — the combo case the verifier specifically
     flagged as worth re-checking): exit 0, `drift=yes`, both classifications correct.
  4. Malformed JSON in the policy schema (`jq` parse failure → fails closed to actionable):
     exit 0, `drift=yes`, no crash.
  5. `none` case (head at the pin): exit 0, `drift=none`.
  6. `prose-only` case (head at `958448c`, touches neither mirrored tree): exit 0,
     `drift=prose-only`.

  All six exit 0 under `-e`. Cases 1 and 3 are the ones that reach the previously-crashing
  `diff -u` line (a suppressed policy-schema entry present in the report) and both now
  succeed — the blocker is confirmed closed, not just patched-and-hoped. Cases 2, 4, 5, 6
  confirm the fix didn't regress any other classification path.

- 2026-09-19 (Phase 1 verify #2, fresh Opus, re-verify against the verify #1 gap list): **PASS**
  — all 4 prior items confirmed closed, no new issues. Independently re-extracted the `id: diff`
  step and ran it under real `bash -e` (not plain `bash`) against fresh `git clone` checkouts of
  the sibling spec repo at the real `c137f735`/`0de1fab2` pair: exit 0, `drift=non-actionable`,
  correct `close_comment`, 5154-byte step summary — matching this log's own numbers exactly, an
  independent reproduction rather than a re-read of my claim. Ran a **negative control**: patched
  a copy with `LC_ALL=C`/`|| true` stripped from that one line and re-ran under `bash -e` — it
  reproduced the original failure precisely (exit 1, `GITHUB_OUTPUT` has only `head_sha=`, no
  `drift=` key) — confirming the fix is causal, not coincidental. Confirmed the header-comment
  rewrite matches measured reality (ran `actionlint` itself, exit 0; confirmed via a smoke test
  that actionlint genuinely invokes shellcheck). Confirmed the `else`-branch `close_comment` text
  is consistent with the other two non-`yes` exit paths. `git diff --numstat`:
  `.github/workflows/spec-drift.yml` +244/-16, `docs/testing.md` +2/-0 — in the expected range,
  no production Rust code touched, no unrelated files.

  **Phase 1 finalized.** Plan `Status: DONE` (with the blocker divergence noted inline, per
  `/implement`'s own rule that a plan should say so when the actual approach diverged). No
  `ASSUMPTIONS.md` entries — the blocker was a verification-round bug fix, not an ambiguous
  design call. Committing as one focused commit next, then proceeding to Phase 2.

- 2026-09-19 (reverify pass, fresh Opus): **REVISE** — 3 blockers, 6 should-fix, 7 nice-to-have, all applied to the plan and this map; see the plan's `## Plan review` for the table. The reviewer re-measured every measured claim independently and **all reproduced**. The blockers were all in the verification layer, not the design: Phase 3's semver criterion was false at HEAD (`7c652b6` already carries the two `0.8.0`-forcing breaks, so it must be a **differential** check per `CLAUDE.md` §8a, and `cargo-semver-checks` is currently unrunnable here — `unsupported rustdoc format v57`); criterion 5's NaN mutation claim named a red that will not appear; and Phase 1's harness aborted on its own first line because `git archive` produces no repository for `git -C spec-head rev-parse HEAD` (`spec-drift.yml:80`) — it must be `git clone` + `checkout`. Two findings improved the work rather than just correcting it: test 1 gained an **empty-tally-at-v3 row** without which the `schema_version` axis was cosmetic and criterion 4's `>= 4` mutation was undetected; and `fetch-depth: 0` was folded into Phase 1 after the reviewer noticed that "Commits since the pin" is **already broken** in the very step Phase 1 rewrites (live #170 shows the fallback text; the checkouts at `:59-70` default to depth 1). One finding was demoted rather than implemented: closing on `non-actionable` leaves nothing tracking pin age, which is a policy decision, so it is **Open question 5**, not a phase.

  **An earlier claim of this plan, struck:** the assertion that single-quoting the `jq` program trips shellcheck SC2016 is **false** — measured, shellcheck is jq-aware and both shellcheck and actionlint are clean on the single-quoted form. The workflow's own `:76-78` note is about plain string literals reaching the issue body and does not generalise. Single-quote the program; the only real trap there is `set -u`, which forbids double quotes.

- 2026-09-19: Plan written. Base `5e95c4a`, spec pin `c137f73`, spec `main` `0de1fab2`. No code changed in the repo; all measurement done against a clean `git archive` export under the session scratchpad. **Four corrections to the briefing, all verified:**
  1. **The `check_quorum` divide-by-zero risk #169 flags does not exist.** `evaluator.rs:574-575` already guards `total_participants == 0` and returns `value <= 0.0` without dividing.
  2. **That branch is not wire-reachable either.** `decision.rs:251`'s `commitment_ready` gate (`:90-92`) runs before `enforce_commitment_policy` (`:256`), and a zero-participant Decision session can never accept a `Proposal` (`:101-111`), so the evaluator is never entered. It **is** reachable through the public `PolicyEvaluator` trait, which is why it still warrants a test — framed as a guard on public surface, not a production path.
  3. **The annotation-only suppression must not make the diff invisible.** #122's `$comment` states a new **MUST**; suppressing it silently would have hidden the very signal that produced #169's larger ask. Phase 1 separates escalation from visibility, and the visibility half (step summary, `::notice::`, the literal annotation diff, the suppressed count echoed into any filed issue) is load-bearing rather than decorative.
  4. **The pin range crosses a normative spec change neither gate can see** — spec #120's `description` requirement on `macp-policy-descriptor.schema.json`, a file outside both `conformance-oracle`'s and `spec-drift.yml`'s scope. This runtime is conformant anyway (proto3 presence; the RFC's own `1.5.0-draft` changelog says "Presence only: no `minLength` is imposed"), but nothing here asserts it. Recorded as Open question 3 and in Long-term posture rather than folded into a phase.

  **One scope call made rather than asked** (per the autonomy ladder): the satisfied-percentage-quorum cases stay in, folded into Phase 3's `check_quorum` table. Three lines, closes a real coverage hole, and without them the zero-floor table test cannot be falsified. Recorded as Open question 1 with its answer.
