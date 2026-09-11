# PROGRESS — spec #99 alignment (policy `schema_version` 3)

**Plan:** `plans/spec-99-schema-version-3.md` · **Base:** `882beeb` on `origin/main` (workspace `0.7.5`) · **Spec base:** `b59af6a` · **Built:** 2026-09-11 by Opus, two fan-out readers and a seven-stage executable probe in a detached worktree off `origin/main`.

## Repo map — read this instead of re-scanning

All line numbers are against **`origin/main` = `882beeb`**, not against the checked-out `feat/handoff-implicit-accept-rev2`. The only file the two differ in among everything below is `crates/macp-core/src/session.rs`, where G4 moved `CURRENT_SEMANTICS_REV` from `1` to `2` and added doc bullets, shifting later lines by 7.

### Policy evaluation

| Path | Purpose |
|---|---|
| `crates/macp-policy/src/evaluator.rs` | The whole voting path, 2598 lines. `:9-11` the "additive per §3" comment that #99 falsifies · `:12` **`SUPPORTED_SCHEMA_VERSIONS = &[1, 2]` — Phase 2's one-line half** · `:14-25` `check_schema_version`, called from `:109`, `:542`, `:601`, `:654`, `:721` (one per mode) · `:74-90` the rustdoc outcome table; its `NoVotes` row and its "deferred to spec issue #98 item 3" paragraph are both wrong after Phase 2 · `:103` `evaluate_decision_commitment_outcome`, the only entry point that matters · `:177-186` the `FinalizeDecline` arm — **Phase 4 lands here** · `:195` `let (_, reject_count, _, _) = aggregate_votes(...)` — **Phase 3 makes this decisive-only** · `:216` `if rules.voting.algorithm != "none"`, the voting-block entry — Phase 4 adds a conjunct · `:227` the `Passed` + `allow_decline_over_approval` branch that applies no decline guard — **Phase 3** · `:252-263` the `NoVotes` arm; its positive half is the #147 fail-open — **Phase 2 lands here** · `:285` `count_unique_voters` and `:297` `check_quorum`, both of which must stay weight-blind · `:317` `enum VotingResult` (private) · `:326` `check_voting_algorithm` (private; the discriminating observable, assertable only from this module's own `#[cfg(test)]`) · `:337` the front-of-dispatch `non_abstain_total == 0 => NoVotes` — **retained, now normative** · `:409` `weighted_total < 0.0` (comment cites `minimum: 0`, needs re-citing) · `:419` `weighted_total == 0.0 => NoVotes` (comment cites spec #98, needs re-basing) · `:470` `aggregate_votes` · `:495` `compute_weighted_votes`, `:508` **`unwrap_or(1.0)` — the electorate bug, Phase 3** |
| `crates/macp-policy/src/registry.rs` | `:17` `DECISION_VOTING_ALGORITHMS` (6 entries, `plurality` already present) · `:298` `schema_version == 0` is the *only* version check at registration · `:432` `validate_conditional_constraints` · **`:437` `if matches!(mode, "macp.mode.decision.v1" | "*")` — the mode guard the whole Decision block sits inside; Phase 1's new raw-JSON `weights` check must go *inside* it** · `:440` `weighted && weights.is_empty()` — widens to any supplied map in Phase 1 · `:446-450` `supermajority && threshold <= 0.5`; the `majority` sibling joins it · `:479-500` the rustdoc that cites `minimum: 0` "both **inclusive**" and defers to spec #98, twice · `:501` `validate_decision_voting`, `:511` `!(0.0..=1.0).contains(&threshold)`, `:522` `**weight < 0.0 || weight.is_nan()` — **Phase 1's two one-line edits** · `:1190` `register_zero_voting_threshold_succeeds` and `:1226` `register_zero_voting_weights_succeed` — **both invert** · `:1592-1610` `canonical_schema_dir` (env var, then sibling fallback; a set-but-missing dir *panics* by design) · `:1638` `enum_lists_match_the_canonical_schemas`, **`:1659` (`voting.threshold.minimum`) and `:1665-1668` (`voting.weights.additionalProperties.minimum`) are the two assertions that are red right now — and `assert_eq!` aborts at the first, so the second only becomes visible once the first is fixed. Expect two rounds, not one** |
| `crates/macp-core/src/policy/rules.rs` | `:22-23` `threshold` with `#[serde(default = "default_threshold")]`, `:45` `default_threshold() -> 0.5` — why an omitted threshold survives Phase 1 · `VotingRules.weights` is a `HashMap` defaulting to empty, which is why Phase 1 must discriminate on raw JSON, not the parsed struct |
| `crates/macp-policy/src/defaults.rs` | `:13` `policy.default` (`algorithm: "none"`) · `:53`, `:78`, `:102` the three reserved `policy.std.*` profiles, all `schema_version: 1`, thresholds `0.5` / `0.6666666666666666` / n-a — all register unchanged after Phase 1, verified |

**Reach:** nothing outside `macp-policy` reads `VotingResult`; modes call through `macp_core::PolicyEvaluator`. Phases 2–4 are entirely inside `crates/macp-policy/src/evaluator.rs` and observable only through `PolicyDecision` at the mode boundary — which is exactly why every acceptance criterion in those phases asserts the deny **reason** or the `VotingResult` variant, not just the variant of `PolicyDecision`.

### Session bootstrap (Phase 5)

| Path | Purpose |
|---|---|
| `crates/macp-core/src/session.rs` | `:28` `CURRENT_SEMANTICS_REV = 1` on `origin/main` (G4 has `2`) — **not the axis this plan uses** · `:345` `validate_canonical_session_start_payload` · **`:347-349` the empty-`participants` rejection — Phase 5 **moves** it (mode-scoped on `mode != "macp.mode.decision.v1"`), it does not delete it; see the strictness row below** · `:359-362` the 1000-participant cap, untouched · `:480` `standard_mode_requires_explicit_versions_and_participants`, the test that splits |
| `crates/macp-modes/src/mode/decision.rs` | `:101-111` `authorize_sender` — `Proposal`/`Evaluation`/`Objection`/`Vote` go through `is_declared_participant`, which returns `false` on an empty roster, so an empty-roster Decision session forbids every one of them **including the initiator's**. This is the reachability guard `decision_zero_participants.json` exists to test · `:113-121` `on_session_start`, **`:118` the second rejection Phase 5 removes** (and `:115`'s `session` parameter becomes unused — a `-D warnings` failure if left) · `:355` `session_start_requires_declared_participants` |
| `crates/macp-modes/src/mode/util.rs` | `:170-172` `is_declared_participant` — a plain `any()` over the slice · `:178` `check_commitment_authority`, which is role-based and unaffected by an empty roster |
| `crates/macp-modes/src/mode/mod.rs` | `:62-67` the **default** `authorize_sender`: `!participants.is_empty() && !contains(sender)` — an empty roster authorizes everyone. No standards-track mode uses it; same shape at `multi_round.rs:130` and `passthrough.rs:56`. **Phase 5 does not reach this and does not widen it** — dynamically registered extension modes already get `strict_session_start: false` (`mode_registry.rs:911`), so an empty roster and this fail-open default are **already reachable today**. Demoted from an open question to a standalone issue |
| The four modes Phase 5 must leave alone | `proposal.rs:196` (empty), `quorum.rs:273` (empty), `task.rs:134-140` (≥1 non-initiator), `handoff.rs:102` (`len() < 2`; the earlier cite `:191` was wrong — `:90` is `on_session_start`), `multi_round.rs:98` (empty) — each re-rejects in its own `on_session_start`, which is what confines the relaxation to Decision |
| `src/runtime.rs` | **Corrected cites.** `:353` `mode_name` is already in hand · `:360` `self.mode_registry.requires_strict_session_start(mode_name)` · `:362` the gated `validate_canonical_session_start_payload` call. (`:377-380` — what this map cited before — is the unrelated `mode_version` mismatch block.) Mirrored at `src/replay.rs:238-244` (`mode_name` bound), `:249` (strictness), `:256` (the gated call) — **not** `:296-298`. So the relaxation is replay-symmetric, and both sites already hold the mode name, which is what makes Phase 5's mode-scoping possible |
| `crates/macp-core/src/session.rs` (strictness) | `:309-319` the **free fn** `requires_strict_session_start` — a static name list · `:375-384` `validate_strict_session_start_payload(mode, payload)`, which **already takes `mode`** and already branches on it. **Trap:** the two call sites above use `ModeRegistry::requires_strict_session_start` (`crates/macp-modes/src/mode_registry.rs:450`, a per-entry flag), which disagrees with the free fn — `promote_mode` sets the flag `true` (`mode_registry.rs:621`) for names the free fn has never heard of. Do not swap one for the other while repointing |

### Replay and recovery (Phase 3's failure path — read before touching the evaluator)

| Path | Purpose |
|---|---|
| `src/replay.rs` | `:85` `replay_entry` · **`:133` `mode.on_message_at(...)?` — a policy Deny on a stored `Commitment` becomes an `Err` here and propagates straight out of `replay_session`** · `:191` `validate_replay_consistency` — **never reached** for such a session, because replay errored first · `:63-70` the checkpoint fallback to full replay when `policy_definition` is missing · `:317-322` full replay **re-resolves the bound policy from the live registry**, which RFC-MACP-0012 §8 item 2 forbids (plan Open question 6) |
| `crates/macp-modes/src/mode/decision.rs` | `:239-244` the `enforce_commitment_policy` call inside the `"Commitment"` arm — the only thing between a `PolicyDecision::Deny` and a failed replay |
| `crates/macp-modes/src/mode/util.rs` | `:119-144` `enforce_commitment_policy` · `:125-127` **an absent `policy_definition` returns `Ok(())`** — governance silently skipped · `:143` `Err(MacpError::PolicyDenied { reasons })` |
| `src/main.rs` | `:258` `strict_recovery` from `MACP_STRICT_RECOVERY` · `:351` the `validate_replay_consistency` call · **`:379-385` strict arm — the runtime refuses to start** · **`:386-392` default arm — `"failed to replay session; skipping"`, i.e. the session disappears from the registry on restart** |

### Conformance harness (Phase 6)

| Path | Purpose |
|---|---|
| `tests/conformance_loader.rs` | **Fixture discovery is a hardcoded list, not a glob.** `:526-535` `fixtures_dir()` — the *only* consumer of `MACP_CONFORMANCE_FIXTURES_DIR`; empty string counts as unset; fallback `CARGO_MANIFEST_DIR/tests/conformance` · `:537-545` `macro_rules! conformance_test!` · `:547-601` the 19 registrations — **12 lines get appended here** · `:15-39` `ConformanceFixture` (no `deny_unknown_fields`, so `_comment`/`description` are dropped silently; `policy_version` is **mandatory** here though the canonical schema leaves it optional) · `:300-307` `expected_state` accepts only `Open`/`Resolved`/`Expired` — the canonical schema also permits `Suspended`/`Cancelled` · `:388` `run_conformance_fixture`; **`:420-434` panics on a `SessionStart` failure**, which is why `decision_zero_participants.json` needs Phase 5 and not a loader change · `:356-386` `assert_replay_equivalence`, the strictest gate in the repo, and **it is live for every fixture — an earlier version of this map said "dormant", which is wrong.** Verified: the field is `#[serde(default = "default_true")]` (`:37-38`) with `default_true()` at `:52`, and the call is gated `if fixture.verify_replay_equivalence` at `:519-521`. **The default is `true` and no fixture in `tests/conformance/` sets it to `false`** (the only file that even names the key is `schema.json`, where it is a schema property), so it fires for **all 31** fixtures and will fire for all 12 new ones. Treat that as the opportunity it is: every newly vendored fixture gets a **free full-replay check** for nothing — `replay_session` is re-run over the log and compared on `state`, `resolution`, `mode_state` and the dedup set `seen_message_ids` (`:373-385`). **Executor-facing consequence: a Phase 3 regression will surface as a `replay state mismatch` (or `replay mode_state mismatch`) panic from a *conformance* test, not from an evaluator test.** That is not an obvious symptom for an evaluator change — expect it and do not chase it as a harness bug · `:616-639` `every_fixture_is_registered`, which `include_str!`s its own source — **vendoring without registering fails the suite** · `:646-698` `fixtures_conform_to_canonical_format`, `:695` the `checked >= 17` floor (two below the real count) · both guards **hardcode the vendored dir** (`:618`, `:654`) and ignore the env var, so a canonical-only fixture is invisible to the Rust side |
| `tests/conformance/` | 19 fixtures + `schema.json`, plus `cmt-hash/` (5 vectors + `vector-schema.json`). **3 files have drifted**, not 2: the two `_comment`-only reflows *and* `schema.json`, which lost `minItems: 1` on `participants`. `cmt-hash/` needs **no** change — all 6 byte-identical, both set differences empty |
| `.github/workflows/ci.yml` | `:555` `conformance-oracle` · `:564-566` the spec checkout, **no `ref:`** — this is why somebody else's merge reds our CI · `:569-608` `check_dir`, globbing `*.json` in both directions, so `schema.json` counts and `README.md`/`lint_fixtures.py` do not · `:606-607` the two invocations · `:626-629` the second run with `MACP_CONFORMANCE_FIXTURES_DIR` · `:636-653` the parity step: `MACP_POLICY_SCHEMAS_DIR` at `:646`, and `:648-653` pins the test name with `--exact` **and** asserts `1 passed`, because libtest exits 0 on a filter that matches nothing · `:683` the job is in the final gate's `needs:` |
| `integration_tests/tests/tier1_protocol/test_policy_registry.rs` | `:596-643` `register_policy_refuses_out_of_schema_values` (6 cases; gains 2) · `:645-672` `register_policy_accepts_schema_legal_boundary_values` — **`:654` `threshold: 0.0` and `:658` all-zero `weights` both invert**, leaving only the two quorum cases |

### Canonical source (read-only)

| Path | Purpose |
|---|---|
| `../multiagentcoordinationprotocol` at `b59af6a` | `schemas/json/macp-policy-descriptor.schema.json` — the "Version 3 is SEMANTIC, not additive" description · `schemas/json/policy/decision-rules.schema.json` — `threshold.exclusiveMinimum: 0`, `weights.minProperties: 1`, `weights.additionalProperties.exclusiveMinimum: 0`, and a new `allOf` arm pinning `majority` `threshold.minimum: 0.5` (inclusive, deliberately asymmetric with supermajority's exclusive `0.5`) · `schemas/json/tests/invalid-policy-rules/` — 4 negative fixtures naming each constraint · `rfcs/RFC-MACP-0012-policy.md` §4.1 (empty tally, legacy arm, electorate rule, threshold floor) and §8 (items 3 and 4 plus the two named `v<=2` changes) · `rfcs/RFC-MACP-0007-decision-mode.md` §6.2 (rewritten NoVotes bullet, decisive-reject decline guard, objection-authorized decline) · `schemas/conformance/README.md` — the 38-line block describing the 12 new fixtures as matched v3-vs-legacy pairs |

**The sibling checkout does not track what CI reads, and it moves under you.** During this planning pass it went from a dirty tree to a local branch `phase3-decision-rules-and-prose` sitting **4 commits ahead of `origin/main`** (`c36aae8` CI rule-schema validation, `4145608` quorum threshold vocabulary — which removes `weighted` from `threshold.type` — `58a6edd` mode-rule error codes, `11a977c` supermajority threshold and prose). `origin/main` is still `b59af6a`, and that is what `conformance-oracle` checks out with no `ref:`. Pointing `MACP_POLICY_SCHEMAS_DIR` at the sibling tree produces a `threshold.type has drifted` parity failure that **does not exist in CI**. Always export first: `git -C ../multiagentcoordinationprotocol archive b59af6a | tar -x -C <scratch>`. The 32 conformance `.json` files happen to be identical between the two, but the policy schemas are not. Note also that `4145608` and `11a977c` are the **next** wave of runtime-side work and will red this same job again when they merge; they are out of scope here.

### Measured baseline — the probe

Detached worktree off `882beeb`, all 32 canonical fixtures vendored, 12 registered, changes applied one at a time.

| Stage | Change | Conformance loader | Of the 12 new |
|---|---|---|---|
| 0 | none | 23 / 33 | **2 pass** (`decision_empty_tally_legacy`, `decision_legacy_require_vote_quorum`), 10 fail |
| A | `SUPPORTED_SCHEMA_VERSIONS = &[1,2,3]` | 25 / 33 | 4 pass (adds both `decision_none_v3_*`) |
| B | + `NoVotes` positive deny at `schema_version >= 3` | 29 / 33 | 8 pass (adds `empty_tally_binding`, `majority_empty_tally`, `supermajority_empty_tally`, `plurality`) |
| C | + `compute_weighted_votes` `unwrap_or(0.0)` | 31 / 33 | 10 pass (adds both `decision_weighted_zero_weight*`) |
| D | + objection-authorized decline skips the vote block | 32 / 33 | 11 pass (adds `decision_finalize_decline_empty_tally`) |
| E | + empty `participants` accepted | **33 / 33** | **12 / 12** |
| F | + registration mirrors tightened (`exclusiveMinimum`, `majority >= 0.5`) | 33 / 33 | 12 / 12; `enum_lists_match_the_canonical_schemas` green against a clean `b59af6a` export |
| G | + decisive-reject count and the `Passed`-arm decline guard | 33 / 33 | 12 / 12; **zero additional collateral** |

**Whole-workspace at stage G:** `758 passed / 5 failed`. The five, all asserting the pre-#99 contract and all enumerated in the plan: `macp-core::session::tests::standard_mode_requires_explicit_versions_and_participants`, `macp-modes::mode::decision::tests::session_start_requires_declared_participants`, `macp-policy::evaluator::tests::unsupported_schema_versions_are_denied`, `macp-policy::registry::tests::register_zero_voting_threshold_succeeds`, `macp-policy::registry::tests::register_zero_voting_weights_succeed`. Plus one `-D warnings` failure not counted by libtest: `unused variable: session` at `crates/macp-modes/src/mode/decision.rs:115` once `:118` is removed.

**Two tests that survive and must not be deleted:** `evaluator::tests::non_abstain_short_circuit_precedes_algorithm_dispatch` (`:1227`) and `evaluator::tests::zero_weighted_total_still_returns_no_votes` (`:1188`) both stay **green** through every probe stage. Their assertions are still correct; only their stated premises died. Rename and re-comment, never delete — the brief's instruction to retire the first one is based on the fix #147 proposed, which #99 did not adopt.

**`cargo semver-checks check-release --workspace` at stage G:** "Summary no semver update required" on all seven crates, 196 checks each. Re-check at ship time by reading the printed summary per crate, never the exit status (`DECISIONS.md` **D7**).

## Environment constraints (every phase)

- Prefix every cargo command `RUSTC_WRAPPER=""`. Never `sccache --stop-server`.
- Never `pkill`. An unrelated runtime holds `50051`; use scratch port `50123` and kill only the PID you started.
- **`MACP_POLICY_SCHEMAS_DIR` must point at a clean `b59af6a` export, not at `../multiagentcoordinationprotocol`** — see above. It is documented in no tracked env table; Phase 1 adds it.
- `tests/conformance/` is vendored and byte-diffed in both directions. Copy from a `git archive` export; never hand-edit, never reformat.
- `CLAUDE.md` is gitignored — its edits never appear in a PR diff. The tracked docs are `README.md`, `docs/policy.md`, `docs/API.md`, `docs/deployment.md`, `docs/modes.md`.
- No dependency is added or removed by any phase, so `integration_tests/Cargo.lock` needs no regeneration. If that changes, regenerate it in the same PR (`RUSTC_WRAPPER="" cargo metadata --manifest-path integration_tests/Cargo.toml --format-version 1 > /dev/null`).
- Branch from `origin/main` (`882beeb`). **Do not work on `feat/handoff-implicit-accept-rev2`.** Never edit `CHANGELOG.md`. Never force-push. Do not touch branch protection (12 required contexts, `strict: true`, zero reviews).
- Spec repo: **read and issues only. No PRs, no commits, no file edits.**

## Phase status

| Phase | Title | Status |
|---|---|---|
| 1 | Re-mirror the tightened decision-rules schema; invert the two deferral tests | **DONE** |
| 2 | Accept `schema_version` 3; evaluate the empty tally under the policy's declared version | TODO |
| 3 | The weighted electorate; decisive rejects only | TODO |
| 4 | The objection-authorized decline | TODO |
| 5 | Accept a zero-participant Decision session | TODO |
| 6 | Vendor 12 fixtures, sync 3 drifted files, register all 12 | TODO |

## Log

- 2026-09-11 (Phase 1 executed, branch `fix/spec-99-schema-version-3` off `882beeb`): the four registration mirrors changed as planned (`threshold > 0.0 && <= 1.0`, weight filter `<= 0.0`, `majority >= 0.5`, raw-JSON non-empty `weights` inside the `:437` mode guard), the two deferral tests inverted, seven tests added, four parity assertions moved or added. Gates: workspace `758 passed / 0 failed` (no pre-existing failures at this stage — the five the stage-G table predicts belong to Phases 2–5), tier 1+2 `119 + 8 + 5 passed / 0 failed`, fmt/clippy/doc clean, both lockfiles unmoved, all seven crates "no semver update required".

  **Two plan errors found, both in Phase 1's acceptance criteria.**

  1. **Criterion 4 (`register_mixed_sign_weights_fails`) is vacuous.** `{"a": 1.0, "b": -1.0}` was **already** refused at `882beeb` by the old `weight < 0.0` filter, so the test passes against unfixed code — mutation-verified. The plan asserts this criterion "closes #148", but its own Design question 3 says the opposite in passing ("negatives were already refused by this runtime's own mirror at `registry.rs:522`"), and `register_negative_voting_weight_fails` (`:1205` at base) already covered the shape. What Phase 1 newly forbids is the **zero** half. The test is kept as a regression pin naming the reported descriptor, with a comment saying so; the discriminating tests are `register_zero_voting_weights_fail` and `register_empty_weights_map_fails`.
  2. **Criterion 2's descriptor cannot discriminate the `threshold` floor.** `{"algorithm": "majority", "threshold": 0.0}` is refused by *either* mirror — the `exclusiveMinimum: 0` floor or the new `majority >= 0.5` arm — so reverting the floor alone leaves the test green (mutation-verified). Added `register_zero_voting_threshold_fails_for_unanimous_too`, which asserts on the range-check message text and is red under that mutation. `unanimous` is the right vehicle: the plan's own edge cases say the floor is deliberately unconditional and reaches the algorithms that never read `threshold`.

- 2026-09-11 (reverify pass, fresh Opus): 13 findings applied to the plan and this map. **Blockers:** Phase 3's risk note rewritten — the weighted-electorate reversal flips the **positive** direction too and far more commonly (worked example now in Phase 3 edge cases, Phase 3 docs and Enterprise concerns; the "decline only" third conjunct struck as false for this runtime); Phase 3's replay **failure mode** added (an affected session does not replay — skipped with a `WARN`, or startup refused under `MACP_STRICT_RECOVERY=1`) with a pre-upgrade audit query, plus a new "Replay and recovery" map section; Phase 3's stated reason for not gating replaced (the `decision_weighted_zero_weight_v1.json` argument is false — the loader creates a fresh session at the current `semantics_rev`), with the correction that `docs/deployment.md` §4 shipped **ungated** on a justification that does not transfer; and this map's "`assert_replay_equivalence` is dormant" claim corrected — it defaults `true` and fires for every fixture. **Should-fix:** Phase 5 now mode-scopes rather than flat-removes (`validate_strict_session_start_payload` already takes `mode`), with the repoint trap recorded; a positive-direction Phase 3 test added as criterion 8; Phase 1's raw-JSON `weights` check scoped inside the `:437` mode guard; Design question 1's replay-exactness premise moved to **Open question 6**; a zero-participant `unanimous` unit test added as Phase 5 criterion 6. **Nice-to-have:** the Priority-1 rename now cites `unanimous` vacuous truth first, open question 3 demoted to a standalone issue, Phase 5 docs state the tracked-invariant absence outright, and `MACP_POLICY_SCHEMAS_DIR` gets a `docs/deployment.md` env-table row in Phase 1. Several `file:line` cites corrected — see the rows above.
- 2026-09-11: Plan written. Base `882beeb`, spec `b59af6a`. No code changed in the repo; all measurement done in a detached worktree under the session scratchpad, removed afterward. Issues **#147**, **#148**, **#149** are closed by Phases 1–3; issue **#163** is superseded (its items 1–3 are Phase 1, its item 4 is Phase 2 but with the opposite resolution to the one it predicted, and its `schema_version` 3 paragraph — the part it flagged as "not yet planned anywhere" — turned out to be the largest item, spanning Phases 2 through 6).

---

## Local environment: a linker break that every remaining phase will hit

Found during Phase 2, unrelated to this repo. The default SDK (`MacOSX27.0.sdk`) is newer than
`/usr/bin/ld` (ld-1267), so **every link fails**:

```
tapi error: malformed file ... unknown architecture arm64e.x1-macos
```

Workaround, required on each cargo invocation alongside `RUSTC_WRAPPER=""`:

```
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk
```

Machine issue, not a repo one — CI is unaffected. Recorded here because a phase that hits it can
easily read it as a code regression.

Two other environment facts worth carrying forward:
- Agent bash calls reset cwd, so the spec export needs the **absolute** path
  `/Users/ajitkoti/code/multiagentcoordinationprotocol/multiagentcoordinationprotocol`, not the
  relative `../`.
- `cargo test -p macp-policy` **without** `MACP_POLICY_SCHEMAS_DIR` is not a neutral run:
  `canonical_schema_dir` falls back to the sibling checkout, which sits 4 commits ahead of spec
  `main`, and the parity test fails with a phantom `threshold.type has drifted` that does not exist
  in CI.
