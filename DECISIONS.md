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
