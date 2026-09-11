# PLAN — macp-runtime backlog closeout (policy correctness, handoff implicit accept, watch_sessions bound)

**Written:** 2026-09-10 · **Base:** `999890e` (v0.7.4) on `main` · **Planner:** Opus, four fan-out readers
**Models for execution:** Opus executes, fresh Opus verifies. **NO FABLE AT ANY TIER** (user instruction);
where the skill would escalate to Fable, substitute a fresh Opus agent and record the substitution in PROGRESS.md.

---

## Context

Five open issues (#145–#149), one blocked dependabot PR (#153), and two unblocked items from
`plans/defer/follow_ons.md` (the handoff implicit-accept timer, the `watch_sessions` initial-sync
memory bound). Everything else in `plans/` is closed and `ASSUMPTIONS.md` has zero `UNCONFIRMED`
entries.

**Six things the issue text and the task brief got wrong.** All were verified against `999890e` by
reading the code; none is inferred from the issue bodies.

1. **#147 is not an oversight — it is the normative contract.** RFC-MACP-0012 §4.1 (`:113`) states it:
   *"with `require_vote_quorum` false, a positive commitment is not blocked by the absence of votes,
   even under `majority` or `unanimous`. A policy that intends its voting algorithm to be binding
   therefore MUST set `commitment.require_vote_quorum` to `true`."* Restated at `:211`, mirrored in
   `docs/policy.md:99`, justified for all three `policy.std.*` profiles at `defaults.rs:197`, and
   **pinned by a test** — `evaluator.rs:2251` `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum`.
   Fourteen places document it. The issue's claim "no test covers it" is false.
   **Decision (user, 2026-09-10): spec issue first; the runtime fix does NOT ship in this plan.**
2. **The runtime performs NO JSON-Schema validation at all**, despite `CLAUDE.md` claiming
   "Policy rules are validated against mode-specific JSON schemas at registration time" and listing
   `schemas/json/policy/*.schema.json` under Key files. **There is no `schemas/` directory in this
   repo.** `crates/macp-policy` depends only on `macp-core`, `serde`, `serde_json`, `tokio`;
   `registry.rs:255-275` does serde deserialization plus three hand-written checks and nothing else.
   This is the actual root cause of most of the cluster, and it is the plan's central lever.
3. **The canonical schemas already forbid most of what the issues ask us to forbid.**
   `quorum-rules.schema.json` declares `threshold.value` as `{"type":"integer","minimum":0}` — so
   #145's fractional `0.5` is schema-invalid to begin with. `decision-rules.schema.json` declares
   `voting.weights.additionalProperties = {"type":"number","minimum":0}` — so #148's negative weights
   are schema-invalid, and `voting.algorithm` carries a six-value `enum`. Enforcing the schema is
   therefore *conformance work*, not new policy.
4. **Three residual defects are schema-LEGAL and need a spec change, not just enforcement:**
   `voting.threshold: 0.0` (schema `minimum: 0` is inclusive), all-zero `weights`
   (`minimum: 0` inclusive → `weighted_total == 0.0`), and #147's `NoVotes` semantics. These go
   upstream as one issue and are **out of implementation scope here**.
5. **PR #153 is not a criterion-0.8 incompatibility.** The `Check (MSRV)` job fails at
   `ci.yml:93-94` (`cargo metadata --locked`) *before* `cargo check` ever runs, on a pure
   manifest/lock inconsistency. `benches/replay_bench.rs` imports no `black_box`, criterion 0.8.2's
   MSRV is 1.86 against our 1.89.0, and criterion is **not** in `integration_tests/Cargo.lock`
   (it is a root-package dev-dep, and `integration_tests` takes `macp-runtime` as a non-member path
   dependency, so cargo omits its dev-deps). One manifest line fixes it.
6. **`watch_sessions` has no unbounded channel.** There is no channel on the snapshot path at all —
   the response is a lazily-polled `async_stream::try_stream!`, already backpressured by HTTP/2.
   The unbounded resource is `registry.get_all_sessions()` (`registry.rs:263-274`), which deep-clones
   every `Session` into a `Vec` held resident for the whole sync, gated on client read speed, times
   `MACP_MAX_CONCURRENT_STREAMS` (default 128). `docs/API.md:160`'s "bounded capacity" sentence
   describes only the *live* broadcast path and does not cover the snapshot.

**Scope decision.** The work splits into four independently releasable groups. Group 2 is what an
external consumer (`zer07labs/seam-runtime`, pinning `macp-policy =0.6.0` and carrying documented
"delete when this lands" workarounds) is actively blocked on; it reaches crates.io first and alone.

---

## Release groups

| Group | Phases | Ships as | Gated on |
|---|---|---|---|
| **G1 — unblock CI** | 1 | merge of PR #153, no release | — |
| **G2 — policy & quorum correctness** | 2–6 | its own release (0.8.0) | — |
| **G3 — watch_sessions bound** | 7–8 | its own release | G2 released (avoid mixing behaviour deltas) |
| **G4 — handoff implicit accept** | 9–13 | its own release, wire-visible | G3 merged |
| **G5 — tracked-file hygiene** | 14 | rides G2's PR | — |
| **BLOCKED — #147 + degenerate thresholds** | — | not in this plan | upstream RFC-MACP-0012 §4.1 amendment |

---

## Phases

### Phase 1 — criterion manifest bump (G1)

- **Status:** DONE — `c0a2250` on `dependabot/cargo/major-updates-159dc18219`, PR #153, CI running.
  No divergence from the planned approach. Bench compiles against criterion 0.8.2 with zero warnings,
  verified locally before push (CI had never reached the compile step).
- **Delivers:** PR #153 green and merged; `main` lock resolvable under `--locked`.
- **Depends on:** nothing.
- **Files:** `Cargo.toml` (line 115) on branch `dependabot/cargo/major-updates-159dc18219`.
- **Approach:** `criterion = "0.5"` → `"0.8"`, pushed to the dependabot branch (not a new branch — the
  lock change already lives there and auto-merge is enabled). **Locally run `cargo check --all-targets`
  before pushing**: CI has never compiled the bench against 0.8.2 because the `--locked` guard fails
  first, so CI green is not evidence of compatibility. Rejected: closing #153 and hand-authoring the
  bump, which discards dependabot's lock resolution and re-opens next week.
- **Edge cases:** `alloca 0.4.0` enters the tree with a `cc` build-dep (C compiler; present on
  `ubuntu-latest`). If `cargo check` fails locally, STOP and report — do not edit the bench to chase
  a green build in this phase.
- **Acceptance criteria:**
  1. `Cargo.toml:115` reads `criterion = "0.8"`.
  2. `RUSTC_WRAPPER="" cargo metadata --locked --format-version 1` exits 0.
  3. `RUSTC_WRAPPER="" cargo check --all-targets` exits 0 with no edits to `benches/`.
  4. `integration_tests/Cargo.lock` is **unchanged** (criterion is absent from it — verify with
     `grep -c '^name = "criterion"' integration_tests/Cargo.lock` → 0).
  5. All 12 required contexts green on #153; PR merged.
- **Tests:** none new — `benches/replay_bench.rs` compiling under `--all-targets` is the test.
- **Docs:** none.

### Phase 2 — registration-time schema conformance (G2)

- **Status:** DONE — `7f55ccb` + `46de083` on `feat/policy-schema-conformance`, 2 verify rounds
  (GAPS 0-BLOCKER/5-SHOULD-FIX → PASS). Divergences from plan, all recorded in PROGRESS: criterion 9
  was not implementable as written (built-ins bypass `register` entirely, so the stated startup-crash
  hazard does not exist); criterion 11 rested on a false premise (`75.0` IS a legal JSON Schema
  integer); `crates/macp-core/src/policy/rules.rs` was listed as a deliverable but correctly went
  untouched — the doc comments landed on the validators instead. Required new public API
  (`validate_dir`, `PolicyFileOutcome`) the plan did not anticipate. A reachable `mode: "*"` fail-open
  is pinned, not closed — **Phase 3 owns it**.
- **Delivers:** `RegisterPolicy` and `MACP_POLICIES_DIR` loading refuse every policy the canonical JSON
  schemas already forbid, plus a validate-only mode so an operator can find out before upgrading.
  Closes the reachable half of #145 and defect 1 of #148.
- **Depends on:** nothing.
- **Files:** `crates/macp-policy/src/registry.rs`, `crates/macp-core/src/policy/rules.rs` (doc
  comments only), `src/main.rs`, `docs/policy.md`.
- **Approach:** extend `validate_conditional_constraints` (`registry.rs:278-312`) with hand-written
  validators **mirroring the canonical schemas**, each carrying a doc comment citing the exact schema
  file and constraint. Constraints to add:
  - `voting.algorithm` ∈ {`none`,`majority`,`supermajority`,`unanimous`,`weighted`,`plurality`}
    (`decision-rules.schema.json` enum).
  - `voting.threshold` ∈ [0.0, 1.0]. **Do not refuse 0.0** — schema-legal (`minimum: 0` is
    inclusive); it is deferred to spec #98.
  - every `voting.weights` value ≥ 0.0 (`minimum: 0`). **Do not require > 0** — schema-legal, same reason.
  - **`voting.quorum`** (`decision-rules.schema.json:25-41`): `type` ∈ {`count`,`percentage`},
    `value` ≥ 0. Note `evaluator.rs:293` currently accepts `"count" | "n_of_m"` and `n_of_m` is **not**
    in that enum — exactly the drift this phase exists to close.
  - quorum `threshold.value` is a non-negative **integer**; for `type: "percentage"` additionally ≤ 100.
  - quorum `threshold.type`: accept {`n_of_m`,`percentage`,`count`} and **refuse `weighted` as
    unimplemented**. **`count` is deliberately accepted even though the canonical enum omits it** —
    this runtime documents it (`docs/policy.md:142`) and both layers already treat it as an alias for
    `n_of_m` (`quorum.rs:77`, `evaluator.rs:709`). Refusing it would break documented behaviour. The
    schema gap is item 4 of spec #98. `weighted` is refused rather than silently treated as a raw
    count (its current behaviour) because the schema types `threshold.value` as `integer`, so a
    weighted sum is not even expressible — there is no defined semantics to implement.
  **Why hand-written and not a `jsonschema` crate:** adding a dependency to any workspace crate forces
  regenerating `integration_tests/Cargo.lock` in the same PR (`ci.yml:517-533`). See Long-term posture
  for the vendoring option the reverify round surfaced, which is the better end state.
- **Edge cases & failure modes:** `validate_rules_for_mode` accepts **any** JSON object for an unknown
  mode (`registry.rs:255-275` returns `Ok(())`) and no struct uses `deny_unknown_fields`. `load_from_dir`
  (`registry.rs:147`) funnels through `register` and `main.rs:241-245` propagates its error with `?`, so
  **one newly-invalid file aborts startup**. Correct (fail-closed) but an operational break: it needs
  the dry-run below and a changelog entry. Note also `server.rs:1545`: when `MACP_POLICIES_DIR` is set
  the registry is **read-only over the wire**, so the file path and the `RegisterPolicy` path are
  mutually exclusive in a deployed runtime — both must be covered. `registry.rs:33` pre-registers the
  three `policy.std.*` definitions **in the constructor**, so a validator that accidentally refuses one
  takes the process down at startup with no wire-level symptom.
- **Acceptance criteria:**
  1. `{"voting":{"algorithm":"majority","weights":{"a":-1.0}}}` is refused, naming `voting.weights`
     and the offending key.
  2. Quorum `{"threshold":{"type":"n_of_m","value":0.5}}` is refused, naming `threshold.value` and
     stating an integer is required.
  3. `{"voting":{"algorithm":"majorty"}}` is refused, naming the legal enum values.
  4. `{"voting":{"algorithm":"majority","threshold":1.5}}` is refused.
  5. `{"voting":{"quorum":{"type":"n_of_m","value":1}}}` is refused (not in the schema enum).
  6. Quorum `{"threshold":{"type":"count","value":2}}` **succeeds** — documented alias, deliberately
     kept.
  7. Quorum `{"threshold":{"type":"weighted","value":2}}` is refused as unimplemented.
  8. `{"voting":{"algorithm":"majority","threshold":0.0}}` and
     `{"voting":{"weights":{"a":0.0,"b":0.0}}}` both **succeed** — schema-legal, deferred to spec #98.
  9. **`std_policies()` still register cleanly** — a test asserts all three survive every new
     constraint. Without this, a bad validator is a startup crash with no test coverage.
  10. A validate-only mode (`MACP_POLICIES_DRY_RUN=1`) loads every file in `MACP_POLICIES_DIR`,
      reports every rejection with its filename, and exits 0/1 without starting the server.
  11. `register_valid_quorum_rules_succeeds` (`registry.rs:725`) is **edited**, not deleted — it passes
      `{"type":"percentage","value":75.0}`, a float for an integer field.
  12. Every new validator's doc comment cites its schema file and constraint by name.
- **Tests:** one refusal test per constraint; one acceptance test per schema-legal boundary
  (`threshold: 0.0`, weight `0.0`, `value: 0`, `percentage: 100`, `type: "count"`); the
  `std_policies()` survival test; a dry-run test over a fixture directory containing one bad file.
  A parity test comparing the Rust-side enum lists against the canonical schemas — **run it inside the
  `conformance-oracle` CI job**, which already has the spec repo checked out, rather than
  `#[ignore]`-ing it (an ignored test runs nowhere, CI included).
- **Docs:** `docs/policy.md:50` (registration-validation paragraph) gains the new constraints;
  **`docs/policy.md:49`'s claim that "the runtime validates the rules against the target mode's
  schema" must be corrected** — it is false today and only partly true after this phase. This is the
  *tracked* copy of the same over-claim `CLAUDE.md` carries. `docs/policy.md:142` threshold-type list
  reconciled with what is actually accepted.
### Phase 3 — unify the quorum threshold, ceil and floor-to-1 (G2)

- **Status:** DONE — `23d0ad2`, 1 verify round (PASS; the critical items were verified by the
  orchestrator directly after the verifier agent died twice to machine sleep — see PROGRESS).
  **Divergence from plan, and it was an improvement:** the plan said "make the mode's rounding match
  the evaluator's". The executor instead extracted the arithmetic into a single shared resolver
  (`QuorumThreshold::effective` + `EffectiveThreshold` in `macp-core`) that both layers now call,
  on the grounds that #145 is "one implementation derives two thresholds" and patching one copy to
  agree with the other re-creates the precondition for the next drift. Parity is now true by
  construction rather than by assertion. **The plan's criterion-6 premise was also wrong** — see the
  Approach note below. `cargo semver-checks` run: exit 0, purely additive. Phase 5 was NOT
  pre-empted; `effective_threshold` is still private.
- **Delivers:** one threshold interpretation across mode and evaluator; `T = 0` unreachable. Closes #145.
- **Depends on:** Phase 2 (registration now blocks the fractional input; this is defense in depth for
  directly-constructed `PolicyDefinition`s, which is how every mode/evaluator unit test builds one).
- **Files (as built — wider than first planned):** `crates/macp-core/src/policy/rules.rs` (the new
  shared resolver), `crates/macp-modes/src/mode/quorum.rs`, `crates/macp-policy/src/evaluator.rs`,
  `crates/macp-policy/src/registry.rs` (the wildcard fail-open Phase 2 deferred here),
  `docs/policy.md`, `ASSUMPTIONS.md`. The first three were planned; the registry and `macp-core` edits
  are the justified scope expansion the shared-resolver approach required.
- **Approach:** in `QuorumMode::effective_threshold` (`quorum.rs:67-83`), replace
  `_ => rules.threshold.value as u32` with a `.ceil()` matching `evaluator.rs:709`, then clamp the
  whole result to a minimum of 1. Handle `"weighted"` explicitly rather than letting it fall through
  the `_` arm as a raw approval count (a third defect neither issue raises). Delete or correct the
  doc comment at `evaluator.rs:695-697`, which asserts the two layers already agree — it is false in
  ten distinct ways and is the reason the divergence went unnoticed.
  **User decision, settled, do not re-litigate:** ceil is normative and the floor is 1. The
  reject-at-bind-time alternative was considered and not chosen.
- **Edge cases:** `percentage` with `participants.is_empty()` is unreachable (`quorum.rs:119-121`
  rejects it) but the evaluator guards it anyway (`:703-705` → `usize::MAX`); keep both. The mode
  silently falls back to `request.required_approvals` on a rules parse failure
  (`unwrap_or_default()`, `:70`) where the evaluator **denies** (`:687-690`) — this divergence is
  NOT in scope to unify here; record it in `ASSUMPTIONS.md` and leave it.
- **Acceptance criteria:**
  1. For `{"type":"n_of_m","value":0.5}` on a 3-participant session, `effective_threshold` returns 1,
     equal to what `evaluate_quorum_commitment_outcome` computes for the same rules.
  2. A zero-ballot **negative** commitment on such a session is refused (today it seals).
  3. `{"type":"weighted"}` no longer resolves via the `_` arm; its behaviour is explicit and tested.
  4. A differential test asserts mode and evaluator agree across a matrix of
     `{type} × {value} × {participant count}` — at minimum the 3×5×3 grid covering fractional,
     integral, and over-participant values. **`value = 0` is explicitly carved out**: both layers gate
     on `value > 0.0` (`quorum.rs:71`, `evaluator.rs:698`) and diverge *outside* that gate — the mode
     falls back to `request.required_approvals` (`quorum.rs:82`) while the evaluator applies no
     threshold at all. The ceil+floor fix lives inside the gate and does not touch this. Record the
     residual divergence in `ASSUMPTIONS.md` beside the parse-failure one.
  6. **The decline hole is closed or explicitly deferred.** *(Post-execution correction: this
     criterion's premise was half wrong. RFC-MACP-0011 §4a's own formula —
     `(remaining_eligible + current_approvals) < required_approvals` — is **literally satisfied at
     zero ballots** when `required > participants`, so the zero-ballot decline is not an RFC
     violation and the "decline gate mirroring decision-mode's reject-floor" suggested below would
     have been the wrong fix, and a naive one would have broken legitimate §4b declines. The actual
     defect is §6: the policy `threshold` **replaces** `required_approvals`, a field the mode already
     constrains to `1..=participants` (`quorum.rs:144`), and nothing held the replacement to that
     domain. Fixed at ApprovalRequest time — the root, not the symptom.)* With an over-participant threshold
     (`value: 5` on 3 participants) `commitment_ready` still fires via the "mathematically
     unreachable" branch (`quorum.rs:100`, `0+3 < 5`) and `evaluator.rs:717-720` allows the decline
     unconditionally — so a zero-ballot negative commitment still seals. Either add a decline gate
     mirroring decision-mode's reject-floor, or move this to the Blocked table with a reason. Do not
     leave it silent.
  5. `evaluator.rs:695-697` no longer claims parity it does not have.
- **Tests:** the differential matrix above; a zero-approval negative-commitment refusal test. Note
  **zero existing quorum tests break** — every policy-bearing test uses an integral value ≥ 1 and
  every non-policy test hits the `1..=participants.len()`-constrained fallback. This phase is
  additive test work.
- **Docs:** `docs/policy.md:143` (threshold types).

### Phase 4 — the negative-weight evaluator hole (G2)

- **Status:** DONE — `3d73258` (purely additive: 270 insertions, 0 deletions). Phase 3's doc gaps
  closed alongside in `72c9e2c`. 734 tests passing (729 + 5). Every new test mutation-checked.
  **The plan's DIAGNOSIS of this phase was wrong in two compounding ways, though the fix it
  prescribed was right** — see the correction block below. Criterion 5's safety rails
  (`evaluator.rs` `all_abstain_returns_no_votes`, `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum`)
  confirmed **byte-identical** to `23d0ad2` by extracting both bodies from the old blob and diffing,
  and green. Mutation testing independently confirmed criterion 5's premise: removing the
  front-of-dispatch short-circuit reddens the guard test *and* both protected §4.1 tests.

- **CORRECTION, recorded after execution (the plan was wrong, twice):**
  1. **`{"a": 1.0, "b": -1.0}` — the example this plan used throughout — sums to exactly `0.0`**, which
     is the *deferred* case, not the one being fixed. `compute_weighted_votes` totals the cast
     APPROVE/REJECT weights, so with both agents voting that map yields `weighted_total == 0.0`.
     Acceptance criteria 1-3 as literally written would have tested the zero branch and asserted
     behaviour this phase explicitly declines to change. The executor substituted
     `{fraud: 1.0, growth: -2.0}`.
  2. **A negative total never returned `NoVotes` at all.** The guard it escapes is
     `weighted_total == 0.0`, which a negative value does not match — so it fell through to
     `weighted_approve / weighted_total`, where a negative denominator **inverts**
     `ratio >= threshold`. The real defect was therefore a silently inverted comparison that could
     report **`Passed`** — strictly more severe than the "reports no votes" this plan described,
     because it *allowed a positive commitment over a reject from the only in-schema voter*. Issue
     #148 described this correctly; this plan mis-framed it by conflating #148's two separate defects.
     Consequently the fail-open analysis below is right about the `Failed`-vs-`NoVotes` mapping but
     attributes the DENY→ALLOW decline move to the wrong source variant: it comes from `Passed`.
  3. Practical consequence worth keeping: `PolicyDecision`-only assertions would have **passed against
     the unfixed code** for two of the three negative criteria, because those rounds were already
     denied — for the wrong reason. Each negative test therefore asserts the `VotingResult` variant
     directly through `check_voting_algorithm`. Without that, criteria 1 and 3 were vacuous. This is
     the fourth instance in this repo's history of a green signal not measuring what it appeared to.
- **Delivers:** a weighted round whose weights sum negative can no longer report "no votes". Closes
  the **out-of-schema** half of #148 defect 2.
- **Depends on:** Phase 2 (which removes negative weights at the door; this is defense in depth for
  directly-constructed `PolicyDefinition`s, which is how every evaluator unit test builds one).
- **Files:** `crates/macp-policy/src/evaluator.rs`.
- **Approach:** `evaluator.rs:388-390` returns `VotingResult::NoVotes` when `weighted_total == 0.0`.
  **Change it to `< 0.0` only** — return `Failed` for a negative total, and leave the `== 0.0` case
  alone. Rationale, and this is a correction applied after the plan's reverify round:
  - A **negative** total is out-of-schema (`weights.additionalProperties.minimum: 0`), so fixing it is
    enforcing a constraint the spec already states.
  - A **zero** total is schema-legal, and is explicitly deferred to spec #98 item 3. The first draft of
    this phase shipped the zero case while the Blocked table deferred it — the plan contradicted
    itself. Deferring is the correct side: see the fail-open note below.
  **NOT in this phase:** removing the `:326-328` short-circuit, making `unanimous` fail on an empty
  tally, or adding a zero-denominator branch to `supermajority`. That last one was in the first draft
  and is **dead code** — `ratio` at `:352` can only have a zero denominator when
  `non_abstain_total == 0`, which returns at `:326` before the arm is reached. It only becomes live in
  the blocked #147 work that removes the short-circuit; it belongs there.
- **Edge cases & failure modes:** **`NoVotes → Failed` is fail-OPEN in the decline direction** —
  per `evaluator.rs:74-78` and `:233-245` (mirroring RFC-MACP-0007 §6), `NoVotes` + negative outcome is
  denied *unconditionally*, while `Failed` + negative outcome is allowed iff `reject_count > 0`. So
  `{"a":1.0,"b":-1.0}` with one APPROVE and one REJECT moves from DENY to ALLOW on a decline. This is
  a deliberate trade — the round is genuinely decided and a decline backed by an explicit reject is
  the correct outcome — but it is **not** purely a tightening and must not be described as one.
  §4.1's no-result branch is conditioned on no decisive vote having been *cast*, which is false here,
  so the spec does not address this case at all: this phase **fills a gap §4.1 does not address**,
  it does not "move toward conformance."
  The `supermajority` clamp at `:348-352` is a **silent substitution** to `2.0/3.0`, not a refusal,
  and is unreachable for registry-registered policies (`registry.rs:290` already refuses `<= 0.5`).
  Leave it; record it in `ASSUMPTIONS.md`. `unanimous` with an empty `participants` list returns
  `Passed` by vacuous truth (`:370-376`) — unreachable in practice; record, do not fix here.
- **Acceptance criteria:**
  1. `weights: {"a": 1.0, "b": -1.0}`, both voting, **positive** outcome: no longer `NoVotes`;
     `Failed`, and the commitment is denied.
  2. The same weights, **negative** outcome with `reject_count >= 1`: the decline is **allowed**, and
     this direction change is stated in the changelog.
  3. The same weights, **negative** outcome with `reject_count == 0`: still denied.
  4. `weights: {"a": 0.0, "b": 0.0}` with both voting still returns `NoVotes` — **unchanged**,
     deferred to spec #98 item 3. A test pins this so a later phase cannot close it by accident.
  5. `evaluator.rs:2251` and `evaluator.rs:1739` are **unchanged and still pass**. (Verified achievable
     by the reverify round: both return at the front-of-dispatch `NoVotes` and never reach the
     weighted arm; there are only four weighted tests in the file and none has a zero or negative
     total.) If either goes red, the phase has overreached — stop, do not edit the test.
  6. A regression test asserts the `:326-328` short-circuit is still present, guarding against a
     future phase removing it before spec #98 lands.
- **Tests:** all six criteria as named tests.
- **Docs:** `evaluator.rs:74-78`'s rustdoc table gains a negative-weighted-total row; the changelog
  states the decline-direction delta explicitly.
### Phase 5 — publish the corrected effective threshold (G2)

- **Status:** DONE — `e246db5`. 739 tests passing (734 + 5). `cargo semver-checks check-release` on
  `macp-core`, `macp-runtime` and `macp-modes`: **exit 0, 196/196 each, "no semver update required"**.
  Every new assertion mutation-checked.
  **Divergence, and the plan's option set was incomplete:** the plan weighed three signatures, all
  with exactly two outcomes. But the session-level form must decode `session.mode_state`, and that
  decode **can fail** — a third outcome none of the listed options can express (verified empirically:
  a decision-mode state and a bare `{}` both yield `Err(InvalidModeState)`, because `QuorumState`'s
  fields are NOT `#[serde(default)]`). Shipped signature is therefore
  `effective_threshold_for_session(&Session) -> Result<Option<ApprovalThreshold>, MacpError>` —
  one layer per question, no representable-but-impossible state: `Err` = undecodable state,
  `Ok(None)` = no request accepted yet, `Ok(Some(_))` = the resolved bar. A purpose-built
  `ApprovalThreshold { Approvals(u32), Unsatisfiable }` was used rather than re-exporting
  `macp-core`'s `EffectiveThreshold`, because the latter's `Inert` variant is unreachable through
  this path — an impossible state every caller would still have to match — and re-exporting it would
  widen a `macp-core` type into a second crate's public API, which Phase 3's own ASSUMPTIONS entry
  flagged. `Inert` is resolved to `Approvals(request.required_approvals)` before the caller sees it;
  surfacing it would hand back the fallback rule and re-create the 15-line mirror this phase exists
  to delete.
  **Also found:** criterion 3 does not compose with Phase 3's change. The differential matrix
  deliberately includes over-participant thresholds, and Phase 3's new ApprovalRequest guard
  *refuses* exactly those — so the session-level accessor cannot be exercised across the matrix via
  the public message path. The unit test seats `mode_state` directly, justified in-comment by the
  replay case where edited policy rules rebind to an already-accepted request.
- **Delivers:** downstream can delete its 15-line mirror. Closes #146.
- **Depends on:** **Phase 3** — what becomes public must be the corrected function, and Phase 5
  criterion 3 needs Phase 3's differential matrix to exist. Keep the order for bisectability; the
  "must never be reordered" framing in the first draft was overstated, since phases 2–6 all ship in
  one release and no consumer observes the intermediate state.
- **Files:** `crates/macp-modes/src/mode/quorum.rs`, `docs/modes.md`.
- **Approach:** expose `effective_threshold_for_session(session: &Session) -> Option<u32>` as the
  public entry point rather than only flipping `effective_threshold` to `pub`. The private function
  takes an `&ApprovalRequestRecord`, which an external caller can only obtain by decoding
  `session.mode_state` (possible — `macp_modes::mode::util::decode_mode_state` is `pub` at
  `util.rs:166` — but it makes the caller reconstruct internals). The session-level wrapper is what
  the downstream issue actually asks for. Keep the inner fn `pub` too so the request-level form is
  available.
- **Edge cases:** document in the rustdoc that `commitment_ready` is **non-monotonic** in the
  approval count. *(Plan correction after Phase 3: the predicate is no longer
  `approvals >= required || approvals + remaining < required` — Phase 3 added a `counted > 0` guard
  to the second disjunct (`quorum.rs:132`). Document the CURRENT form, not the one quoted here.)*
  Non-monotonicity means — a binary search over it returns a confident wrong answer. The downstream reporter
  hit exactly this.
- **Acceptance criteria:**
  1. `macp_runtime::mode::quorum::QuorumMode::effective_threshold_for_session` is callable from an
     integration test using only public API, with no `decode_mode_state` call.
  2. It returns `None` for a session with no accepted `ApprovalRequest`.
  3. Its value equals the evaluator's `required` for the same session across the Phase 3 matrix.
  4. `cargo doc` renders the non-monotonicity warning.
  5. `cargo-semver-checks` does not flag it (purely additive). *(Plan correction after Phase 3:
     one new type — `macp_core::policy::rules::EffectiveThreshold` — did become public in Phase 3.
     The `Option<u32>` return on the session-level wrapper avoids widening that further, so this
     criterion still holds, but the original "no new type needs to become public" parenthetical is
     no longer true. `ApprovalRequestRecord` and `QuorumState` were already `pub`.)*
- **Tests:** a `tests/` integration test that imports through the public path only.
- **Docs:** `docs/modes.md` quorum section.

### Phase 6 — G2 docs, changelog, and release

- **Status:** DONE (code+docs) — `0b4bfc5`. Criteria 2 and 3 (release-PR lockstep, seven crates on
  crates.io) are post-merge and tracked in PROGRESS, not here. **Criterion 4 was corrected:** it said
  to close #145/#146/#148, but #148's reported defect 2 is the schema-legal zero-weight case deferred
  to spec #98 — closing it would advertise as fixed the precise scenario reported, so **#148 stays
  open with a comment**. Divergence: `CHANGELOG.md` is release-plz-generated from commit subjects and
  cannot carry the operational detail, so the upgrade guidance went to `docs/deployment.md:17-56`
  instead of the changelog (rewriting five final commits to force it in would require a force-push).
  Also found: this phase's `CLAUDE.md` work was only half-done by Phase 2 — the stale `v0.5.0` header
  and a `schemas/json/policy/` path that does not exist in this repo were both still live. Because
  `CLAUDE.md` is gitignored, a "done" claim about it can never be checked against a diff, which is how
  it survived.
- **Delivers:** G2 released to crates.io; the five issues' runtime-side work closed or explicitly deferred.
- **Depends on:** Phases 2–5.
- **Files:** `docs/policy.md`, `CHANGELOG.md` (via release-plz), `CLAUDE.md` (local only), issue comments.
- **Approach:** correct `CLAUDE.md`'s two false claims (no `schemas/` directory here; no JSON-Schema
  validation at registration) and its stale `v0.5.0` header. **`CLAUDE.md` is gitignored
  (`.gitignore:20`, per `DECISIONS.md` D6) so this edit will NOT appear in any PR diff** — state that
  in the phase report rather than letting it look skipped. Comment on #145/#146/#148 with the fix, and
  on #147/#149 explaining the upstream dependency and linking the spec issue.
- **Acceptance criteria:**
  1. `docs/policy.md` documents every constraint Phase 2 added.
  2. The release PR's `deps-isolation` lockstep step passes (all seven versions equal).
  3. All seven crates appear on crates.io at the new version.
  4. #145, #146, #148 closed; #147, #149 carry a comment naming the blocking spec issue.
- **Docs:** as above.

### Phase 7 — bound the `watch_sessions` snapshot (G3)

- **Status:** TODO
- **Delivers:** the initial sync holds one `Session` resident at a time instead of deep-cloning the
  whole registry.
- **Depends on:** Phase 6 merged. (The plan originally gated this on G2 being *released*; that was a
  changelog concern, not a technical one — `watch_sessions` and the policy path share no code. Gating
  on merge is enough; G3 may ride the same release with its own CHANGELOG entry.)
- **Files:** `src/server.rs`, `crates/macp-storage/src/registry.rs`, `docs/API.md`.
- **Approach — take the id list ONCE, then materialize one session at a time.** Replace
  `registry.get_all_sessions()` (`server.rs:1380`) with a single pass that collects the session **ids**
  (`session_ids_after(None, usize::MAX)`, or a new `session_ids()`), then loop `get_session(id)`
  one at a time, yielding as we go.
  **Do NOT page with repeated `session_ids_after` calls.** `SessionRegistry` is a
  `RwLock<HashMap>` with no ordered index (`registry.rs:151`), and `session_ids_after`
  (`:294-321`) scans **every key in the map on every call**, keeping a k-sized max-heap. That is fine
  for `list_sessions` (one call per RPC) but inside `watch_sessions` it becomes ⌈N/k⌉ full map scans
  for one snapshot — O(N²/k), trading a memory bound for a CPU one, and lengthening exactly the window
  that causes the `Lagged` kill below. The single-pass form is O(1) resident `Session` clones with one
  map pass and no keyset staleness. The N `String` ids are already the status quo: `synced`
  (`server.rs:1381`) holds precisely that set for the stream's lifetime regardless.
  **No new env var is needed** — this is the plan's revised position. The original design mirrored
  `list_sessions`' page-size config, but with one-at-a-time materialization there is no page size to
  tune. This deletes the whole Phase 8 env-binding surface (see Phase 8).
  **Two hard constraints that do not change:**
  (a) `let mut rx = self.runtime.subscribe_session_lifecycle();` at `server.rs:1372` MUST stay above
      the `try_stream!` at `:1374`, or the missed-event race its comment guards against returns.
  (b) The snapshot loop never calls `rx.recv()`. The bus has capacity 64 (`runtime.rs:94`); >64 events
      during a slow snapshot make the first subsequent `recv()` return `Lagged`, which `:1398-1402`
      turns into `RESOURCE_EXHAUSTED`. Interleave draining `rx` into a **bounded** pending buffer —
      the first draft said "a pending buffer" without a bound, which would have traded the O(N)
      `Session` bound for an unbounded O(events) one. On overflow, surface the existing
      `RESOURCE_EXHAUSTED` rather than growing.
  Rejected: a silently-truncating emission cap — `core.proto` normatively requires the initial sync to
  carry all active sessions.
- **Edge cases & failure modes:** a single id pass is still **not** a snapshot. A session present when
  ids were taken but removed before its `get_session` runs yields `None` — and its terminal event
  arrives from the bus with no preceding `Created`, so a client can receive a lifecycle event for a
  session it never saw created (and `session: None`). The `synced` dedup at `:1422-1426` handles only
  the double-emit direction. **Decide explicitly:** drop unknown-id terminal events during the sync
  window, or document the anomaly in `docs/API.md`. `synced` is never pruned and grows one `String`
  per `Created` for the stream's lifetime — bound it or document the growth.
- **Acceptance criteria:**
  1. A registry of N sessions yields exactly N `Created` events, once each.
  2. **The traversal is extracted into a batch-yielding function and tested directly**: no batch
     exceeds the bound, and only one batch is live at a time. A registry *call count* alone does not
     prove residency and there is no seam to instrument — `SessionRegistry` is a concrete struct with
     no trait and no counter. If a call count is also asserted, state it as what it is
     ("exactly 1 id-listing call + N get_session calls"), not as a residency proof.
  3. With a slow consumer and >64 concurrent lifecycle events, the stream does **not** terminate with
     `RESOURCE_EXHAUSTED` — and the pending buffer's bound is asserted, not assumed.
  4. `subscribe_session_lifecycle()` is still called before the generator is constructed.
  5. A session removed mid-traversal produces no lifecycle event with `session: None`, or the
     behaviour is documented in `docs/API.md`.
- **Tests:** the five criteria; extend
  `integration_tests/tests/tier1_protocol/test_session_lifecycle.rs:90`, today the **only** test for
  this handler.
- **Docs:** `docs/API.md:152-163` — `:160`'s "bounded capacity" sentence describes only the live
  broadcast path and reads as if it covers the snapshot; amend it.
### Phase 8 — G3 close-out (G3)

- **Status:** TODO
- **Delivers:** G3's docs and its regression coverage.
- **Depends on:** Phase 7.
- **Files:** `docs/API.md`, `integration_tests/tests/tier1_protocol/`.
- **Approach:** the original Phase 8 existed to prove an env-var binding end to end. Phase 7's revised
  design introduces **no env var**, so that work is gone. What remains is worth keeping as its own
  phase: a Tier-1 regression pinning the sync's observable contract through the real gRPC boundary.
  **If Phase 7's implementation does end up introducing a tunable after all**, restore the original
  content — the binding must be proven end to end with two *distinct* non-default values, because
  during the `list_sessions` work a verifier swapped two page-size env vars in `from_env` and the
  entire suite still passed (626 workspace, 92 Tier-1, 8 JWT, 5 Tier-2, zero failures): unit tests
  drove the name-agnostic resolver and the single `from_env` test ran with both vars unset.
- **Acceptance criteria:**
  1. A Tier-1 test with its **own** server (the shared one accumulates sessions from other tests and
     any count assertion against it is flaky) asserts exactly-once `Created` per session across a
     registry large enough to have exercised the old deep-clone path.
  2. `docs/API.md`'s `WatchSessions` section states the post-Phase-7 contract, including the
     reconcile-via-`ListSessions` guidance and whatever Phase 7 criterion 5 settled.
  3. If a tunable exists, the swap test from the Approach above is present and demonstrated red by
     actually swapping, running, and reverting.
- **Tests:** as above.
### Phase 9 — rev-2 scaffolding, behaviour-neutral (G4)

- **Status:** TODO
- **Delivers:** `CURRENT_SEMANTICS_REV = 2` with a `>= 2` branch identical to `>= 1`; the
  suspended-at-offer field on `HandoffOfferRecord`. **Zero behaviour change.**
- **Depends on:** Phase 8 merged.
- **Files:** `crates/macp-core/src/session.rs`, `crates/macp-modes/src/mode/handoff.rs`, and 17
  `LogEntry` literal sites (`src/runtime.rs:260,285,1049`, `log_store.rs:169`, `compaction.rs:38,93`,
  `file.rs:214`, `migration.rs:190`, `recovery.rs:83`, `redis_backend.rs:200`, `rocksdb.rs:293`,
  `src/replay.rs:385,404,612,692`, `tests/replay_round_trip.rs:50`, `benches/replay_bench.rs:58`).
- **Approach:** follow the rev-1 pattern exactly, derived from `5d9fb5e`: bump the const and **add a
  `- 2 —` bullet to its doc comment** (`session.rs:18-28` — that bullet list is the only place
  revisions are documented); `#[serde(default)]` on any new persisted field; no `schema_version` bump
  (rev 1 judged serde-default sufficient, `registry.rs:54-56`); preserve old behaviour verbatim in
  each `else`. Add the suspended-at-offer snapshot field to `HandoffOfferRecord` (`handoff.rs:23-36`)
  mirroring the `offered_at_ms` precedent.
  **This phase is separated precisely because it is high-line-count and zero-risk** — bundling it with
  the semantics work would make a regression un-bisectable.
- **Edge cases:** **no test changes are expected.** The first draft claimed three handoff tests break
  at rev 2; the reverify round disproved it — `handoff.rs:1247` and `:1314` assert
  `semantics_rev >= 1` (true at 2), `:1194` uses `on_message` which has no rev branch, and `:1283`
  sets rev 0 explicitly. Both live rev branches (`:137`, `:201`) are `>= 1` and there is **no
  `== 1` or `< 1` comparison anywhere in the repo**. If any test goes red, the branch structure was
  *restructured* rather than extended — stop and re-read, do not patch the test.
  One real hazard the first draft missed: adding a `#[serde(default)]` field to `HandoffOfferRecord`
  changes `mode_state` bytes, and `tests/conformance/handoff_reject_paths.json:62` carries an
  `expected_mode_state`. It is **safe** — `conformance_loader.rs:516` uses `assert_json_contains`
  (subset), not equality — but the executor will see the diff and should know it is expected.
- **Acceptance criteria:**
  1. `CURRENT_SEMANTICS_REV == 2` and its doc comment enumerates revision 2.
  2. A legacy-log fixture proves a rev-0 and a rev-1 history still replay to their original outcomes
     (`CONTRIBUTING.md:36-40` requires this for any persisted-semantics change).
  3. `src/replay.rs:776` `replay_preserves_recorded_semantics_rev` still passes.
  4. `RUSTC_WRAPPER="" cargo test --workspace --all-targets` fully green with **no** behavioural test
     changes beyond the three rev-pinning edits.
- **Tests:** the legacy-log fixtures; the three edited handoff tests.

### Phase 10 — suspension-correct timing, still lazy-only (G4)

- **Status:** TODO
- **Delivers:** the implicit-accept deadline stops counting suspended time. **This fixes a live
  defect, not just a gap in the new feature.**
- **Depends on:** Phase 9.
- **Files:** `crates/macp-modes/src/mode/handoff.rs`.
- **Approach:** `handoff.rs:298` is a raw `(now_ms - offer.offered_at_ms) >= timeout` with no
  suspension term, so suspending a handoff session past the timeout and resuming auto-accepts on time
  RFC-MACP-0010 §5.1(1) says must not count. Under rev 2 compute
  `(clock - offered_at_ms) - (session.accumulated_suspended_ms - snapshot_at_offer)`. Both inputs are
  on the recorded timeline (`session.rs:81-86`; replay replays suspend/resume at `replay.rs:145-162`
  from recorded `received_at_ms`), so this is replay-deterministic. `Session` is already passed to
  `on_message_at` — **no new `MessageContext` field is needed.**
- **Edge cases:** `outcome_reason` is the string `"implicit accept (timeout)"` (`handoff.rs:302`) and
  lives in serialized `mode_state` — **changing that string breaks byte-exact replay of rev-1
  histories**, which `tests/conformance_loader.rs:356-386 assert_replay_equivalence` compares
  byte-for-byte. Do not touch it in this phase.
- **Acceptance criteria:**
  1. Under rev 2, an offer suspended for longer than its timeout does **not** implicitly accept.
  2. Under rev 0 and rev 1 the old arithmetic is preserved exactly.
  3. `assert_replay_equivalence` passes for a rev-1 history containing an implicit accept.
- **Tests:** a suspend-past-timeout-then-resume case at rev 2 and at rev 1, asserting opposite outcomes.

### Phase 11 — synthesize the accept into accepted history (G4)

- **Status:** TODO
- **Delivers:** the real RFC-MACP-0010 §5.1 behaviour, lazily.
- **Depends on:** Phase 10.
- **Files:** `crates/macp-modes/src/mode/handoff.rs`, `src/runtime.rs`, `src/replay.rs`,
  `src/server.rs`.
- **Approach:** **this is a new mechanism, not an extension of one.** All five callers of
  `make_internal_entry` (`runtime.rs:312,816,875,933,1106`) are `EntryKind::Internal`,
  `sender: "_runtime"`, `message_id: String::new()`, not mode-dispatched on replay, not published to
  subscribers, not counted in accepted ordinals. The synthetic accept is the inverse on all six axes.
  §5.1(2)'s controlling words are "append a synthetic `HandoffAccept` envelope to the session's
  **accepted history**"; the spec has no opinion on any internal variant. **Correction from the
  reverify round:** the first draft argued the `SessionSuspend`/`Resume`/`Cancel` analogy "does not
  carry because in this runtime those are Internal." That inverts the logic — RFC-MACP-0001 §7.5 says
  those envelopes *enter the accepted history*, so this runtime emitting them as `EntryKind::Internal`
  (`log_store.rs:128` filters accepted ordinals to `Incoming` only) is a **pre-existing
  non-conformance**, not a constraint the new work must respect. The conclusion (`Incoming`) still
  holds — because in this runtime accepted history *means* `Incoming` — but it rests on that fact, not
  on the analogy. **Resolving Incoming vs. Internal is the single most important decision in this
  phase** and must be settled by the phase's Opus verifier before code lands. The §7.5 gap goes on the
  follow-on list, not into this phase.
  **The envelope timestamp is the computed deadline, not the observation time.** RFC-MACP-0010
  §5.1(3) says **SHOULD**, not MUST — the first draft of this plan mis-cited it. We adopt it as a
  *local* MUST anyway, on its own engineering merit, which is load-bearing: with the deadline, eager and lazy emission produce
  byte-identical entries and the entry is a pure function of prior log content. Set both
  `timestamp_unix_ms` and `received_at_ms` to the deadline.
- **Edge cases & failure modes:**
  - **`message_id` squatting.** `server.rs:118` is the only `message_id` validation (non-empty). A
    client cannot get `implicit: true` through (`handoff.rs:240`), but it **can** send a valid
    `HandoffContext` (allowed at any disposition, `handoff.rs:210-231`) with
    `message_id = "implicit-accept:<handoff_id>"`. That consumes the dedup slot, the runtime's later
    synthesized id is a duplicate, the accept never enters history, and **the session can never
    commit.** Reserve the prefix at `server.rs:118`, rev-gated.
  - If the entry is `Incoming`, replay re-dispatches it into `handoff.rs:233-242`, which **rejects
    `implicit: true`**. A discriminator surviving serde round-trip on every backend is mandatory.
  - `EntryKind::Internal` entries are never mode-dispatched on replay and `replay.rs:162`'s `_ => {}`
    **silently ignores unknown Internal types** — an old binary replaying a new log diverges with no
    error.
  - `publish_accepted_envelope` (`runtime.rs:229-233`) has two callers, both with the client's `env`.
    A synthetic entry needs explicit plumbing, **called while holding the session mutex** — see the
    FIFO comment at `runtime.rs:586-590`; StreamSession's subscribe-window dedupe
    (`server.rs:560-562,600,629-634,694`) keys on `message_id` and depends on publish order equalling
    acceptance order.
  - `validate_replay_consistency` (`replay.rs:172-215`) compares state, dedup **count**, participants
    and bound versions — **not `mode_state`**. A rev-1 session replayed by a rev-2 binary can diverge
    invisibly in production. Consider closing that hole in this phase.
- **Acceptance criteria:**
  1. After the deadline, the next session-scoped message causes a `HandoffAccept` with
     `implicit: true`, `sender == target_participant`, `accepted_by == target`, and
     `message_id == "implicit-accept:<handoff_id>"` to appear in accepted history **before** the
     triggering message is evaluated.
  2. `timestamp_unix_ms == received_at_ms == the computed deadline`, not the observation time.
  3. Replaying that history reproduces the entry byte-identically —
     `assert_replay_equivalence` green, including set-exact `seen_message_ids`.
  4. A client that pre-sends `message_id = "implicit-accept:h1"` is refused, and the session still
     commits normally.
  5. A client-submitted accept with `implicit: true` is still rejected (`handoff.rs:653` still passes).
  6. An explicit accept or decline landing before the deadline settles the offer and the synthetic
     accept is never emitted (§5.1(4)).
  7. StreamSession subscribers observe the synthetic envelope in acceptance order.
- **Tests:** all seven criteria, each as a named test; plus a replay round-trip over a history
  containing one synthetic accept; plus a mutation test proving criterion 3 fails if the timestamp is
  set to observation time.

### Phase 12 — eager sweep (G4)

- **Status:** TODO
- **Delivers:** the deadline is observed without waiting for the next message.
- **Depends on:** Phase 11. **Safe only after Phase 11 pins the entry timestamp to the computed
  deadline** — that is what makes eager and lazy byte-identical.
- **Files:** `src/main.rs`, `src/runtime.rs`.
- **Approach:** hook into the existing cleanup loop at `main.rs:464-491` as a fourth call alongside
  `cleanup_expired_sessions` / `evict_stale_sessions` / `gc_disk_sessions`. Copy the locking shape of
  `runtime.rs:1086-1130`: snapshot `(id, Arc)` under a brief map read (`:1093-1099`), never hold the
  map lock across per-session locks or I/O. Do **not** reuse `cleanup_expired_sessions` — different
  predicate, and this one must publish to `stream_bus`.
- **Edge cases:** must skip `Suspended` sessions explicitly (the lazy path cannot reach them —
  `step.rs:56-58` returns `SessionNotOpen`); `runtime.rs:1101` already filters `state != Open`, follow
  it. A session resolving between sweep and append must not produce an orphan entry.
  `MACP_CLEANUP_INTERVAL_SECS` becomes the implicit-accept latency bound. It is **already documented**
  in `docs/deployment.md:43` and `docs/architecture.md:211` (the first draft said otherwise); it is
  missing only from `README.md` and `docs/API.md`.
- **Acceptance criteria:**
  1. With no further messages, an offer past its deadline is accepted within one cleanup interval.
  2. The entry the sweep writes is **byte-identical** to the one the lazy path would have written —
     proven by running both paths over the same fixture and comparing.
  3. Suspended sessions are skipped.
  4. `MACP_CLEANUP_INTERVAL_SECS` is documented in all three tracked env tables.
- **Tests:** a sweep test on a scratch port (50123), killing only the PID started.

### Phase 13 — G4 docs and close-out

- **Status:** TODO
- **Delivers:** docs, changelog, `follow_ons.md` item 1 closed, release cut.
- **Depends on:** Phase 12.
- **Files:** `docs/modes.md`, `docs/API.md`, `plans/defer/follow_ons.md`, `CLAUDE.md` (local only).
- **Acceptance criteria:** the wire-visible change is stated in the changelog as a deliberate
  semantics change gated on `semantics_rev = 2`, not as a bugfix; `follow_ons.md:10-20` item 1 is
  marked done.

### Phase 14 — tracked-file hygiene (G5, rides G2's PR)

- **Status:** DONE — `072d159`. Four stale records corrected, each verified against the code first:
  `follow_ons.md` items 7 and 8, the ballot-cardinality cross-repo plan's S1/S2/S3 statuses (now
  MERGED with shas), and the py-sdk plan's non-integer-threshold claim (false when written, true only
  as of this plan's Phase 2). `plans/defer/README.md`'s index updated to match.
- **Delivers:** three stale records corrected.
- **Depends on:** nothing.
- **Files:** `plans/defer/follow_ons.md`,
  `plans/cross-repo/multiagentcoordinationprotocol-ballot-cardinality-and-fixtures.md`.
- **Approach:** `follow_ons.md` item 7 (built-in recommended policies) **already shipped** —
  `policy.std.majority/supermajority/unanimous` exist at `defaults.rs:32,127` with reserved-namespace
  enforcement. Item 8's "Tier-1 has no suspend/resume RPC coverage" **is also closed** —
  `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` has three tests. Mark both. The
  cross-repo ballot-cardinality plan still reads `Status: TODO — blocked on owner ratification` for
  S1/S2/S3 while `plans/conformance-cardinality-program.md`'s status table records all three as
  MERGED; reconcile to the program file, which is correct. Also correct
  `plans/cross-repo/macp-sdk-python-examples-docs-and-release.md:262`, which claims the runtime
  rejects a non-integer quorum threshold at `RegisterPolicy` — it does not today, and will only after
  Phase 2.
- **Acceptance criteria:** no tracked plan file asserts a state contradicted by the code at HEAD.

---

## Blocked — not implemented by this plan

| Item | Blocked on | Why |
|---|---|---|
| **#147** NoVotes short-circuit | upstream RFC-MACP-0012 §4.1 amendment | The behaviour is normative (`:113`, `:211`), mirrored in `docs/policy.md:99`, load-bearing for all three `policy.std.*` profiles, and pinned by `evaluator.rs:2251`. Per `CLAUDE.md`, the spec repo is normative and the runtime follows it. **User decision 2026-09-10: spec issue first.** |
| **#149** `threshold: 0.0` | same | `decision-rules.schema.json` declares `minimum: 0` **inclusive**. Refusing `0.0` makes the runtime stricter than the spec. |
| all-zero `weights` | same | `additionalProperties.minimum: 0` inclusive; `sum == 0` is schema-legal and yields `weighted_total == 0.0`. |

All three went upstream as **one** issue — **spec #98**, https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/98, filed 2026-09-10.
Cross-repo plan: `plans/cross-repo/multiagentcoordinationprotocol-voting-degenerate-values.md`.
**Authorization in force (user, 2026-09-10): issues only in the spec repo. No PRs, no commits, no
file edits there.**

---

## Long-term posture

- **The missing schema-validation layer is the debt.** `CLAUDE.md` has claimed for several releases
  that registration validates against JSON Schema. It does not, and three of five issues in this
  backlog are direct consequences. Phase 2 mirrors the schemas by hand, which closes the immediate
  holes but leaves a **drift surface**: the canonical schema can change upstream without this repo
  noticing. The parity test mitigates but does not remove it.
  **The reverify round surfaced a third option that beats both of the ones originally weighed:**
  vendor the five schema files into `macp-policy` and extend the **existing** conformance byte-diff
  oracle to cover them. `ci.yml:555-629` already checks out the spec repo and already has a generic
  bidirectional `check_dir()` with MISSING/DRIFT/EXTRA reporting — covering `schemas/json/policy/` is
  *one more `check_dir` call in a job that already runs on every PR*. That defuses the
  "three Makefiles and a CI job" objection, because the mechanism is already built. Zero new
  dependencies, zero lockfile churn, and it closes the drift surface entirely. **Deferred out of this
  plan only because it is additive to Phase 2 rather than a precondition for it** — Phase 2's
  hand-written validators are what actually close the fail-opens, and the oracle is what keeps them
  honest afterwards. It should be the next piece of work after G2 releases. The real fix is vendoring the schemas
  with a CI byte-diff oracle, exactly as `tests/conformance/` already does — deferred here because it
  creates a fourth cross-repo vendored copy, which `plans/conformance-cardinality-program.md`
  explicitly warns against without a better sync mechanism than "three Makefiles and a CI job."
- **One-way doors in this plan:** Phase 11's `Incoming` vs. `Internal` choice for the synthetic entry
  (it determines the on-disk shape of every future implicit accept and cannot be changed without
  another `semantics_rev`), and the `message_id` prefix reservation at `server.rs:118` (a wire-visible
  input restriction). Both are inside G4 and both are gated on rev 2.
- **`semver_check = true` gives zero feedback on this plan's real risk.** cargo-semver-checks runs
  only inside release-plz's release-PR computation — *after* merge — and reads rustdoc JSON, so it
  sees API surface only. Every behaviour change here is invisible to it: Phase 2's new refusals,
  Phase 3's changed threshold, Phase 4's changed `VotingResult`, Phase 7's changed event pacing,
  Phase 11's new history entry. **There is no semver job in `ci.yml` at all.**

## Enterprise concerns

- **Fail-closed on startup.** Phase 2's refusals apply to `MACP_POLICIES_DIR` loading via
  `registry.rs:147`, so a policy file that loads today can abort startup after this ships. Correct,
  but it is an operational break and belongs in the changelog, not just the diff.
- **Observability.** Phase 7's chunking and Phase 12's sweep both need a counter or a span; neither
  is observable today. `src/metrics.rs` is per-mode atomics — extend it rather than inventing a
  second mechanism.
- **Rollback.** G2 and G3 are ordinary reverts. **G4 is not:** once a rev-2 session has written a
  synthetic accept, reverting the binary leaves histories an older binary replays differently
  (`replay.rs:162` silently ignores unknown Internal types; `validate_replay_consistency` does not
  compare `mode_state`). G4's rollback story is "roll forward," and that must be stated in its PR.
- **Disk.** `target/` is 2.3G and `integration_tests/target/` 1.2G when warm; 55Gi free at planning
  time. Fourteen phases at one worktree each is ~49G if none are reclaimed. `/ship` prunes on merge —
  if it does not, phases will fail on ENOSPC around phase 10. Flagged in Open questions.

## Open questions

1. **Phase 11's `Incoming` vs. `Internal`.** Decided as Opus in favour of `Incoming` (RFC §5.1(2)'s
   controlling clause, and Internal entries are neither replayed through the mode nor published to
   subscribers), but the RFC is internally ambiguous — it also invokes the Suspend/Resume/Cancel
   analogy, which in this runtime means Internal. **The Phase 11 verifier must confirm or overturn
   this before code lands.** Logged to `ASSUMPTIONS.md` as `UNCONFIRMED` when Phase 11 starts.
2. **Disk headroom.** 14 phases × ~3.5G of build output against 55Gi free. If `/ship`'s post-merge
   prune does not reclaim, this plan does not fit. Mitigation if it bites: run G3 and G4 in a second
   session after G2 releases.
3. **Conformance fixtures for G4.** The fixture format has no way to express elapsed time or a sweep,
   so the implicit-accept behaviour may not be expressible in the corpus at all without a spec-side
   format extension. Flagged, not resolved; a spec issue if Phase 13 confirms it.
4. ~~Whether `macp_core::policy::CommitmentMode` is `#[non_exhaustive]`.~~ **Resolved** — it is
   (`crates/macp-core/src/policy/mod.rs:120`). Adding a variant field to unify the mode/evaluator
   split is therefore not a semver break, should a later phase attempt it.

---

## Plan review

**Round 1 — fresh Opus, 2026-09-10. Verdict: REVISE.** 5 BLOCKER, 9 SHOULD-FIX, 6 NICE-TO-HAVE.
All applied to this file. What changed, and why it mattered:

| # | Finding | Applied |
|---|---|---|
| B1 | Phase 4's proposed `supermajority` zero-denominator branch was **dead code** — `ratio`'s denominator can only be zero when `non_abstain_total == 0`, which returns at `:326` before the arm is reached. Its acceptance criterion was unsatisfiable. | Sub-task and criterion removed; noted as belonging to the blocked #147 work that removes the short-circuit. |
| B2 | Phase 4 shipped the all-zero-weights fix that the **same plan's** Blocked table and cross-repo item 3 deferred. The plan contradicted itself. | Phase 4 narrowed to `weighted_total < 0.0` (the out-of-schema case). The `== 0.0` case stays deferred, now with a test pinning it so a later phase cannot close it by accident. |
| B3 | `NoVotes → Failed` is **fail-OPEN in the decline direction**: `NoVotes` + negative outcome denies unconditionally, `Failed` + negative outcome allows iff `reject_count > 0`. The plan framed the change as pure tightening. | Negative-outcome acceptance criteria added in both directions; Approach reworded from "moves toward conformance" to "fills a gap §4.1 does not address"; changelog obligation stated. |
| B4 | Phase 7's chunking would be **O(N²/k)** — `session_ids_after` scans every key on every call, so paging means ⌈N/k⌉ full map scans per snapshot; and the proposed `rx` pending buffer was itself unbounded. | Redesigned: take the id list **once**, then `get_session` one at a time. One map pass, O(1) resident clones. Pending buffer explicitly bounded. This also deleted Phase 7's env var, which collapsed Phase 8. |
| B5 | Phase 2's quorum `threshold.type` enum would have **refused `count`**, which this runtime documents (`docs/policy.md:142`) and both layers treat as an `n_of_m` alias. And `weighted` has no defined semantics here. | `count` accepted as a documented alias; `weighted` refused as unimplemented rather than silently treated as a raw count; the schema gap added to spec #98 as item 4. |
| S1 | Phase 3's differential matrix could never pass at `value = 0` — both layers gate on `> 0.0` and diverge *outside* the gate. | `value = 0` carved out of the criterion; divergence recorded for `ASSUMPTIONS.md`. |
| S2 | The decline-not-gated hole survives Phase 3 via the "mathematically unreachable" branch — a zero-ballot negative commitment still seals at an over-participant threshold. | New Phase 3 criterion: close it or move it to Blocked with a reason. Not left silent. |
| S3 | Phase 2's constraint list omitted `voting.quorum` (`type` enum `count`/`percentage`, `value >= 0`) — and `evaluator.rs:293` accepts `n_of_m`, which is **not** in that enum. Exactly the drift Phase 2 exists to close. | Added. |
| S4 | Phase 9's "three tests break at rev 2" was **wrong** — both live branches are `>= 1` and no `== 1`/`< 1` comparison exists anywhere. The instructed edits were churn against a non-problem. | Replaced with "no test changes expected; if any go red the branch structure was restructured." Added the real hazard it missed: the `mode_state` byte change is safe because `conformance_loader.rs:516` uses subset matching. |
| S5 | Phase 7 criterion 2 was **not falsifiable** — a registry call count proves chunking, not residency, and there is no seam to instrument. | Rewritten to test an extracted batch-yielding function directly. |
| S6 | Phase 7 introduces `Resolved`-without-`Created` for a session removed mid-traversal — client gets a lifecycle event with `session: None`. Only the double-emit direction was covered. | New edge case and acceptance criterion: drop unknown-id terminal events, or document. |
| S7 | `MACP_CLEANUP_INTERVAL_SECS` is **already documented** (`docs/deployment.md:43`). | Corrected. |
| S8 | Phase 2 had no migration story: one newly-invalid `MACP_POLICIES_DIR` file aborts startup with no way to find out first — and `registry.rs:33` pre-registers the `std` policies **in the constructor**, so a bad validator is a startup crash with no wire-level symptom. | Added `MACP_POLICIES_DRY_RUN=1` validate-only mode and a `std_policies()` survival test. |
| S9 | Phase 11 cited §5.1(3) as MUST (it is SHOULD), and its `Incoming` argument **reasoned from a pre-existing bug** — RFC-MACP-0001 §7.5 says Suspend/Resume/Cancel *do* enter accepted history, so this runtime emitting them as Internal is non-conformance, not a constraint. | Citation corrected; the decision kept on its own engineering merit; the §7.5 gap moved to follow-ons. |
| N1–N4 | Phase 5 ordering overstated; `CommitmentMode` **is** `#[non_exhaustive]` (Open Question 4 resolved); `docs/policy.md:49` carries the same false schema-validation claim as CLAUDE.md but in a **tracked** file; ~20 line-number drifts in the repo map. | All applied; map corrected. |

**Confirmed sound under challenge** (the reverifier was asked to attack these specifically): Phase 1's
diagnosis, verified against the live branch; Phase 4's ability to fix the weighted arm **without**
disturbing `evaluator.rs:2251`/`:1739` (both return at the front-of-dispatch short-circuit and never
reach the weighted arm — the worry was unfounded); Phase 7's `Lagged` mechanism; Phase 5's public
path; `SecurityLayer`'s semver safety; Phase 14's three stale records; and the plan's central premise
that **no JSON-Schema validation exists in this runtime at all**.

**Scope recommendation NOT applied, deliberately.** The reverifier recommended cutting G4 (phases
9–13) from this drive and re-planning it separately, on the grounds that it is 5 of 14 phases, carries
both one-way doors, has a roll-forward-only rollback story, and sits at the tail after the executor
has spent its context on easier phases. That is a scope reduction, and scope is the user's call — the
user chose "everything, including follow-ons" with the size trade-off stated. G4 stays in the plan.
The G3→G4 boundary is the natural place to reassess, and `/drive` will report there.
