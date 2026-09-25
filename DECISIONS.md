# DECISIONS

Durable record of assumptions reconciled from `ASSUMPTIONS.md`. Each entry names the
original assumption, the independent recommendation it received, the verdict, and the
resulting status. `/ship` and later reconciliations read this file rather than replaying
the conversation that produced it.

---

## 2026-08-30 — `plans/list-sessions-pagination.md` closeout (6 entries)

Reconciled at the end of the plan, before merging PR #116. Ranked by blast radius; each
entry got a fresh independent recommender. None was a genuine one-way door — all six are
additive, config-only, or test-only — so all six were recommended on at the Opus tier
rather than escalated.

### D1 — `macp-core` re-export from `macp-runtime`'s root → **CHANGED**

- **Origin:** `plans/rfc-macp-0013-commitment-hash-PROGRESS.md` (not the pagination plan;
  that plan merged in `cfd5414`, so the entry was resolved here rather than orphaned).
- **Assumed:** `src/lib.rs` re-exports four lower crates but not `macp-core`, so consumers
  cannot reach `macp_core::commitment_hash` through `macp_runtime::*`. Logged as a
  pre-existing, low-blast-radius gap, deliberately left alone.
- **Recommendation:** CHANGE, and land before #114 freezes 0.7.0. The entry **under-scoped
  the problem**. The material hole is not `commitment_hash` but
  `macp_core::mode::MessageContext`, which appears in the signature of `on_message_at` — a
  defaulted method on the publicly re-exported `Mode` trait. An external consumer writing a
  custom mode cannot override it without a direct `macp-core` dependency, and that method is
  the documented way to obtain a trustworthy clock instead of the forgeable
  `Envelope.timestamp_unix_ms`. Independently verified: `crates/macp-modes/src/mode/mod.rs`
  re-exports the sibling type `ModeResponse` under a comment stating exactly the
  "keep the path resolving" motive, and simply omits `MessageContext`.
- **Verdict:** Apply both lines. `pub use macp_core;` in `src/lib.rs`, and `MessageContext`
  added to the existing `mode/mod.rs` re-export.
- **Rejected:** `pub use macp_core as core;` — tested and shown to shadow the `core` extern
  prelude. Zero bare `core::` paths exist in the workspace today, making it a latent trap
  rather than a visible error. Also rejected: a narrow `commitment_hash` shim, which fixes
  one symptom, leaves `MessageContext` broken, and entrenches a file-per-module pattern.
- **Accepted consequence:** `macp-core`'s API becomes formally part of `macp-runtime`'s
  public API for semver purposes. Under `version_group = "macp"` lockstep this is a
  duplicate signal, not a new constraint.
- **Why the timing mattered:** #114 publishes `commitment_hash` for the first time in
  0.7.0. Shipping it unreachable from the umbrella crate would push consumers to add
  `macp-core` to their manifests, and a later re-export would not remove a dependency they
  had already taken — it would just create a second canonical path forever.
- **Also corrected:** the overstated re-export claim lives in tracked, published
  `README.md`, not only in the gitignored `CLAUDE.md`.
- **Status:** CHANGED — code lands in the pre-release cleanup PR off `main`.

### D2 — Config-consistency guard made symmetric → **CONFIRMED** (with two fixes)

- **Assumed:** An explicit `DEFAULT > MAX` aborted startup, but the same operator error with
  `MAX` unset was silently clamped behind a `tracing::warn!` that vanishes under
  `RUST_LOG=off`. Made symmetric: abort in both cases.
- **Recommendation:** CONFIRM, and the override of the phase's own acceptance criterion was
  correct. `validate_env_config` contains **zero** clamp-with-warning paths — every sibling
  check aborts — so there was no prevailing convention to be inconsistent with. The
  criterion's stated rationale applies verbatim to the max-unset case; "both are set" was an
  under-specification, not a scoping decision.
- **Nuance the entry undersold:** the abort fires only when `DEFAULT` was *explicitly set*.
  Setting only `MAX=50` still boots and clamps. The line is therefore principled — abort
  when stated intent is unsatisfiable, clamp when nothing was stated — and cannot break a
  deployment that did not opt in.
- **Verdict:** Confirm the behavior; apply both fixes found during review.
  1. **Layer disagreement (real bug).** `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=0` was parsed in
     `src/main.rs` without a `> 0` filter, yielding a spurious second error naming an
     effective maximum of `0`, while `security.rs` treated the same value as the built-in
     1000. The filter is now mirrored; `MAX=0` produces exactly one accurate error.
  2. **Remedy clause** appended to the abort message, matching siblings that state the fix.
- **Corrected in the record:** "it simply stops firing for the binary" is wrong — the
  resolver clamp still fires for the binary when `MAX` is set and `DEFAULT` is not.
- **Verified:** both variables are genuinely new and unreleased (absent from
  `macp-runtime-v0.6.1` and `origin/main`), so the "no such deployment can exist" claim holds.
- **Status:** CONFIRMED.

### D3 — Env-var-to-field binding proof → **CONFIRMED**

- **Assumed:** A verifier transposed the two env values and the entire suite passed. Closed
  by a named struct plus an end-to-end Tier-1 test.
- **Recommendation:** CONFIRM — gap genuinely closed. Re-derived independently rather than
  trusting the phase log: `page_size_above_max_is_clamped` sets D=2/M=3, so correct wiring
  resolves to `(2, 3)` and a transposition to `(2, 2)`; the `page_size=1000` assertion then
  sees 2 where it expects 3 and fails. The 7/900 tests are transposition-blind, exactly as
  logged. Coverage also catches single-variable drops, double-reads, and a destructuring swap.
- **Why the first prescription was wrong (preserved deliberately):** because the resolver
  clamps `default = min(D, M)` and startup refuses `D > M`, correct and transposed wiring
  produce an *identical* default — so the originally-specified `page_size=0` test could not
  have detected a name swap. Detection requires an over-large request with D ≠ M.
- **Residual:** retuning that one test to reuse its neighbours' 7/900 values would silently
  erase the proof. The doc comment explaining the choice of 2 and 3 is the guard.
- **Status:** CONFIRMED.

### D4 — Startup config errors folded into the returned `Err` → **CONFIRMED**

- **Recommendation:** CONFIRM, no follow-up. The stdout-default claim was verified against
  `tracing-subscriber`'s source and by running the binary. The change **removed** an
  inconsistency: `validate_env_config` was the sole fatal path among six whose actionable
  detail existed only in a `tracing` event and could be silenced by a filter.
- **Rejected:** switching the subscriber's writer to stderr globally. It would not satisfy
  the acceptance criterion at all — `RUST_LOG=off` suppresses the event before any writer is
  consulted — while changing the stream of every log line the server emits.
- **Status:** CONFIRMED.

### D5 — Startup-gate tests must poll `try_wait` → **CONFIRMED** (with a correction)

- **Recommendation:** CONFIRM the pattern, do the follow-up, but narrow it. The hang
  mechanism was reproduced empirically; a regression costs a 15-minute CI step timeout with
  no test attributed.
- **Correction to the record:** the entry claimed both pre-existing tests carried the same
  hazard. **Only one did.** `startup_refuses_invalid_policies_dir` sets
  `MACP_ALLOW_INSECURE=1`, so nothing but the policies-dir check stands between it and a
  running server, and its plausible regression is mundane ("skip invalid policy files and
  warn"). `startup_refuses_without_auth_or_insecure_flag` has a structural backstop — the
  independent TLS gate in `src/main.rs` — so weakening the auth gate makes it fail cleanly.
- **Verdict:** Convert the policies-dir test to the bounded helper; leave the auth test on
  `output()` but add `env_remove` for the two TLS paths, whose ambient presence would remove
  its backstop; document the helper's pipe constraint.
- **Known and accepted:** the helper drains its pipes only after exit. Measured output is a
  few hundred bytes against a ≥16KB buffer (~28x headroom), so this is latent, not live, and
  is now named in the doc comment so a future startup-config dump does not cross it silently.
- **Status:** CONFIRMED.

### D6 — `CLAUDE.md` is gitignored → **CONFIRMED** (repo owner's call)

- **Assumed:** The plan treated `CLAUDE.md` as one of four mirrored env-var tables, but it is
  gitignored and has never been tracked, so its edits never reach a clone.
- **Verdict:** Repo owner's decision — **it stays gitignored.** No recommender was needed.
- **Accepted consequence:** three tracked tables (`README.md`, `docs/deployment.md`,
  `docs/API.md`) stay in sync; the local `CLAUDE.md` copy silently diverges for anyone
  cloning fresh. Adding a ~370-line agent-instruction file to the tracked tree is a
  repo-policy change out of scope for this feature.
- **Status:** CONFIRMED.

**Summary:** 5 confirmed, 1 changed, 0 deferred. The change (D1) is additive and
semver-compatible; it must land before #114 publishes 0.7.0.

---

## plans/backlog-closeout-2026-09.md — G4 release shape

### D7 — G4 releases as **0.8.0**, and spends the forced break on `#[non_exhaustive]` → **CONFIRMED (2026-09-11, repo owner's call)**

- **Found:** by the Phase 10 verifier, as a BLOCKER, and confirmed by running
  `RUSTC_WRAPPER="" cargo semver-checks check-release --workspace` directly:

  ```
  --- failure constructible_struct_adds_field ---
    field HandoffOfferRecord.suspended_ms_at_offer
        crates/macp-modes/src/mode/handoff.rs:49
  Summary semver requires new major version: 1 major and 0 minor checks failed
  ```

  `HandoffOfferRecord` is publicly reachable via `macp_modes::mode::handoff` with all-pub
  fields and no `#[non_exhaustive]`, so Phase 9's added field breaks any external exhaustive
  struct literal. `release-plz.toml` sets `semver_check = true`, so this **blocks the release
  PR**; all seven crates share one `version_group`, so it moves the whole family.

  **Corrected 2026-09-13 (Phase 13 verify round).** The capture above was taken mid-G4 and
  is only half the picture: it names one struct in one crate, and an earlier reading of this
  record took both forced breaks to be on `HandoffOfferRecord`. They are not. Checked against
  the published `.crate` sources for 0.7.6 (`macp-modes-0.7.6/src/mode/handoff.rs`,
  `macp-storage-0.7.6/src/registry.rs`), 0.7.6 **already ships** `HandoffOfferRecord.offered_at_ms`,
  `PersistedSession.semantics_rev` and `PersistedSession.max_suspend_ms` — those were released,
  not forced by this branch. Relative to the 0.7.6 baseline this branch adds exactly **one
  field to each of two different structs in two different crates**:

  | Crate | Struct | Added field |
  |---|---|---|
  | `macp-modes` | `HandoffOfferRecord` (`crates/macp-modes/src/mode/handoff.rs`) | `suspended_ms_at_offer` |
  | `macp-storage` | `PersistedSession` (`crates/macp-storage/src/registry.rs`) | `suspension_intervals` |

  The correction does not touch the verdict: two forced `constructible_struct_adds_field`
  majors in two crates is, if anything, a stronger case for spending 0.8.0 once, and the
  lockstep `version_group` means one break and two cost the same.
- **Why there is no route back to 0.7.x:** for a `0.x` crate the minor position acts as major,
  so a major break means **0.8.0**. Every alternative is *also* a major break — adding
  `#[non_exhaustive]`, making the struct private, or making its fields private. Storing the
  state elsewhere does not help either: `HandoffState` has the same all-pub shape, and
  Phase 11's plan requires "a discriminator surviving serde round-trip on every backend",
  i.e. at least one more field on this same struct. **The break is unavoidable in G4.** The
  only real question was how to spend it.
- **Verdict:** take 0.8.0 **and** add `#[non_exhaustive]` to the handoff (and quorum)
  mode-state records in the same release.
- **Reasoning:** these records are internal `mode_state` serialization detail that external
  callers have little reason to construct by literal, and the workspace already establishes
  the pattern everywhere it matters — `Session` (`macp-core/src/session.rs:64`), `MacpError`
  (`error.rs:5`), `ModeResponse` (`mode.rs:11`), `PolicyFileOutcome`
  (`macp-policy/src/registry.rs:67`). The mode-state records are the exception, not the rule.
  Since a major bump is being spent regardless, spending it once to end the class is strictly
  better than spending it now on the field alone and again on the next persisted-state field.
- **Accepted consequence:** external code constructing `HandoffOfferRecord`/`HandoffState`/the
  quorum records by struct literal breaks at 0.8.0 and must move to whatever constructor is
  provided. This is a real break, deliberately taken, and belongs in the changelog as such —
  not as a bugfix.
- **Where it lands:** the `#[non_exhaustive]` attributes belong in **Phase 13** (the G4
  release close-out), as their own commit, so the behaviour change in Phase 11 stays
  bisectable from the API change. Phase 11 may add fields freely in the meantime.
- **Executed scope (2026-09-13):** "end the class" was taken literally, as the reasoning
  above requires — the first pass sealed only the 6 handoff/quorum records plus
  `PersistedSession` and left 11 mode-state records of the same class unsealed, which would
  have spent 0.8.0 and still left the next `ProposalState`/`MultiRoundState` field to force
  another major. **18 structs** are sealed: `HandoffOfferRecord`, `HandoffContextRecord`,
  `HandoffState`, `ApprovalRequestRecord`, `BallotRecord`, `QuorumState`, `ProposalRecord`,
  `TerminalRejectRecord`, `RejectRecord`, `ProposalState`, `TaskRecord`, `TaskRejectRecord`,
  `TaskUpdateRecord`, `TaskCompleteRecord`, `TaskFailRecord`, `TaskState`, `MultiRoundState`
  (all `macp-modes`) and `PersistedSession` (`macp-storage`). The added records are the same
  class on the same evidence: `ProposalState.rejections`, `ProposalState.phase`,
  `MultiRoundState.convergence_type` and `MultiRoundState.converged` all carry
  `#[serde(default)]`, i.e. each was added after the fact and each would be a major today.
  No struct-literal construction of any of them exists outside `macp-modes` — audited with
  `git grep` across the workspace, `tests/`, and the separate `integration_tests/` workspace
  that consumes these crates as path deps (the true external-caller position); every match
  is inside the defining file.
- **Deliberately NOT sealed:** the five `macp-core` decision types (`DecisionState`,
  `Proposal`, `Evaluation`, `Objection`, `Vote`). `macp-core` is *vocabulary*, and these are
  the argument types of the public `PolicyEvaluator` trait — the seam a consumer driving
  `macp-core` + `macp-modes` with its own evaluator sits on. Unlike the mode-state records
  they **are** literal-constructed across crate boundaries today:
  `crates/macp-modes/src/mode/decision.rs` builds all five in production code
  (`default_state()` and the four `on_message` arms), and `crates/macp-policy/src/evaluator.rs`
  builds `DecisionState` fixtures in its tests. `DecisionState` derives no `Default`, so a
  downstream evaluator implementor would have **no** way to build a fixture for their own
  trait impl. Sealing them is therefore not free the way the mode-state records are: it needs
  constructors/builders designed first. Left open deliberately, not overlooked.
- **Enums stay unsealed:** `HandoffDisposition`, `BallotChoice`, `ApprovalThreshold`. A `_`
  arm silently reinterpreting a future governance variant is issue #145's defect class, and
  `enum_variant_added` is already a major lint that blocks the release PR.
- **Status:** CONFIRMED (2026-09-11); factual premise corrected and executed scope recorded
  2026-09-13.

## 2026-09-20 — `plans/parity-contract-176.md` closeout (1 entry)

Reconciled at the end of the plan, before `/ship`. One `UNCONFIRMED` entry, low blast
radius (no public contract, schema, auth model, migration, or external dependency) — Opus
tier, no Fable escalation.

### D8 — Phase 1 semver-check acceptance criterion verified by manual inspection, not the tool → **CONFIRMED**

- **Assumed:** Phase 1's acceptance criterion (a clean `cargo semver-checks check-release
  --workspace --baseline-version 0.8.0`) couldn't be verified locally because the installed
  `cargo-semver-checks` 0.45.0 crashes on this repo's rustdoc v57 output
  (`error: unsupported rustdoc format v57 for file... (supported formats are v53, v55,
  v56)`), so the session substituted a fresh-Opus manual API-delta inspection (confirmed
  purely additive: ten `"1.0"` literal-to-constant replacements, two `fn` →
  `#[doc(hidden)] pub fn` visibility changes, four new constants — no removals, renames,
  signature changes, field additions, or visibility narrowings) and deferred the automated
  check to CI, citing PR #173 as precedent.
- **Analysis:** Confirmed CI does not share the crash — `.github/workflows/release-plz.yml`
  pins `release-plz-action` at commit `b5543c19b03be9bd48852d20ca89f478b7723260`
  ("v0.5.132"), whose `action.yml` (fetched directly) installs `cargo-semver-checks@0.50`
  via a separate `taiki-e/install-action` step, decoupled from this repo's
  `rust-toolchain.toml`. A real, closed upstream issue (`release-plz/release-plz#3018`,
  filed by a sibling project hitting the identical crash class) documents that
  `cargo-semver-checks` 0.48 already supports rustdoc v56/v57 — past the v56 ceiling of the
  locally-stale 0.45.0 — so 0.50 almost certainly does too. PR #173's cited precedent is
  weaker than the original entry framed it: that PR's own body states "no production code
  changed... so there is no semver surface to check regardless," meaning it never actually
  exercised the CI gate against a real diff — Phase 1's diff is the first real test of it.
  Investigation also surfaced a genuine but inapplicable residual: this repo's
  release-plz-action defaults to release-plz core `0.3.161` (cut 2026-09-03), which predates
  `release-plz#3021`'s fix (merged 2026-09-08) for a separate classifier bug where a
  *minor*-only deny-lint violation from a completed cargo-semver-checks run can misreport as
  "compatible." Irrelevant to this diff (verified purely additive, no lints of any kind
  would fire), but worth a maintainer follow-up to bump the action's `version:` input past
  `0.3.161` independently of this plan.
- **Decided by:** Opus (`/reconcile`).
- **Verdict:** CONFIRMED — deferring to CI is the strongest available option and is not a
  false safety net; the specific local crash mode does not carry into CI's independently
  pinned, newer tool version.
- **Status:** CONFIRMED (2026-09-20).

## 2026-09-22 — `plans/backlog-closeout-2026-09.md` closeout (39 entries)

Reconciled a week-plus after the plan itself shipped (all 14 phases `DONE`, released as
0.8.0 on 2026-09-20) — the code, not just the plan, has been live in production. Six fresh
Opus subagents analyzed the 39 `UNCONFIRMED` entries in parallel, each re-verifying its
assigned entries' `file:line` citations against current code and re-running the relevant
test suites (macp-core, macp-modes, macp-policy, macp-runtime — all green, several
hundred tests, every cited test confirmed present and passing). None of the 39 turned out
to be a genuine one-way door requiring escalation — all Opus tier, no Fable. Two small,
non-wire-visible code fixes surfaced and were applied in this same pass (below); the rest
are confirmations, several with a stale-citation or scope correction folded in. The
remaining 4 `UNCONFIRMED` entries in `ASSUMPTIONS.md` (2 tagged `plans/spec-99-schema-version-3.md`,
2 tagged "spec #126 alignment") are out of scope for this pass and left for a future
`/reconcile` run scoped to those plans.

**Fixes applied in this pass:**
- `src/runtime.rs`: the `Err(_)` arm of `resume_session` logged a generic "session
  force-expired" with no way to tell `MAX_SUSPEND_MS` (duration cap) from
  `MAX_SUSPENSION_CYCLES` (rev≥2 cycle-count cap) apart, even though `Session::resume`
  mutates both fields before returning `Err` and they were sitting right there. Added a
  `tracing::warn!` naming both booleans and both raw counts. Observability-only, no
  behavior or wire change.
- `src/replay.rs:184-186`: `validate_replay_consistency`'s rustdoc field list stopped at
  `suspended_at_ms` and never mentioned the 8th comparison, `suspension_intervals`, that
  the function has compared since Phase 11b. Doc-only.
- `src/runtime.rs:3316-3341` and `:3519-3537`: two test doc comments describing 11d/11e/
  Phase 12 as future work, written when they still were. Rewritten to describe the shipped
  state — the client-boundary double-guard is now the reserved-`message_id` check in
  `dispatch_implicit_accept` (`crates/macp-modes/src/mode/handoff.rs:733-740`), not "11d
  restructuring that arm"; the eager sweep is `sweep_due_synthetic_accepts`
  (`src/runtime.rs:1438`), not "Phase 12's caller." Doc-only.

Non-code follow-ups noted below, not actioned in this pass (each is small and independent;
listed so they aren't lost): a spec clarification for RFC-MACP-0011 §4a's literal
zero-ballot reading (D11); the `src/replay.rs:63-67` checkpoint-bypasses-registry-validation
caveat, shared by D10 and D12; a distinguishing line in `CHANGELOG.md`'s 0.8.0 `[Unreleased]`
follow-up (not the released section, which release-plz regenerates) noting the
`message_count` consequence of D26's `record_participant_activity` skip.

### D9 — Quorum `threshold.value = 0` means two different things in the two layers → **CONFIRMED**
The 2026-09-11 narrowing holds: `crates/macp-policy/src/registry.rs:516-527` refuses a
*supplied* `threshold.value <= 0` at registration, so `EffectiveThreshold::Inert` is reachable
only by omitting the key. The residual divergence (mode falls back to `required_approvals`,
evaluator applies no bar) is real but inert — the mode is strictly the stricter layer and
gates first (`crates/macp-modes/src/mode/quorum.rs:477-480`), so the lax evaluator reading can
never seal something the mode refused. Pinned by the matrix test's own carve-out comment
(`quorum.rs:1822-1826`).

### D10 — The quorum mode silently tolerates a rules object the evaluator rejects → **CONFIRMED**
Still fail-closed: `quorum.rs:220-221`'s `unwrap_or_default()` yields an inert threshold on a
malformed rules object, while `evaluator.rs:43-52` denies the same object outright — a
confusing error, not a hole. New reachability note: `src/replay.rs:63-67` restores a
checkpoint's inline `policy_definition` verbatim without re-validating it, so a definition
written by an older binary can reach this path on restart, bypassing registration entirely
(shared with D12).

### D11 — A zero-ballot quorum decline is refused even though RFC-MACP-0011 §4a permits it → **CONFIRMED**; spec clarification recommended (non-blocking)
Both guards ship and are tested: the `ApprovalRequest` domain check (`quorum.rs:394-410`,
refuses an effective threshold outside `1..=participants`) and the `counted > 0` conjunct
(`quorum.rs:328`). Wire-visible and shipped in 0.8.0 with a migration note
(`docs/deployment.md:50-58`). The departure from a literal §4a reading is recorded only in
this repo's own docs (`docs/policy.md:212`) — worth filing upstream as a spec clarification
that an empty ballot box is a misconfiguration, not a decision, to ratify the departure
rather than leave it local.

### D12 — Wildcard (`mode: "*"`) policies are now held to every mode's schema → **CONFIRMED**
`registry.rs:386-390` fans a `"*"` policy across all standards-track modes; both conditional-
constraint families gate on `matches!(mode, "…" | "*")`. `quorum_threshold_constraints_apply_to_wildcard_policies`
(`registry.rs:1592-1646`) pins all four cases. The entry's "did not materialise in any test"
prediction held. Shares D10's checkpoint-bypass caveat.

### D13 — `EffectiveThreshold` is deliberately not `#[non_exhaustive]` → **CONFIRMED**
Exactly two match sites workspace-wide, both exhaustive (`quorum.rs:222-231`,
`evaluator.rs:1037-1050`) — the premise still holds. The reasoning now lives in the code
(`crates/macp-core/src/policy/rules.rs:273-287`) and is consistent with `CLAUDE.md` §8a's
repo-wide "enums stay unsealed" rule for the same reason. Revisit trigger (a third external
consumer) has not fired.

### D14 — A negative weighted total fails the round, which moves one decline from DENY to ALLOW → **CONFIRMED**, fully closed
The short-circuit (`evaluator.rs:679-684`) and all three tests pass, asserting the
`VotingResult` variant directly. The release-notes obligation is discharged
(`docs/deployment.md:61`, `docs/policy.md:142`). The deferred half is no longer deferred:
spec #99 / RFC-MACP-0012 §4.1 has since made zero decisive weight normatively `NoVotes`,
now cited directly in `evaluator.rs:685-697`.

### D15 — `supermajority` silently substitutes 2/3 for an out-of-domain threshold → **CONFIRMED**
Arm unchanged (`evaluator.rs:626-631`); the registry door is now tighter than when written —
`registry.rs:468-471` refuses `supermajority` with `threshold <= 0.5` (which also refuses an
omitted threshold, since the default is `0.5`), pinned by two registration tests. Substitution
branch is dead through every registered path.

### D16 — `unanimous` passes by vacuous truth on an empty participant list → **CONFIRMED**; rationale corrected
Code unchanged (`evaluator.rs:647-664`) and now *more* clearly right, but for the opposite
reason recorded originally. RFC-MACP-0012 §4.1 has since ratified this exact case in the
runtime's favor. The entry's "unreachable — `SessionStart` requires non-empty participants"
claim is now **false**: `allows_empty_participants` (`crates/macp-core/src/session.rs:574-576`)
admits it for Decision. Reachability is instead preserved by `DecisionMode::authorize_sender`
forbidding every `Vote` over an empty roster, pinned by
`zero_participant_unanimous_is_no_votes_not_a_pass` (`evaluator.rs:1841-1906`).

### D17 — The public effective-threshold accessor answers three questions in three layers, not one `Option` → **CONFIRMED**
Shipped shape matches exactly (`crates/macp-modes/src/mode/quorum.rs:144-290`), now published
API on crates.io since 0.8.0. `tests/quorum_threshold_public_api.rs` exercises all three
layers from outside the crate. Closest of this cluster to a one-way door, but the door shut
at the 0.8.0 publish with no defect behind it — narrowing later is a major, but nothing
argues for churning it.

### D18 — The rev-2 scaffolding branch cannot be *literally* identical to the rev-1 branch → **CONFIRMED**; one detail corrected
The seam (`crates/macp-modes/src/mode/handoff.rs:169-226`, `rev2_elapsed_ms`) paid for
itself — it's where the suspension-correction term now lives, with its own rustdoc and unit
tests. Correction: the `const _: () = assert!(..)` form did not survive; it shipped as a
plain `assert!` under `#[allow(clippy::assertions_on_constants)]`
(`crates/macp-core/src/session.rs:1402-1405`, fixed in 7815a97, whose message notes CI's
rust-cache had been serving a stale `target/` and never re-linted the file).

### D19 — Discharging Phase 10's acceptance criterion 3 when no conformance fixture exists → **CONFIRMED**
`assert_replay_equivalence` (`tests/conformance_loader.rs:356`) is still called only from the
vendored-fixture loop, and no local fixture is addable (CI byte-diffs `tests/conformance/`
against pinned `SPEC_REV` in both directions). Discharged through the real `replay_session`
path instead (`src/replay.rs`, tests named rather than cited by line since they've drifted
~300 lines: `legacy_rev1_handoff_history_with_suspension_still_implicitly_accepts`,
`rev2_handoff_history_implicitly_accepts_on_unsuspended_time`,
`current_rev_handoff_history_replays_identically_to_rev1`).

### D20 — Updating `CURRENT_SEMANTICS_REV`'s rev-2 doc bullet outside Phase 10's Files list → **CONFIRMED**
Doc-only; the bullet (now at `crates/macp-core/src/session.rs:77-98`) accurately describes
both halves of the revision with no stale "currently identical to revision 1" wording. Still
the only place semantics revisions are enumerated.

### D21 — Omitting the in-flight suspension term while the implicit-accept check is lazy-only → **CONFIRMED**; forward constraint resolved
The entry's own "Phase 12 must resolve this" held: Phase 12 shipped the eager sweep and chose
*skip suspended sessions entirely* over adding an in-flight term
(`sweep_due_synthetic_accepts` at `src/runtime.rs:1464-1466`, re-checked independently in
`synthesize_due_accept` at `:727-729`). Standing invariant to carry forward: any future caller
of `due_synthetic_envelope` / `unsuspended_deadline` must filter to `Open`, or add the
in-flight term.

### D22 — Synthetic accept stands even when the triggering message is later rejected → **CONFIRMED**; blast radius narrowed
Ordering is load-bearing and explicit: synthesis (`src/runtime.rs:876`) happens before the
trigger's `mode.on_message_at` (`:878`), while the trigger's dedup slot is consumed only by
`step::commit` (`:895`) — a later `Err` provably leaves `message_id` free. Both mitigations
shipped: carve-out in `CONTRIBUTING.md:48-68`, acceptance tests both in-process
(`tests/handoff_implicit_accept_live.rs:698`) and on the wire
(`integration_tests/tests/tier1_protocol/test_handoff_implicit_accept.rs:584`). Correction:
synthesis sits after `authorize_sender` and `validate_client_envelope`, so the trigger must be
an authenticated, authorized participant — the entry's stated blast radius is wider than the
code actually allows. Record this at normative-carve-out weight (per `CLAUDE.md`'s
freeze-profile section), not as an implementation note.

### D23 — Persisted suspension intervals, with a cycle cap, rather than a derived deadline → **CONFIRMED**
Fully realized (`crates/macp-core/src/session.rs:171,290-298`,
`crates/macp-storage/src/registry.rs:70-163`). `MAX_SUSPENSION_CYCLES = 1024` is a `pub const`,
so raising it later is non-breaking. The cap-cause logging gap this entry implied is the fix
applied above.

### D24 — The `implicit` payload flag as discriminator, guarded by a mode-trait boundary hook → **CONFIRMED**
Hook exists with fail-open default (`crates/macp-modes/src/mode/mod.rs:100`) and the handoff
override (`crates/macp-modes/src/mode/handoff.rs:276-297`); both live entry points covered
(`src/runtime.rs:500,865`, funnelled from `server.rs:449,872`). The specific mitigation
requested shipped verbatim — the rustdoc hazard at `mode/mod.rs:84-99` uses the runtime itself
as the worked example. `MacpError::InvalidPayload` and `InvalidEnvelope` both map to wire code
`"INVALID_ENVELOPE"` (`crates/macp-core/src/error.rs:58,66`), confirming the "rev-2 error
surface does not shift" claim.

### D25 — Retiring the interim implicit-accept path fail-loud rather than fail-open → **CONFIRMED**, no longer an assumption
Its entire safety argument was "phases 10-13 ship as one PR and one release" — now verified
fact: `7c652b6` (#171) is the single commit, first released as 0.8.0 (`f97fd15`, tag
`macp-runtime-v0.8.0`, 2026-09-20). No 0.7.x release ever published
`CURRENT_SEMANTICS_REV = 2`, so no rev-2 history can predate the synthetic entry.

### D26 — The synthetic commit deliberately skips `record_participant_activity` → **CONFIRMED**; changelog follow-up noted
Skip is correct: `record_participant_activity` is called only from
`crates/macp-modes/src/step.rs:104`, and `synthesize_due_accept` bypasses `step::commit`
entirely, doing dedup insert and `apply_mode_response` by hand (`src/runtime.rs:764-765`).
Pinned by `synthetic_accept_is_not_credited_as_participant_activity`
(`src/runtime.rs:3676`). Gap: the `message_count` consequence was never noted anywhere
durable — belongs in `CHANGELOG.md`'s `[Unreleased]` section or a fresh entry here, not a
hand-edit of the released 0.8.0 section (which release-plz regenerates from commit history).

### D27 — Counting granularity of the widened `validate_replay_consistency` → **CONFIRMED**
Three independent `if` blocks, three increments, three warn lines (`src/replay.rs:244-274`),
exactly as chosen. Blast radius nil — the count feeds one log line plus a Prometheus counter,
never a branch. Addressed by the rustdoc fix applied above.

### D28 — `cancel_session` reads a clock solely to stamp its log entry → **CONFIRMED**
Unchanged: a single `Utc::now()` (`src/runtime.rs:1027`) after the terminal-state early
return, matching `Session::cancel()` taking no timestamp. Lowest-consequence entry in the
cluster — zero wire surface.

### D29 — `suspension_intervals` added to `validate_replay_consistency` (11b's "optional fourth comparison") → **CONFIRMED**; doc fix applied
Live at `src/replay.rs:266-274`, warn-only, single caller (`src/main.rs:351`). The rustdoc
field-list gap this entry left open is the fix applied above.

### D30 — Criterion 3's checkpoint fixture binds no `policy_version` → **CONFIRMED**
The trap is real and still present (`src/replay.rs:63-70`); the sibling test
`replay_from_checkpoint_restores_suspension_intervals` (`:742-822`) avoids it correctly via a
non-vacuous tripwire. Bonus: `replay_from_checkpoint_restores_state`'s vacuity, flagged but
not fixed by the original entry, has since been closed with the same tripwire technique.

### D31 — `Session::resume`'s stray doc comment re-attached → **CONFIRMED**
Rustdoc-only. `resume`'s doc (`crates/macp-core/src/session.rs:258-270`) now names both caps
and documents the record-before-check ordering the code at `:290-298` actually depends on.

### D32 — A degenerate suspension pair (`e < s`) counts as a zero-width pause at `s` → **CONFIRMED**, verified algebraically
Checked rather than trusted: the clamp `cur = e.max(s).max(cur)` (`session.rs:422`) keeps the
per-pair contribution in `[0, max(e-s,0)]` in every case (degenerate, overlapping, unsorted),
so over-reporting is structurally impossible. `unsuspended_deadline_never_over_reports_on_adversarial_pairs`
(`session.rs:1326-1391`) covers backwards, nested, overlapping and unsorted pairs.

### D33 — Nothing below `semantics_rev` 2 reads `suspension_intervals`, so dropping the overflow is invisible → **CONFIRMED**
`unsuspended_deadline` has exactly one non-test caller (`handoff.rs:440`, itself gated
`semantics_rev < 2 → None`), and no code path ever raises a session's `semantics_rev` (only
writers: replay from the recorded `SessionStart`, and the `SessionStart` builder itself,
pinned by `replay_preserves_recorded_semantics_rev`). A rev≤1 session can never later become a
rev≥2 reader of its own truncated vec.

### D34 — Committing a replay test that pins today's *pre-11d* refusal of the synthetic shape → **CONFIRMED**
Played out exactly as designed and was already consumed: the test
(`src/replay.rs:1660`, `synthetic_shaped_entry_reaches_dispatch_not_the_client_boundary`) was
flipped by 11d from `Err(InvalidPayload)` to `Ok` with an implicit-accept assertion, precisely
the failure direction the entry engineered.

### D35 — Keeping a runtime-level assertion that is double-guarded (and saying so) rather than dropping it → **CONFIRMED**; doc fix applied
The assertion (`src/runtime.rs:3343`) still passes and its non-vacuous isolation still lives
in the mode-level unit test. The doc-comment fix applied above (naming the current
`message_id`-check guard instead of a predicted-but-unrealized 11d restructuring, and the
shipped eager-sweep location) is this entry's resolution.

### D36 — Flipping an `src/replay.rs` test in a phase whose file list names only the mode crate → **CONFIRMED**
"The plan is a document; the failing test is the fact" was the right call. The rewritten
rustdoc (`src/replay.rs:1641-1658`) now describes the post-11d state honestly and remains the
only end-to-end proof through `replay_session` that the synthetic entry is accepted as data.

### D37 — One shared `IMPLICIT_ACCEPT_REASON` const instead of two copies of the literal → **CONFIRMED**, stronger now
`crates/macp-modes/src/mode/handoff.rs:52`, used by both the synthesizer and the interim
in-`Commitment` arm — drift is structurally impossible. Since written, the string has become
externally published (`docs/modes.md:95`, the TypeScript SDK handover plan, the live test
harness), raising the cost of drift and retroactively justifying the const.

### D38 — Error codes and check order inside the rev-2 implicit-accept arm → **CONFIRMED**, explicitly not wire-visible
Verified line by line (`handoff.rs:712-743`): rev gate → offer lookup → sender → `accepted_by`
match → `message_id` → disposition, unchanged. At `semantics_rev >= 2` no client envelope with
`implicit = true` can reach this arm at all (refused earlier, `handoff.rs:286-295` via
`src/runtime.rs:865`); at rev <= 1 the arm returns `InvalidPayload` unconditionally,
byte-identical to pre-0.8.0. The only observers are replay and direct library callers, exactly
as predicted.

### D39 — A fifth test, beyond the four acceptance criteria, to make the phase's negative rule killable → **CONFIRMED**
Both extra tests exist and pass:
`implicit_accept_dispatch_does_not_reverify_the_deadline` (`handoff.rs:2834`) reproduces the
negative-scalar state exactly as claimed, and `due_synthetic_envelope_returns_none_unless_an_offer_is_due`
(`handoff.rs:2621`) covers all seven listed edges plus undecodable `mode_state`.

### D40 — `debug_assert!(suspended_at_ms.is_none())` placed inside `due_synthetic_envelope` → **CONFIRMED**, escalation already acted on
The entry's own late correction — that a `debug_assert!` cannot protect release builds, so
11e's non-`Open` filter is load-bearing — was carried through. Enforcement that ships is
`src/runtime.rs:727-729`, documented as a correctness gate (`:652-661`), with its own test
`synthesis_is_skipped_for_a_non_open_session` (`src/runtime.rs:3536`) proving concrete harm
would occur without it. The `debug_assert!` correctly stays as the mode-level tripwire.

### D41 — Four handoff-mode tests broke that the plan's "complete" sweep had cleared, and two replay tests besides → **CONFIRMED**
Independently corroborated by two subagents against the live, green workspace suite. No
disposition change.

### D42 — A terminal checkpoint made the live-replay criterion pass with the synthetic entry deleted → **CONFIRMED**

### D43 — Two storage backends in the live harness, chosen for what each one cannot do → **CONFIRMED**

### D44 — `ModeRef::due_synthetic_envelope` returns `None` for a mode that has vanished → **CONFIRMED**

### D45 — The freeze-profile invariant amended in `CONTRIBUTING.md`, with the carve-out spelled out → **CONFIRMED**
Consistent with `CLAUDE.md`'s own "Freeze-profile priorities" section, which documents the
same carve-out (gitignored `CLAUDE.md` leaves `CONTRIBUTING.md:48-69` as the sole durable
copy visible to a fresh checkout).

### D46 — Tier-1 bounds the synthetic's deadline timestamp rather than leaving it unasserted → **CONFIRMED**; bound description corrected
The shipped test is *stronger* than the entry's shorthand in both directions — the lower
operand is a clock read taken before two RPCs (sound but not millisecond-tight), and the
upper operand is read 300ms before the commitment is sent (300ms stronger than claimed). A
second bounded pair exists in the Phase 12 sweep test (`test_handoff_implicit_accept.rs:898-908`)
that the entry didn't mention.

### D47 — Phase 11f ships five tier-1 tests where the plan specified four → **CONFIRMED**; citations corrected
The mutation-kill argument is real: `src/server.rs:526` replays a history snapshot at
subscribe time with no gap-fill re-read, so dropping the publish would hand an attached
subscriber the Commitment first. Corrections: the `runtime.rs:775` citation was wrong when
written (the file introduced the call at `:795` in the same commit), not drifted since; "all
124 tier-1 tests" should read 121.

**Cluster summary:** 39/39 CONFIRMED, 0 CHANGE requiring more than the 2 code fixes above, 0
DEFER, 0 ESCALATE. Two items flagged for someone's attention outside this reconcile pass: D11
(a spec clarification worth filing upstream for RFC-MACP-0011 §4a) and D26 (an
`[Unreleased]`-section changelog note still to write).

- **Decided by:** Opus (`/reconcile`, 6 parallel subagents, Opus tier throughout).

### D48 — `fromJSON(releases)[0].version` indexing instead of a `jq select()` on `package_name` → **CONFIRMED**
Stronger than the entry's own framing: every one of the seven crate manifests declares
`version.workspace = true`, so there is structurally only one version string in the repo to
read, not merely a CI-enforced convention that could drift. `ci.yml:312-342`'s lockstep
assertion blocks any divergence from ever reaching `main` in the first place, and the cited
safety net (`docker.yml:221-237`'s Cargo.toml cross-check) was confirmed to have actually run
and passed on Phase 2's real backfill dispatch, not just in a dry run.

### D49 — Backfilling only `:0.8.1`, not `:0.8.0`, for issue #184's acceptance criterion 2 → **CONFIRMED**
Phase 2 has since actually run (`gh workflow run docker.yml --ref main -f
ref=macp-runtime-v0.8.1`, run `36169029979`): all 4 acceptance criteria passed against the
live GHCR registry, no edge case triggered, ~12 minutes wall-clock. Nothing in that evidence
surfaces a reason to also image `0.8.0`; AC2 asked for "the current release," which is what
shipped. The reversal path (dispatch `0.8.0` before any future `0.8.1` rebuild, to avoid
dragging the moving `0.8` tag backwards) remains available and correctly documented.

### D50 — Accepting a ~45-minute release-run concurrency hold over an out-of-band PAT-driven trigger → **CONFIRMED (2026-09-25, Phase 5 observation)**
Shipped code matches the plan's reasoning exactly (`docker.yml:64-68`'s `timeout-minutes: 90`
bound, `release-plz.yml:11-13`'s unchanged concurrency group, `docker` as a true sibling of
`publish` so a slow/failed image build can't cost a crates.io release) and nothing
contradicts it. But the specific scenario reasoned about — two concurrent cold multi-arch
builds racing for the same commit inside the release run's group — hasn't been observed yet:
no release has been cut since PR #191 merged, so Phase 2's single ~12-minute dispatch is
encouraging but not dispositive. This is exactly what Phase 5 ("Observe the next real
release," still `plans/docker-tag-trigger-184.md` Phase 5, `TODO`) exists to settle. Re-
evaluate once Phase 5 records a real automatic release run's wall-clock, ideally across a
few releases, to confirm the queueing stays occasional rather than becoming routine.

**Update — Phase 5 has now recorded a real automatic release run.** Merging PR #185
("chore: release v0.8.2") triggered `release-plz.yml` run `36194214828`: the `docker` job's
"Build and push" step ran 38m55s (21:57:57Z-22:36:52Z), for a total release-run wall-clock
of 40m34s — inside the plan's own 35-50 minute projection and under its ~45-minute headline
figure, well under the 90-minute timeout bound. `docker-version-guard` and `publish` both
completed in under 2 minutes, running concurrently with the slow image build, confirming
the sibling-job isolation held: the crates.io publish was not delayed by the image build.
No hung build, no timeout, no GHCR permission failure. Full per-job timestamps and GHCR
verification are in `plans/docker-tag-trigger-184-PROGRESS.md`'s Phase 5 checkpoint. One
data point is not "a few releases," so the trend (does queueing stay occasional as release
cadence continues) is still worth a glance at the next release or two, but the core
assumption — that the hold lands in the projected band and does not regress to the
360-minute default or starve `publish` — is now directly confirmed rather than merely
reasoned about. No follow-up action needed; nothing to change.

- **Decided by:** Opus (`/reconcile`, 1 subagent analyzing all three entries together — all
  explicitly low/reversible blast radius per the plan's own "Long-term posture" section, no
  entry rose to a one-way door or trust boundary requiring escalation). Re-confirmed by
  Opus directly against the live Phase 5 evidence above (no subagent needed — this is a
  factual observation against an already-decided, low-blast-radius assumption, not a new
  judgment call).

### D51 — Fixing `parse_contribute_value`'s canonical-proto/JSON collision (issue #192) via `semantics_rev`-gating, not an unconditional global change → **CONFIRMED**
`parse_contribute_value` (`crates/macp-modes/src/mode/multi_round.rs`) mis-decodes a canonical
protobuf `Contribute` payload as legacy JSON at value byte-lengths 13, 32, and 123 — the proto
tag/length-prefix bytes happen to be JSON-insignificant whitespace (or, at 123, the literal
`{`), exposing the value's own bytes to `serde_json` and returning a wrong substring with no
error anywhere. An unconditional fix would change what an already-persisted, already-accepted
byte sequence decodes to on every future replay — a genuine one-way door under RFC-MACP-0003
§1's replay-determinism constraint (`src/replay.rs`'s `replay_entry` re-derives state from the
original accepted bytes, byte-for-byte, on every restart).

This was routed to a scoped Fable consult (`plans/parse-contribute-value-192.md`'s "Open
questions" section) before any code was written, per this repo's Autonomy ladder (a one-way-
door-shaped call on a public/persisted-data question). Fable's recommendation — gate the fix
behind `session.semantics_rev >= 3` (a new revision on this repo's existing, twice-precedented
`Session::semantics_rev`/`CURRENT_SEMANTICS_REV` mechanism, `crates/macp-core/src/session.rs`),
rather than an unconditional global change (which would silently corrupt legacy replay) or a
`MultiRoundState`-local flag (rejected: `on_session_start` re-mints `MultiRoundState` fresh on
every replay too, so a mode-local flag would just have to derive from `session.semantics_rev`
anyway — strictly more surface for the same gate) — was independently corroborated against
`src/replay.rs`, `src/runtime.rs`, and `crates/macp-core/src/session.rs`'s builder defaults
before being accepted, and matched exactly what shipped in Phase 1: `CURRENT_SEMANTICS_REV`
2 → 3, `parse_contribute_value` gained a `semantics_rev` parameter, and the tie-break trusts a
JSON parse only when the same bytes do NOT also round-trip byte-identically through the
canonical proto encoding.

Also confirmed a fact the plan's first draft asserted but had not itself verified: no
`semantics_rev ==` comparison exists anywhere in the codebase (all gates are `>=`/`<=`), so
bumping the shared counter is mechanically additive for every other consumer (Handoff, the
suspension-cycle cap) — re-checked directly this session via a workspace-wide grep, zero hits.

Per the plan's own framing, this does not need to route back through `/reconcile` as an open
item — it is a defensible, already-analyzed engineering decision, not an `UNCONFIRMED` guess.
The one accepted, deliberately-not-further-reduced trade-off from this same decision — the
reverse-direction residual (a payload starting with literal `0x0a` that is simultaneously
valid legacy JSON and valid canonical proto) — is logged separately in `ASSUMPTIONS.md`
(`UNCONFIRMED` in the narrow "not yet proven zero real traffic hits it" sense, though provably
narrow in construction and identical to a trade-off `macp-sdk-python`'s PR #77 already
accepted for the same reason).

- **Decided by:** Fable (scoped consult on the one-way-door-shaped design question, during
  `/plan`), independently corroborated by Opus against live code before Phase 1 was written,
  and confirmed by the actual shipped implementation and its test suite (`plans/parse-contribute-value-192.md`,
  `plans/parse-contribute-value-192-PROGRESS.md`).
