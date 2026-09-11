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

- **Status:** DONE — `5ee294d` + `11103d0` on `feat/policy-schema-conformance`, 2 verify rounds
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

- **Status:** DONE — `b6abf39`, 1 verify round (PASS; the critical items were verified by the
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

- **Status:** DONE — `248b916` (purely additive: 270 insertions, 0 deletions). Phase 3's doc gaps
  closed alongside in `05ce8ab`. 734 tests passing (729 + 5). Every new test mutation-checked.
  **The plan's DIAGNOSIS of this phase was wrong in two compounding ways, though the fix it
  prescribed was right** — see the correction block below. Criterion 5's safety rails
  (`evaluator.rs` `all_abstain_returns_no_votes`, `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum`)
  confirmed **byte-identical** to `b6abf39` by extracting both bodies from the old blob and diffing,
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

- **Status:** DONE — `87e2cf4`. 739 tests passing (734 + 5). `cargo semver-checks check-release` on
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

- **Status:** DONE (code+docs) — `5ea31a7`. Criteria 2 and 3 (release-PR lockstep, seven crates on
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

- **Status:** DONE — `a9f284c` + `68fdaed` + `8817799` + `ba6eb41`, 1 verify round (GAPS, no blockers).
  **Shipped design differs from the plan's, and the plan's was the worse one.** The plan specified an
  id-list snapshot; the gate showed the `Arc<Mutex<Session>>` handle snapshot is strictly better — it
  is a TRUE snapshot (so criterion 5's edge case is vacuous, not documented) and the tighter memory
  bound: ~8 B/session of shared, conditional retention versus ~64-72 B/session paid unconditionally
  and duplicated between `ids` and `synced`. At 10k sessions × 128 streams, ~10 MB + at most one
  registry pinned, versus ~92 MB. The orchestrator initially rejected this on the belief that pinning
  Arcs lets a slow client hold the registry's worth of sessions; both premises were wrong — the Arcs
  are shared, and eviction removes the map entry unconditionally (only *deallocation* defers).
  Net simplifying: `watch_sync.rs` 400→331 lines, public surface 5→3 items, the batch abstraction and
  the anomaly documentation deleted rather than fixed.
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

- **Status:** DONE — folded into Phase 7's PR per the verify gate's call (`ba6eb41`). Its env-binding
  criterion was void (Phase 7's redesign removed the env var it existed to test) and its docs
  criterion was already delivered, so what remained was the own-server tier-1 test. That test
  *replaced* Phase 7's weaker shared-server variant rather than joining it: on a shared runtime the
  only assertable property is "each of mine appears once", which cannot assert the sync emits
  **nothing else** — the other half of the criterion. A private runtime starts empty, so
  `created_counts.len() == 60` now fails on a foreign or duplicated `Created`.
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

- **Status:** DONE (`8e81481`, branch `feat/handoff-implicit-accept-rev2`, not yet pushed).
  Gate: 758 workspace tests passed / 0 failed (751 baseline + 7), tier-1 119 + 8 JWT + 5 tier-2,
  fmt/clippy/rustdoc clean, both lockfiles unmoved.
  **Divergence 1 — the Files list was wrong as a work item.** It named "17 `LogEntry` literal sites"
  as files to touch. They are an accurate *inventory* of where `LogEntry` is constructed, but the new
  field went on `HandoffOfferRecord` (inside `mode_state`), not on `LogEntry` — so **0 of the 17
  needed an edit**. The phase's actual diff is three non-additive lines: the const value, the record
  field, and the call site.
  **Divergence 2 — the planned "`>= 2` branch identical to `>= 1`" is inexpressible.** `clippy -D
  warnings` rejects two branches with identical bodies (`clippy::if_same_then_else`). The scaffolding
  is instead a single `rev2_elapsed_ms` helper that currently returns the rev-1 arithmetic verbatim;
  that function *is* the seam Phase 10 edits, which is what the plan wanted structurally.
  **Divergence 3 — a pre-existing test fails on a clean tree in this environment.**
  `macp-policy::registry::tests::enum_lists_match_the_canonical_schemas` reds locally because the
  sibling spec checkout sits on the unmerged `fix/issue-98-voting-semantics` branch, which already
  moved `voting.threshold` to `exclusiveMinimum: 0`. CI checks out spec `main`, so CI is green today
  — but this job reds the moment spec #98 merges. Tracked in macp-runtime issue #163. Every gate
  number above was therefore re-run with `MACP_POLICY_SCHEMAS_DIR` pointed at spec `origin/main`, so
  758/0 reflects what CI sees, not what this working copy sees.
  The Edge-cases prediction held exactly: no test changed, and the no-`== 1`-comparison claim was
  independently re-verified (four `semantics_rev` comparisons repo-wide, all `>= 1`).
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

- **Status:** DONE — `04d267d` (change) + `810a0c3` (gap closure), branch
  `feat/handoff-implicit-accept-rev2`, not pushed; accumulating toward G4's single PR.
  Gate: **766 passed / 0 failed**, tier-1 119 + 8 JWT + 5 tier-2, fmt/clippy/rustdoc clean, both
  lockfiles byte-unmoved. Verified round 1 = GAPS, round 2 = PASS (766/0 independently reproduced).
  **Two plan errors, both in my own acceptance criteria.** Criterion 3 was **undischargeable**:
  `assert_replay_equivalence` (`tests/conformance_loader.rs:356`) has one call site (`:520`) inside
  the vendored-fixture loop, no fixture exercises an implicit accept, `tests/conformance/` is
  byte-diffed by CI — and decisively, fixtures run through the live `Runtime` where
  `Session::builder` unconditionally stamps `CURRENT_SEMANTICS_REV`, so **a rev-1 history is not
  expressible in that harness at all**. Discharged by intent instead via a differential legacy-log
  fixture on the real `replay_session` path (`src/replay.rs:1036,1063`). Criterion 5 named the
  **wrong guard** for this phase's own hard edge case: `outcome_reason` is pinned by
  `assert_implicitly_accepted` (`src/replay.rs:920-927`), which the plan never mentions, not by
  `assert_replay_equivalence`. A mutation adding one trailing space to the string killed six tests
  and confirmed which guard fires.
  **A correction in the plan's favour:** the executor reported the `:298`/`:302` cites as stale;
  they were **correct at `882beeb`**, the commit the plan was written against, and were displaced by
  Phase 9. Intra-plan drift, not a drafting error.
  **Scope beyond the Files list:** `crates/macp-core/src/session.rs` (the rev-2 doc bullet, falsified
  by this change) and `src/replay.rs` (the criterion-3 substitute) — two files, both justified in
  `ASSUMPTIONS.md`.
  **Of the two multi-suspension tests added in the gap round, only one is a behavioural guard** —
  `rev2_handoff_history_subtracts_every_suspension_pair`. The other passes under mutation because
  more unsuspended time only makes an accept *more* likely; it pins fixture state, not behaviour.
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

### Phase 11 — synthesize the accept into accepted history (G4) · replanned 2026-09-11 as 11a–11f

**The one-way door, settled and verified against the RFC text (2026-09-11, second independent
read).** RFC-MACP-0010 §5.1(2) (`../multiagentcoordinationprotocol/rfcs/RFC-MACP-0010-handoff-mode.md`)
says the runtime "MUST append a synthetic `HandoffAccept` envelope to the session's **accepted
history** — before evaluating any subsequent message against the offer's acceptance state, and in
particular before any `Commitment` evaluation", and that "because the synthetic accept is an accepted
history entry, replay simply replays it: the timer itself is outside the replay boundary, its
recorded product is inside — the same construction as runtime-emitted
`SessionSuspend`/`SessionResume`/`SessionCancel` envelopes (RFC-MACP-0001 §7.5)". In this runtime
accepted history *means* `EntryKind::Incoming` — `log_store.rs:128` filters accepted ordinals to
`Incoming` only — so the synthetic accept is an **ordinary `Incoming` entry**: `Incoming` is
CONFIRMED, the §7.5 `Internal` treatment of the three lifecycle envelopes stays a recorded
non-conformance (follow-on 9), out of scope here.

**The shape of the mechanism (the decisions that are hard to reverse), fixed up front:**

1. **The entry is an ordinary `Incoming` `LogEntry` — no schema change, no discriminator field.**
   The replay re-dispatch trap ("replay re-dispatches an `Incoming` entry into the mode, which
   rejects `implicit: true`") is resolved by moving the client-rejection to the **client boundary**,
   not by marking the entry: `HandoffAcceptPayload.implicit == true` (already in `raw_payload`,
   already serde/prost round-tripped on every backend) *is* the discriminator, and it is trustworthy
   because at rev ≥ 2 no client-originated envelope carrying it (or squatting its `message_id`
   namespace) can ever be accepted (11c). Replay then dispatches the synthetic entry through the
   normal `mode.on_message_at` path (`replay.rs:85-140`) with zero replay-code changes, and the
   rev-2 mode accepts a *well-formed* implicit accept (11d). RFC §5.1(3)'s own wording supports the
   boundary placement: "clients MUST NOT submit it **via `Send`**". The rejected alternative — a
   `LogEntry` discriminator field + a special replay arm — forks live/replay dispatch, adds a
   persisted field, and makes an old binary's replay of a new log diverge *silently* (`replay.rs:167`
   `_ => {}`); under the chosen design an old (pre-11) binary replaying a rev-2 log fails **loudly**
   (its mode rejects `implicit: true` → `replay_session` `Err` → skip/strict-abort), which is the
   correct downgrade posture.
2. **Envelope constants** (each a MUST from §5.1(3) except where noted): `sender` = the offer's
   `target_participant`; `accepted_by` = the target; `message_id` = `implicit-accept:<handoff_id>`;
   `implicit` = `true`; `payload.reason` = `"implicit accept (timeout)"` (local choice — keeps
   `outcome_reason` byte-identical with the rev ≤ 1 interim string at `handoff.rs:398`, so
   `assert_implicitly_accepted` (`src/replay.rs:920-927`) survives fixture migration);
   `macp_version` = `"1.0"`; `mode` = session mode; `timestamp_unix_ms` = `received_at_ms` = **the
   computed deadline D** (§5.1(3) SHOULD, adopted as local MUST).
3. **D is the interval-walk deadline, not the naive formula.** `D = the earliest T with
   (T − offered_at) − suspended_in[offered_at, T] ≥ timeout`. The naive
   `offered_at + timeout + banked_since_offer(at observation)` is **wrong whenever a suspend/resume
   pair lands after the true deadline but before observation** — reachable in the lazy path
   (suspend/resume are RPCs, not session-scoped messages, so they can occur between D and the next
   message) — and it is observation-time-dependent, which forecloses Phase 12's byte-identity
   criterion permanently (the timestamp is baked into persisted history; fixing it later is another
   `semantics_rev`). The walk needs the pause boundaries, which no current state carries:
   `Session` has only the scalar (`session.rs:81-86`) and the checkpoint fast path
   (`replay.rs:36-84`) replays only post-checkpoint entries, so a log scan for the
   `SessionSuspend`/`SessionResume` entries is blind behind a checkpoint. Hence 11b: `Session`
   records completed suspension intervals, persisted via `PersistedSession`, rebuilt on replay for
   free because `replay.rs:151-166` already calls `session.suspend/resume` with recorded times.
4. **The decision "is the deadline elapsed?" stays the Phase-10 scalar** (`rev2_elapsed_ms`,
   `handoff.rs:148`) — it is exact for the decision (elapsed(T) ≥ timeout ⇔ T ≥ D, since every
   banked pause since the offer lies in [offer, T]); only the *timestamp* needs the walk.
5. **Emission is kernel work, mode-informed.** Two new **defaulted** `Mode` trait methods
   (semver-minor): `validate_client_envelope` (11c — reject forged `implicit` and the reserved
   `message_id` namespace, live client path only) and `due_synthetic_envelope` (11d — "given this
   session and clock, this envelope must enter history first"). The kernel (`process_message`,
   `runtime.rs:612`) wires them in 11e: synthesize → dispatch through the mode → durable append →
   commit → publish under the session mutex → then process the triggering message. Replay never
   calls either hook — the synthetic entry is data.
6. **At most one synthetic accept per session, ever**: RFC-0010 §5(5) — once an offer is accepted no
   further offers may be issued, so `Option<Envelope>`, not a queue.

**What Phase 12 (eager sweep) may assume once 11a–11f land** — pinned here because Phase 12's
byte-identity criterion is only achievable if Phase 11 guarantees it:

- The synthetic entry is a **pure function of the log prefix up to D** (offer record, bound policy
  timeout, suspension intervals with start < D). Nothing in it depends on observation time.
- Emission is **idempotent and mutex-serialized**: the deterministic `message_id` is in
  `seen_message_ids` after emission, the due-check returns `None` once disposition ≠ `Offered`, and
  both live emitters run under the session mutex — a sweep and a racing lazy trigger cannot
  double-append.
- The emission subroutine is factored as one `Runtime` method (11e:
  `synthesize_due_accept(&self, session_id, &mut Session, now_ms)`) that does
  due-check → dispatch → append → commit → publish; Phase 12's sweep calls exactly it.
- The sweep **MUST skip non-`Open` sessions** (same filter as `runtime.rs:1103`). This resolves
  `ASSUMPTIONS.md`'s "in-flight suspension term" entry in the skip direction: the interval walk uses
  completed pairs only, an in-progress pause is not in the vec, and a suspended session ticks no
  unsuspended time anyway. Phase 12 must not add an in-flight term.
- Publication uses `publish_accepted_envelope` while holding the session mutex (the FIFO premise at
  `runtime.rs:586-590`); the sweep must do the same.

**Test-harness reality check** (so no criterion below repeats the Phase-10 class of error):
`assert_replay_equivalence` (`tests/conformance_loader.rs:356`) is private to that file and called
only from the vendored-fixture loop (`:520`); `tests/conformance/` is vendored and byte-diffed by
CI, so **no fixture can be added**; fixtures run through the live `Runtime`, which stamps
`CURRENT_SEMANTICS_REV` unconditionally, so **rev ≤ 1 histories are only expressible in the
`src/replay.rs` `LogEntry`-fixture harness** (`handoff_entry`/`handoff_history*`, `:835-930`), never
in a live-`Runtime` harness. Live rev-2 flows are expressible in-process (the
`tests/stream_integration.rs:13` `make_runtime` pattern: `Runtime::new` + `rt.process` +
`rt.suspend_session`/`resume_session` + `rt.log_store.get_log` + `replay_session`), and over the
wire in tier-1. Every criterion below names its harness from this list.

---

#### Phase 11a — replay-determinism prep: one clock per internal entry, wider consistency check

- **Status:** TODO
- **Delivers:** follow-ons 10 and 11, verbatim: the double-`Utc::now()` skew class is dead, and
  `validate_replay_consistency` sees `mode_state` and suspension state. No new semantics.
- **Depends on:** Phase 10 (committed: `04d267d` + `810a0c3`).
- **Files:** `src/runtime.rs`, `src/replay.rs`.
- **Approach:** Add an `at_ms: i64` parameter to `make_internal_entry` (`runtime.rs:266`), mirroring
  `make_incoming_entry(env, received_at_ms)` (`:247`), and pass the caller's already-read clock at
  all five call sites: `maybe_expire_session` (`:312`, has `now` at `:305`), `cancel_session`
  (`:816`, currently reads **no** clock — add one read), `suspend_session` (`:875`, has `now_ms`),
  `resume_session` (`:933`, has `now_ms`), `cleanup_expired_sessions` (`:1106`, has `now`). This
  kills the class where live `accumulated_suspended_ms` and the replayed value differ by ~1 ms —
  which since Phase 10 gates an accept/reject decision, so a flip within 1 ms of the deadline made
  a live-`Resolved` session fail replay and vanish at `src/main.rs:385` ("failed to replay session;
  skipping"). Then widen `validate_replay_consistency` (`replay.rs:183-222`) with three warn-only
  comparisons: `mode_state` (byte equality), `accumulated_suspended_ms`, `suspended_at_ms`. Rejected
  alternative: leaving the widening to 11e — 11b–11e's own replay tests want the wider check as a
  tripwire, so it goes first.
- **Edge cases & failure modes:** `SessionResumePayload.banked_ms` (`runtime.rs:924-931`) remains
  live-clock-derived and ignored by replay (`replay.rs:151-166`) — after this change it equals the
  replay-derived value by construction; document it as informational in a comment, do not start
  consuming it (that would change replay of legacy logs). The consistency check stays **warn-only**
  (log is authoritative, snapshots best-effort — `replay.rs:176-181`); making it fatal would turn a
  benign snapshot lag into an outage.
- **Acceptance criteria:**
  1. `make_internal_entry` takes the clock as a parameter and no caller lets it read `Utc::now()`
     itself — discharged by the signature (compile-time) plus a new runtime-level test
     `suspend_resume_entries_share_the_session_mutation_clock` (live-`Runtime` harness, new file
     `tests/handoff_implicit_accept_live.rs` or extend `tests/integration_mode_lifecycle.rs`):
     suspend then resume, read the two log entries, assert
     `session.accumulated_suspended_ms == resume_entry.received_at_ms − suspend_entry.received_at_ms`
     exactly. (Honest note: pre-fix this could only fail on a clock tick between the two reads, so
     it pins the invariant rather than differentially proving the bug; the signature is the real
     guarantee.)
  2. `validate_replay_consistency` counts a `mode_state` mismatch and a suspension-state mismatch —
     discharged by extending `replay_consistency_flags_state_and_dedup_divergence`
     (`src/replay.rs:788`), which builds both `Session`s directly (fields are `pub`; writable today).
- **Tests:** the two above; full workspace gate
  (`RUSTC_WRAPPER="" MACP_POLICY_SCHEMAS_DIR=<spec origin/main schemas> cargo test --workspace --all-targets`)
  stays 766+/0.
- **Docs:** rustdoc on `make_internal_entry` (why the clock is injected) and on the `banked_ms`
  field's informational status.

#### Phase 11b — suspension intervals on the session + the deadline function (pure model)

- **Status:** TODO
- **Delivers:** the state and arithmetic 11d needs for D. **Zero behavior change** — nothing reads
  the new field or function outside tests.
- **Depends on:** 11a.
- **Files:** `crates/macp-core/src/session.rs`, `crates/macp-storage/src/registry.rs`,
  `src/replay.rs` (tests only).
- **Approach:** Add `pub suspension_intervals: Vec<(i64, i64)>` (completed `(suspended_at,
  resumed_at)` pairs, session-timeline ms) to `Session` — additive-safe: `Session` is
  `#[non_exhaustive]` (`session.rs:64`). `Session::resume` (`session.rs:183`) pushes
  `(suspended_at, now_ms)` unconditionally before the cap check (the Phase-9 precedent:
  record everywhere, read only under rev ≥ 2 — `handoff.rs:49` comment). Recording in `resume`
  means **replay reconstructs the vec with zero replay-code changes**, because `replay.rs:151-166`
  already drives `session.suspend/resume` from recorded `received_at_ms` — and 11a just made those
  equal the live mutation clock exactly. Persist it: `suspension_intervals` on `PersistedSession`
  (`registry.rs:15-53`, `#[serde(default)]`) and in both `From` impls, so the checkpoint fast path
  (`replay.rs:36-84`), snapshot loads, and disk GC survivors all carry it. Add
  `Session::unsuspended_deadline(&self, from_ms: i64, duration_ms: i64) -> i64`: walk pairs with
  `start >= from_ms` in order — `cur = from_ms; remaining = duration_ms;` for each pair `(s, e)`:
  if `s − cur >= remaining` return `cur + remaining`, else `remaining −= s − cur; cur = e`; finally
  `cur + remaining`. Pure, saturating, no clock. Rejected alternatives, on the record: (a) scanning
  the log for suspend/resume entries at emission time — blind behind a checkpoint (the fast path
  replays only post-checkpoint entries) and puts log I/O inside the mode decision; (b) an
  incrementally-maintained per-offer deadline field — requires mutating `mode_state` on resume,
  which is not mode-dispatched, so it needs a new dispatch path in both live and replay code in
  lockstep (strictly more machinery than one vec); (c) no intervals, naive D0 — forecloses Phase 12
  and bakes observation-dependent timestamps into permanent history (see the header).
- **Edge cases & failure modes:** a pause can never straddle `from_ms` (offers are accepted only
  while `Open`); a pair with `s` exactly at the returned deadline does not extend it (the offer's
  unsuspended time already hit the timeout at that instant); an in-progress suspension is
  deliberately **not** in the vec (completed pairs only — the lazy path runs only on `Open`
  sessions, and Phase 12 skips suspended ones; `debug_assert!(session.suspended_at_ms.is_none())`
  at the 11d call site). Growth is one 16-byte pair per suspend/resume cycle — each cycle already
  writes two log entries and a snapshot, so the marginal cost is noise; unbounded in principle,
  documented, not capped (a cap would silently corrupt D). `cancel()` while suspended records no
  pair — terminal, nothing will read it. Legacy snapshots/checkpoints deserialize an empty vec —
  correct for every rev ≤ 1 session (nothing reads it) and for rev-2 sessions no such artifact can
  predate this code (rev 2 has never been released; Phases 10–13 ship as one PR).
  **Semver:** the `Session` field is additive (non-exhaustive); the `PersistedSession` field is a
  `constructible_struct_adds_field` **major** — already spent by D7's 0.8.0 decision, which
  anticipated "at least one more field" in Phase 11 (it guessed `HandoffOfferRecord`; it lands on
  `PersistedSession` instead, and `HandoffOfferRecord` gains **nothing** — record that when Phase 13
  runs `cargo semver-checks`).
- **Acceptance criteria:** (all in named tests, all writable today)
  1. `unsuspended_deadline` unit matrix in `macp-core`: no pauses; one pause fully before the
     deadline (extends by its width); one pause starting after the raw deadline (**does not**
     extend); the boundary pause (`s == returned deadline`); two pauses; pause predating `from_ms`
     (ignored) — `session.rs` unit tests.
  2. Replay rebuilds the vec: a `LogEntry` fixture with two suspend/resume pairs
     (`handoff_history_with_two_suspensions`, `src/replay.rs:1107`) replays to
     `suspension_intervals == [(1_050, 1_300), (1_330, 1_500)]` — `src/replay.rs` test harness.
  3. Checkpoint round-trip: extend `replay_from_checkpoint_restores_state` (`src/replay.rs:565`) or
     a sibling so a checkpoint written after a pause restores the vec — `src/replay.rs` harness.
  4. Live/replay agreement: in the live-`Runtime` harness, suspend/resume then compare the live
     session's vec against `replay_session`'s — and 11a's widened consistency check would flag a
     divergence here if the vec is later added to it (optional fourth comparison; take it if cheap).
- **Tests:** as above; workspace gate green.
- **Docs:** rustdoc on the field (completed pairs only, recorded at every rev, read at rev ≥ 2) and
  on `unsuspended_deadline` (the walk, with the RFC §5.1(1)/(3) citations).

#### Phase 11c — the client boundary: reject forged implicit accepts and reserve the id namespace

- **Status:** TODO
- **Delivers:** at rev ≥ 2, no client-originated envelope can carry `implicit: true` or a
  `message_id` in the `implicit-accept:` namespace — closing the squat DoS **before** any code
  exists that would trust the namespace. Behavior change is rev-≥ 2-only and additive-restrictive.
- **Depends on:** 11a (independent of 11b; ordered here so the namespace is reserved before 11d/11e
  rely on it — landing acceptance-of-implicit before this would open a forgery window in the tree).
- **Files:** `crates/macp-modes/src/mode/mod.rs`, `crates/macp-modes/src/step.rs`,
  `crates/macp-modes/src/mode/handoff.rs`, `src/runtime.rs`, plus runtime-level tests.
- **Approach:** New defaulted trait method
  `Mode::validate_client_envelope(&self, session: &Session, env: &Envelope) -> Result<(), MacpError>`
  (default `Ok(())`), documented as: *called on live client-submitted envelopes only — never on
  replay, never on runtime-synthesized envelopes; library kernels MUST call it on inbound traffic*.
  `HandoffMode` implements it, **gated to `session.semantics_rev >= 2`** so rev ≤ 1 wire behavior
  is byte-identical: (a) `message_type == "HandoffAccept"` whose payload decodes with
  `implicit == true` → `Err(MacpError::InvalidPayload)` (same code the mode returns today at
  `handoff.rs:335`, so the rev-2 error surface doesn't shift); (b) any `message_id` starting with
  the new `pub const IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX: &str = "implicit-accept:"` →
  `Err(MacpError::InvalidEnvelope)`. Call sites: (1) `process_message` (`runtime.rs:612`) after
  `mode.authorize_sender` (`:659`) and before dispatch — after authorize so rev ≤ 1 error ordering
  (Forbidden before InvalidPayload) is untouched; (2) `process_session_start` (`runtime.rs:344`)
  after the session is built and the mode resolved, before the commit-point append — because the
  squat works through `SessionStart` too (its `message_id` enters `seen_message_ids` at `:559`);
  (3) `step::validate_message` (`step.rs:66`) between authorize and `on_message`, so library
  consumers inherit the boundary (replay does not use `step` — verified: `replay_entry` calls
  `authorize_sender`/`on_message_at` directly, `replay.rs:126-137`; executor must re-confirm no
  other `validate_message` caller exists on the replay path). **Why the plan's old placement was
  wrong:** it said "reserve the prefix at `server.rs:118`, rev-gated" — unimplementable as written:
  `validate_envelope_shape` runs before any registry lookup, so the session's `semantics_rev` (and
  mode) are unknown there, and a transport-level check would also miss library consumers.
- **Edge cases & failure modes:**
  - **The squat is real and wider than one message type** (verified): dedup is per-session
    (`session.seen_message_ids`), `server.rs:118` checks only non-emptiness, and the offerer can
    send an accepted `HandoffContext` (any disposition, `handoff.rs:305-328`) — or the initiator a
    `SessionStart` — carrying `message_id = "implicit-accept:<any future handoff_id>"`. Once
    accepted, the slot is consumed; after 11e the synthesis would be silently skipped and the
    session could never commit. Hence: reserve the **prefix**, for **all** message types, at rev ≥ 2,
    in handoff sessions.
  - Post-11e, a client re-sending the synthetic's exact `message_id` after emission hits the dedup
    check **before** the hook (`step::check_preconditions` runs first, `runtime.rs:634`) and gets a
    `duplicate = true` ack rather than `InvalidEnvelope`. Accepted oddity: it mutates nothing and
    reordering the hook ahead of dedup would change rev ≤ 1 duplicate semantics.
  - Scope is handoff sessions only (the hook lives on the mode): a decision-mode client using an
    `implicit-accept:` id is unaffected — squatting is per-session, so cross-mode reservation buys
    nothing.
  - The mode's in-`handle_message` rejection at `handoff.rs:335` **stays untouched in this
    sub-phase** (belt and suspenders until 11d restructures it).
- **Acceptance criteria:**
  1. In the live-`Runtime` harness, at current rev: a `HandoffContext` with
     `message_id = "implicit-accept:h1"` is rejected with `InvalidEnvelope`, a `SessionStart` with
     such an id is rejected, and neither consumes a dedup slot (a follow-up valid message with the
     same id... is itself reserved — assert instead that history and `seen_message_ids` are
     unchanged). Test `reserved_message_id_namespace_is_rejected_at_rev2`.
  2. A client `HandoffAccept` with `implicit: true` is rejected at the hook (mode-level unit test
     calling `validate_client_envelope` directly, plus the runtime-level path) —
     `client_implicit_accept_rejected_at_the_boundary`.
  3. Rev ≤ 1 unaffected: a `src/replay.rs` `LogEntry` fixture at `semantics_rev = 1` containing an
     entry with a reserved-prefix id **still replays** (the hook is not on the replay path), and a
     mode-level test with `session.semantics_rev = 1` (writable — field is `pub`) shows the hook
     returns `Ok` — `reserved_namespace_is_rev_gated`.
  4. `step::validate_message` enforces the hook — a `macp-modes` unit test drives it directly.
- **Tests:** the four above; workspace gate green; no tier-1 change yet (11f covers the wire).
- **Docs:** trait-method rustdoc is the contract (library kernels MUST call it); prefix const
  rustdoc cites RFC-0010 §5.1(3).

#### Phase 11d — the synthesis contract in the mode (wired to nothing yet)

- **Status:** TODO
- **Delivers:** `HandoffMode` can (a) say when a synthetic accept is due and produce the exact
  envelope, and (b) accept a well-formed implicit accept arriving through dispatch (live synthesis
  in 11e, and replay) at rev ≥ 2. **Live behavior unchanged**: the kernel does not call the new
  hook yet, the interim in-`Commitment` path (`handoff.rs:383-399`) still runs at every rev, and
  the client path to `implicit: true` is already closed (11c).
- **Depends on:** 11b (needs `unsuspended_deadline`), 11c (needs the boundary closed before the
  mode will accept implicit accepts in dispatch).
- **Files:** `crates/macp-modes/src/mode/mod.rs`, `crates/macp-modes/src/mode/handoff.rs`.
- **Approach:** Second defaulted trait method
  `Mode::due_synthetic_envelope(&self, session: &Session, now_ms: i64) -> Option<Envelope>`
  (default `None`). Handoff implementation: `semantics_rev >= 2` && bound policy has
  `acceptance.implicit_accept_timeout_ms > 0` (same resolution as `handoff.rs:380-383`,
  `unwrap_or_default` on parse failure ⇒ 0 ⇒ never due — matches interim) && the single `Offered`
  offer has `offered_at_ms > 0` && `implicit_accept_elapsed_ms(session, offer, now_ms) >= timeout`
  (the Phase-10 scalar seam — exact for the decision) → build the envelope with the header's
  constants and `timestamp_unix_ms = session.unsuspended_deadline(offer.offered_at_ms, timeout)`.
  `debug_assert!(D <= now_ms)`. Then restructure `handle_message`'s `HandoffAccept` arm
  (`handoff.rs:333-336`): `if payload.implicit` → at `semantics_rev < 2` reject `InvalidPayload`
  (today's behavior, preserved verbatim for legacy replay); at rev ≥ 2 **validate strictly and
  accept**: offer exists, `disposition == Offered`, `env.sender == offer.target_participant`,
  `payload.accepted_by == offer.target_participant`,
  `env.message_id == format!("{IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX}{handoff_id}")` — then apply the
  same mutation as an explicit accept (`disposition = Accepted`, `accepted_by = Some(target)`,
  `outcome_reason = Some(payload.reason)`). The mode MUST NOT re-verify the deadline arithmetic in
  this arm: on replay the entry is dispatched with `ctx.accepted_at_ms = received_at_ms = D`, but
  `accumulated_suspended_ms` at that replay point already includes pauses that occurred **after** D
  (replayed before the synthetic entry, since suspend/resume entries between D and the trigger sit
  earlier in the log) — a re-check would compute less unsuspended time than the timeout and
  wrongly reject a correctly-emitted entry. This is the single most important negative rule in the
  phase; write it into the arm's comment. (Note the log's `received_at_ms` becomes locally
  non-monotonic in exactly that case — synthetic-at-D after resume-at-later-than-D. Nothing orders
  by `received_at_ms`: replay is positional, ordinals are positional (`log_store.rs:126-134`).
  Document, don't "fix".)
- **Edge cases & failure modes:** no policy bound / no offer / offer already settled → `None`;
  session with `offered_at_ms == 0` cannot exist at rev ≥ 2 (rev ≥ 1 records the acceptance clock,
  `handoff.rs:290`) but keep the guard — it costs nothing and the interim has it; the trait
  method takes the session **immutably** and allocates only when due (per-message cost at steady
  state: one `mode_state` decode for handoff sessions with a bound timeout — accepted; a cached
  flag was rejected as premature). Payload bytes must be a fixed prost encoding — pin them.
- **Acceptance criteria:** (all `macp-modes` unit tests — hand-called hooks, injected clocks; all
  writable today)
  1. `due_synthetic_envelope_emits_at_the_deadline`: not due one ms before, due at/after; envelope
     field-by-field equals the header's constants; `timestamp_unix_ms` equals the walk deadline —
     including the killer case: **a pause after the true deadline does not move the timestamp**
     (pause at D+50 while `now` is D+500 → timestamp still D, where the naive formula says D+pause).
     This test is the committed replacement for the old plan's "mutation test" — it fails on the
     observation-time and naive-formula implementations alike.
  2. `synthetic_payload_bytes_are_pinned`: the prost bytes of the payload equal a literal vector
     (guards Phase 12 byte-identity and accidental field reordering).
  3. `implicit_accept_dispatch_accepted_at_rev2`: dispatching the returned envelope through
     `on_message_at` mutates the offer exactly like the interim did (`assert_implicitly_accepted`
     shape); rejected when malformed (wrong sender, wrong `accepted_by`, wrong `message_id`, offer
     already settled) and at `semantics_rev = 1` (field write).
  4. `client_submitted_implicit_accept_is_rejected` (`handoff.rs:729`) is **restructured, not
     preserved** — the old Phase-11 criterion 5 ("still passes") is withdrawn as written: under any
     design in which replay re-dispatches the `Incoming` entry, the mode cannot keep rejecting
     `implicit: true` unconditionally at rev ≥ 2. It becomes the rev-gated pair in criterion 3 plus
     11c's boundary tests, which together enforce the same RFC MUST at the layer the RFC names.
- **Tests:** as above; workspace gate green. The interim path still runs — the Phase-10 rev-2
  fixtures stay green through this sub-phase by design.
- **Docs:** trait rustdoc (the kernel contract: dispatch-append-commit-publish before the trigger;
  never called on replay); the no-re-verification comment in the accept arm.

#### Phase 11e — the cutover: kernel emission, interim retired for rev ≥ 2, fixture migration

- **Status:** TODO
- **Delivers:** the RFC-MACP-0010 §5.1 behavior, live and lazily. **This is the atomic commit:
  kernel wiring and the interim's rev-gate move together** — either alone leaves rev-2 sessions
  with double-application or no implicit accept at all.
- **Depends on:** 11d.
- **Files:** `src/runtime.rs`, `crates/macp-modes/src/mode/handoff.rs` (the interim gate),
  `src/replay.rs` (fixture migration), `crates/macp-core/src/session.rs` (rev-2 doc bullet),
  new `tests/handoff_implicit_accept_live.rs`, `tests/stream_integration.rs`.
- **Approach:** Factor a `Runtime` method
  `async fn synthesize_due_accept(&self, session_id: &str, session: &mut Session, now_ms: i64) -> Result<(), MacpError>`
  — the seam Phase 12's sweep calls — doing: `mode.due_synthetic_envelope(session, now_ms)`; if
  `Some(syn)` and `!session.seen_message_ids.contains(&syn.message_id)`:
  `mode.authorize_sender(session, &syn)` (mirrors replay, which authorizes every `Incoming` entry —
  the target is a declared participant by offer validation, `handoff.rs:257`), dispatch
  `mode.on_message_at(session, &syn, &MessageContext::new(syn.timestamp_unix_ms))`, build the entry
  with `make_incoming_entry(&syn, syn.timestamp_unix_ms)` (so `received_at_ms == timestamp_unix_ms
  == D`), durable `append_log_entry` (**commit point A** — failure returns `StorageFailed` and
  nothing has mutated, same discipline as `runtime.rs:669-674`), `log_store.append`, then the
  commit **without** `step::commit`: insert the id into `seen_message_ids` and
  `apply_mode_response` only — deliberately **no** `record_participant_activity` (replay never
  records activity for any entry, `replay.rs:92-137`; crediting the target with "activity" they did
  not perform would also be a lie), then `metrics.record_message_accepted`, then
  `publish_accepted_envelope(&syn)` **still under the session mutex**. Wire it into
  `process_message` after 11c's hook and before the trigger's dispatch (between `runtime.rs:659`
  and `:663`), reusing the trigger's `accepted_at_ms` clock read. Then gate the interim loop: wrap
  `handoff.rs:383-399`'s mutation in `if session.semantics_rev < 2` — at rev ≥ 2 an expired-offer
  `Commitment` on a history **lacking** the synthetic entry now fails `commitment_ready`
  (`handoff.rs:158`) with `InvalidPayload`. That fail-loud choice is deliberate: leaving the interim
  active at rev ≥ 2 would let replay of a foreign/buggy rev-2 log (no synthetic entry) silently
  resolve, hiding exactly the divergence this phase exists to make impossible.
  "Atomically enough" for the two appends: they are sequential under one session mutex, each with
  the existing append-is-the-commit-point discipline. The only new intermediate state — synthetic
  committed, trigger append failed or trigger rejected by mode validation — is a **valid history**:
  the accept was due regardless of the trigger's fate, the entry is idempotent (dedup by
  deterministic id + disposition gate), and a crash between the appends replays to the same state.
  A synthetic-append failure rejects the trigger with `StorageFailed` *before* the trigger consumed
  a dedup slot — §5.1(2) forbids evaluating the trigger without the accept in history, and this
  runtime never acknowledges what it could not persist. Note the freeze-profile invariant reads
  "**rejected messages** must not mutate accepted history" — the synthetic entry is not the
  rejected message's mutation but the runtime's own observation, which the trigger merely
  occasioned; state this in the code comment and the changelog.
- **Edge cases & failure modes:**
  - **Trigger ordering with prechecks** (all verified against `runtime.rs:634-651`): a duplicate
    trigger returns early — no synthesis (a duplicate is not "processed"; the next fresh message
    synthesizes); a TTL-expired trigger expires the session — no synthesis (terminal, no commitment
    possible); a suspended session rejects every message (`step.rs:58`) — the lazy path can never
    observe an active suspension (the `ASSUMPTIONS.md` entry, now load-bearing); an
    unauthorized or 11c-rejected trigger — no synthesis (rejection precedes it).
  - An explicit `HandoffAccept`/`HandoffDecline` from the target arriving **after** the deadline:
    synthesis runs first, the explicit message then finds `disposition != Offered` and is rejected
    `InvalidPayload` — §5.1(4)'s history-order rule, exactly. Before the deadline, the explicit
    settles the offer and `due_synthetic_envelope` returns `None` forever.
  - `SuspendSession`/`ResumeSession`/`CancelSession` RPCs are not session-scoped messages and never
    trigger synthesis; a session can go terminal with an unobserved expired offer — permitted
    (§5.1(2)'s MUST binds message processing, not termination).
  - Fixture migration (the exact break-list; anything outside it going red means the design
    drifted — stop and re-read): `src/replay.rs`
    `current_rev_handoff_history_replays_identically_to_rev1` (`:986`) — premise dissolves at rev 2;
    replace with a rev-2 fixture *containing* the synthetic entry and keep a rev-1-only sibling;
    `rev2_handoff_history_implicitly_accepts_on_unsuspended_time` (`:1068`) and
    `rev2_handoff_history_accepts_on_unsuspended_time_across_two_pauses` (`:1152`) — insert the
    synthetic entry (id `implicit-accept:h1`, sender `bob`, `received_at == timestamp == D`,
    payload from the pinned bytes) before the commitment; the rev-1 arms and
    `rev2_handoff_history_subtracts_every_suspension_pair` (`:1130`, asserts `Err`) stay green.
    `crates/macp-modes` `implicit_accept_timeout_fires` (`:1290`),
    `rev2_matches_legacy_arithmetic_when_nothing_was_suspended` (`:1606`), and any
    `implicit_accept_outcome` (`:1531`) caller asserting interim acceptance at current rev — pin to
    `semantics_rev = 1` (explicit field write) or migrate to the hook flow;
    `rev2_stops_counting_suspended_time_toward_implicit_accept` (`:1582`) asserts rejection — stays.
  - `seen_message_ids` grows by one for the synthetic — live and replay agree (replay inserts every
    `Incoming` id, `replay.rs:136`), and 11a's widened consistency check watches the rest.
  - Accepted ordinals shift by one for sessions with a synthetic accept — wire-visible to passive
    subscribers, rev-gated by construction (only rev-2 sessions emit); 11f asserts the contiguous
    sequence.
- **Acceptance criteria:** (harness named per item)
  1. `lazy_synthesis_enters_history_before_the_trigger` (live-`Runtime`, new
     `tests/handoff_implicit_accept_live.rs`): policy with a short timeout, offer, sleep past it,
     send `Commitment` → resolved; the log contains the synthetic `Incoming` entry **immediately
     before** the commitment entry with `sender == target`, `implicit == true`,
     `accepted_by == target`, `message_id == "implicit-accept:h1"`.
  2. `synthetic_timestamp_is_the_deadline_not_observation_time` (same harness): trigger sent well
     after the deadline; assert `entry.timestamp_unix_ms == entry.received_at_ms == D` computed
     from the offer entry's `received_at_ms + timeout` (no suspension variant), strict equality —
     and a suspended variant using `rt.suspend_session`/`resume_session` where D reflects only the
     pause that precedes it.
  3. `live_history_replays_byte_identically` (same harness): after criterion 1's flow, run
     `replay_session` over `rt.log_store.get_log(sid)` and assert `state`, `resolution`,
     `mode_state` (byte-equal), and `seen_message_ids` (set-equal) against the live session — the
     four assertions of `assert_replay_equivalence`, re-implemented locally because that fn is
     private to `tests/conformance_loader.rs` and its fixture loop cannot be extended (vendored,
     CI-byte-diffed). **The old criterion 3 ("`assert_replay_equivalence` green") is discharged by
     intent, not letter — the letter is unsatisfiable in this repo, per the Phase-10 finding.**
  4. `non_commitment_message_triggers_synthesis` (same harness): a post-deadline `HandoffContext`
     — not a `Commitment` — causes emission (§5.1(2) binds every session-scoped message; the old
     interim only fired on `Commitment`).
  5. `late_explicit_accept_loses_to_history_order` (same harness): explicit accept after the
     deadline → rejected, offer stands implicitly accepted (§5.1(4)).
  6. `squatter_cannot_block_synthesis` (same harness): pre-send the reserved id (rejected per 11c),
     then the deadline flow — session still commits (the old criterion 4, now dischargeable).
  7. `rev2_commitment_without_synthetic_entry_fails_replay` (`src/replay.rs` harness): a rev-2
     history with an expired offer and a commitment but **no** synthetic entry replays to `Err`
     (the fail-loud gate), while the same entries at `semantics_rev = 1` still resolve (interim
     preserved for legacy).
  8. `stream_subscribers_see_the_synthetic_envelope_in_order`
     (`tests/stream_integration.rs` pattern): subscribe via the stream bus, run the flow, assert
     the synthetic envelope arrives between the pre-deadline message and the commitment.
- **Tests:** the eight above plus the migrated fixtures; full workspace gate green
  (`RUSTC_WRAPPER=""`, `MACP_POLICY_SCHEMAS_DIR` pointed at spec `origin/main` — issue #163).
- **Docs:** rewrite the rev-2 bullet in `session.rs:18-35` (it currently describes Phase 10's
  arithmetic only; it must now describe the synthetic entry — the only place revisions are
  documented, per the Phase-10 precedent); rustdoc on `synthesize_due_accept` naming Phase 12 as
  its second caller. Tracked-file prose docs land in Phase 13.

#### Phase 11f — wire-level proof: tier-1 coverage

- **Status:** TODO
- **Delivers:** the behavior proven through the real gRPC boundary, in the suite CI runs on every
  PR.
- **Depends on:** 11e.
- **Files:** `integration_tests/tests/tier1_protocol/` (new `test_handoff_implicit_accept.rs` +
  `mod.rs` registration). **No dependency changes**, so `integration_tests/Cargo.lock` must stay
  byte-unmoved — its CI guard is `cargo metadata --locked`.
- **Approach:** Follow the established tier-1 patterns: `RegisterPolicy` with
  `acceptance.implicit_accept_timeout_ms` (pattern: `test_policy_registry.rs`), handoff session
  bound to it, `SuspendSession`/`ResumeSession` (pattern: `test_suspend_resume.rs`), passive
  subscribe (pattern: `test_passive_subscribe.rs`). Run on scratch port 50123, kill only the
  spawned PID.
- **Edge cases:** wall-clock timing over a real socket — use a timeout comfortably above scheduler
  noise (≥ 500 ms) and sleep with margin; the deadline-timestamp equality assertions stay in the
  in-process tests (11e) where the offer's acceptance clock is readable — tier-1 asserts
  **presence, shape, and order**, not millisecond equality.
- **Acceptance criteria:** four named tier-1 tests, all writable in the existing harness:
  1. `implicit_accept_resolves_after_timeout`: offer, wait, `Send` commitment → OK; `GetSession`
     shows `Resolved`.
  2. `synthetic_envelope_appears_on_stream_in_order`: passive subscribe replays the synthetic
     envelope between offer(/context) and commitment with contiguous sequence numbers (the
     ordinal-shift check).
  3. `client_implicit_and_reserved_ids_rejected_on_the_wire`: `HandoffAccept` with
     `implicit: true` and a `HandoffContext` with a reserved id both rejected; session then
     commits normally.
  4. `suspended_time_does_not_tick_on_the_wire`: suspend across the timeout, resume, commit
     promptly → rejected (`InvalidPayload`), proving the deadline excluded the pause end-to-end.
- **Tests:** tier-1 run per `docs/testing.md`
  (`MACP_TEST_BINARY=../target/debug/macp-runtime cargo test -- --test-threads=1`), count moves
  from 119 to 123; tiers 2/3 untouched.
- **Docs:** none here (Phase 13 owns `docs/modes.md`/`docs/API.md`/changelog and closes follow-on
  1; `CLAUDE.md` is gitignored — say so in the PR description when its local copy is touched).

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

### Phase 13 — G4 docs, API hygiene and close-out

- **Status:** TODO
- **Delivers:** docs, changelog, `follow_ons.md` item 1 closed, `#[non_exhaustive]` on the
  mode-state records, release cut **as 0.8.0**.
- **Depends on:** Phase 12.
- **Files:** `docs/modes.md`, `docs/API.md`, `plans/defer/follow_ons.md`,
  `crates/macp-modes/src/mode/handoff.rs`, `crates/macp-modes/src/mode/quorum.rs`,
  `CLAUDE.md` (local only).
- **Added 2026-09-11 — the release is 0.8.0, not 0.7.6, and this phase owns the API change that
  forces it.** `RUSTC_WRAPPER="" cargo semver-checks check-release --workspace` fails with
  `constructible_struct_adds_field` on `HandoffOfferRecord.suspended_ms_at_offer`
  (`handoff.rs:49`), added by Phase 9. `release-plz.toml` sets `semver_check = true`, so this
  **blocks the release PR**, and the single `version_group` moves all seven crates. There is no
  route back to 0.7.x: `#[non_exhaustive]`, privatising the struct or its fields, and relocating
  the state are all equally breaking, and Phase 11 adds another field to the same struct. Per
  `DECISIONS.md` D7 the break is spent on `#[non_exhaustive]` for the handoff and quorum
  mode-state records, matching the pattern `Session` (`session.rs:64`), `MacpError`
  (`error.rs:5`), `ModeResponse` (`mode.rs:11`) and `PolicyFileOutcome`
  (`macp-policy/src/registry.rs:67`) already follow — the mode-state records are the exception.
  **Keep it as its own commit** so Phase 11's behaviour change stays bisectable from the API
  change.
- **Acceptance criteria:**
  1. the wire-visible change is stated in the changelog as a deliberate semantics change gated on
     `semantics_rev = 2`, not as a bugfix;
  2. `follow_ons.md` item 1 is marked done;
  3. `cargo semver-checks check-release --workspace` reports the bump as major **deliberately**,
     and the changelog names the exhaustive-construction break for external callers;
  4. `follow_ons.md` item 12's note survives — after this phase, mode-state fields are additive.

### Phase 14 — tracked-file hygiene (G5, rides G2's PR)

- **Status:** DONE — `282b970`. Four stale records corrected, each verified against the code first:
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
