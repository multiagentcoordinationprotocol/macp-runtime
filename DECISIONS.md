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
