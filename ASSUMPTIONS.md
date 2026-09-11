## macp-core re-export from macp-runtime's root
- **Plan:** `plans/rfc-macp-0013-commitment-hash-PROGRESS.md` (RFC-MACP-0013, PR 3 of 5)
- **Assumed:** CLAUDE.md states "the root crate re-exports the lower crates so the historical `macp_runtime::*` paths are preserved," and today `src/lib.rs` re-exports `macp_modes`, `macp_policy`, `macp_storage`, and `macp_auth` — but not `macp_core` itself (only thin `error`/`session` shims). An external consumer of the published `macp-runtime` crate therefore cannot reach `macp_core::commitment_hash::commitment_hash` through `macp_runtime::*` and must take a direct `macp-core` dependency.
- **Chose:** Left this alone in Phase 1 — `commitment_hash` is a new module-only export in `macp-core` (no flat function re-export at that crate's root either, since the crate's existing flat re-exports are all types, and a function re-export shadowing its own module name would read oddly at a call site). Whether `macp-runtime`'s root should also re-export `macp_core` wholesale is a pre-existing gap this phase surfaced, not one it introduced.
- **Alternatives:** Add `pub use macp_core;` (or a narrower `pub mod commitment_hash` shim) to `src/lib.rs` in this same PR.
- **Blast radius if wrong:** Low and cheap to reverse — adding a re-export later is backward-compatible (additive) and doesn't touch any existing call site.
- **Status:** CHANGED (2026-08-30) — re-export added; see `DECISIONS.md`. The entry under-scoped the
  problem: `macp_core::mode::MessageContext` appears in the signature of `Mode::on_message_at`, a defaulted
  method on the publicly re-exported `Mode` trait, so an external consumer could not override the one method
  that offers a trustworthy clock. Resolved by `pub use macp_core;` plus adding `MessageContext` to the
  existing `crates/macp-modes/src/mode/mod.rs` re-export.

## Startup config errors folded into the returned Err (stderr), not just tracing
- **Plan:** `plans/list-sessions-pagination.md` (Phase 2, config plumbing)
- **Assumed:** Phase 2's acceptance criterion says an invalid page-size env var must abort startup "with a message naming the variable." Verified against the binary that the per-variable detail from `validate_env_config` goes through `tracing::error!`, whose `tracing_subscriber::fmt()` default writer is **stdout**; only the generic summary (`startup aborted: N configuration error(s) detected`) reaches stderr. Under `RUST_LOG=off` the operator gets the summary and no indication of which variable is wrong.
- **Chose:** Folded the collected error details into the returned `Err` so they reach stderr regardless of logging configuration, keeping the existing `tracing::error!` calls for structured logs. Taken as a tier-2 call under `/drive` (needs judgment, not a one-way door): it is text-only, changes no abort condition, exit code, or validation order, and it is what makes the phase's own acceptance criterion actually true rather than true-only-with-logging-on. A startup abort is the one message that must be self-sufficient.
- **Alternatives:** Leave it and keep the tests asserting across stdout-or-stderr (the executor's original, correctly flagged rather than silently applied); or switch the subscriber's writer to stderr globally (larger blast radius — changes every log line's stream, not just the abort path).
- **Blast radius if wrong:** Low. Text of one error string; revert is a two-line change. Risk is cosmetic duplication (detail appears on both stdout via tracing and stderr via the Err) for operators who do run with logging on.
- **Status:** CONFIRMED (2026-08-30) — the stdout-default claim was independently verified against
  `tracing-subscriber` and the running binary. The change removed an inconsistency rather than creating one:
  this was the sole fatal startup path whose detail could be silenced by a log filter.

## Startup-gate tests must poll try_wait, never Command::output()
- **Plan:** `plans/list-sessions-pagination.md` (Phase 2, config plumbing)
- **Assumed:** A startup-abort test that uses `Command::output()` blocks until the child closes its pipes. When the validation under test is removed, the binary does **not** exit — it starts a server — so `output()` blocks forever. Observed directly: a mutation check hung and blew a 10-minute timeout before the tests were rewritten.
- **Chose:** `run_expecting_startup_abort` spawns with piped stdio and polls `try_wait` against a 15s deadline, killing and failing if the process is still alive. This converts a future regression into a **test failure in ~30s** instead of a hung CI job.
- **Alternatives:** `Command::output()` (the obvious form, and what the two pre-existing tests in this file still use — `startup_refuses_invalid_policies_dir` and `startup_refuses_without_auth_or_insecure_flag` carry the same latent hazard; left alone as out of scope for this plan).
- **Blast radius if wrong:** Low for this feature. But the two pre-existing tests remain a latent CI-hang risk if either of their validations is ever weakened — worth a separate follow-up, noted here so it is not lost.
- **Status:** CONFIRMED (2026-08-30), with a correction and the follow-up done. **The two pre-existing tests
  did not carry the same hazard.** Only `startup_refuses_invalid_policies_dir` was genuinely hang-prone — it
  sets `MACP_ALLOW_INSECURE=1`, so nothing but the policies-dir check stands between it and a running server;
  it is now on the bounded helper. `startup_refuses_without_auth_or_insecure_flag` has a structural backstop
  (the independent TLS gate in `src/main.rs`), so it keeps `output()` and instead gained `env_remove` for the
  two TLS paths that would defeat that backstop. The helper drains its pipes only after exit — safe at a few
  hundred bytes against a >=16KB buffer, now recorded in its doc comment.

## Config-consistency guard made symmetric (aborts instead of silently clamping)
- **Plan:** `plans/list-sessions-pagination.md` (Phase 2, config plumbing) — **revises the phase's own acceptance criterion 3**
- **Assumed:** As first built, an explicit `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE > MACP_LIST_SESSIONS_MAX_PAGE_SIZE` aborted startup naming both values, but the identical operator error with `MAX` unset (e.g. `DEFAULT=5000`, effective max 1000) was silently clamped, announced only by a `tracing::warn!` that writes to stdout and disappears under `RUST_LOG=off`. Same mistake, two completely different outcomes, with the quieter one attached to the more likely case.
- **Chose:** Compare an explicitly-set `DEFAULT` against the **effective** max — explicit when set, otherwise the built-in 1000 — and abort in both cases, naming the effective max and whether it was explicit or the default. Kept `default.min(max)` in the resolver as defence-in-depth for library consumers of `SecurityLayer::from_env()`, who never reach `validate_env_config` (private to `src/main.rs`); it simply stops firing for the binary. Tier-2 call under `/drive`: reversible, config-only, no wire or persisted format.
- **Alternatives:** Leave the asymmetry and record it (what the verifier offered as the other option) — rejected because half a guard is worse than either whole option; or drop the abort entirely and always clamp — rejected because it discards stated operator intent silently, which is the failure mode the guard exists to prevent.
- **Blast radius if wrong:** Moderate-but-contained. A deployment that sets a large DEFAULT and relies on being clamped would now fail to start rather than running with a smaller page size. No such deployment can exist — both variables are new in this change and unreleased. Reversing is a small edit to one validation block.
- **Status:** CONFIRMED (2026-08-30), with two fixes. **Correction:** "it simply stops firing for the binary"
  is wrong — the resolver clamp still fires for the binary when `MAX` is set and `DEFAULT` is not; only the
  explicit-default branch stops firing. **Fixed:** `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=0` was parsed in
  `src/main.rs` without a `> 0` filter, so it produced a spurious second error naming an effective maximum of
  `0` while `security.rs` treated the same value as the built-in 1000 — the two layers disagreed on what `0`
  meant. The filter is now mirrored, and the abort message states the remedy.

## The env-var-to-field binding is unproven until an end-to-end test exists
- **Plan:** `plans/list-sessions-pagination.md` (Phase 2 → carried into Phase 3/4 as a hard requirement)
- **Assumed:** A verifier swapped the two env values feeding `resolve_list_sessions_page_sizes` in `from_env` and **the entire test suite passed** — 626 workspace tests, 92 Tier-1, 8 JWT, 5 Tier-2, zero failures. The swap would make `DEFAULT=2 MAX=900` resolve to a cap of 2 instead of 900, and `validate_env_config` accepts it. Nothing catches it because the unit tests drive the name-agnostic pure resolver and the single `from_env` assertion runs with both vars unset, where a swap is invisible.
- **Chose:** Two-part fix. Now: replace the positional `Option<String>` pair with a named struct so the call site must write the field name beside the env-var name. **Correction — this does NOT make the error unrepresentable, as first claimed.** The fixer demonstrated the residual: swapping the field initialisers is now a semantic no-op (field order carries no meaning), but transposing the env-var *name strings* while leaving the field names in place still compiles and still passes all 626 workspace and 105 integration tests. The struct converts an invisible positional hazard into a visible name/name mismatch in adjacent text; it does not eliminate the class. **The gap is closed only by the Phase 3 end-to-end test**, which is why a pointer comment sits at the call site. Later (recorded as a hard requirement in the Phase 3 section): Tier-1 coverage via `server_manager::start_with_env` asserting a distinctive non-default default page size and a **different** distinctive max, since identical values would not detect a swap.
- **Alternatives:** Rely on the named struct alone — rejected, and the demonstration above is why: it does not even prevent the mistake, only makes it more visible to a reader. Or add the Tier-1 test in Phase 2 — impossible, nothing reads the fields until the handler exists.
- **Blast radius if wrong:** High if it were to regress and go unnoticed — a silently wrong page cap is invisible to clients that ignore `next_page_token`, which is the exact failure mode this whole feature exists to fix. Low now, given the named struct plus the pending end-to-end test.
- **Status:** CONFIRMED (2026-08-30) — gap closed. Independently re-derived rather than taken from the phase
  log: `page_size_above_max_is_clamped` sets D=2/M=3, so correct wiring resolves to `(2, 3)` and a transposition
  to `(2, 2)`; the `page_size=1000` assertion then sees 2 where it expects 3 and fails. The 7/900 tests are
  transposition-blind, as recorded. Residual: retuning that one test to reuse 7/900 would silently erase the
  proof — the doc comment explaining the choice of 2 and 3 is the guard.

## CLAUDE.md is gitignored, so its env-table copy cannot be kept in sync by a PR
- **Plan:** `plans/list-sessions-pagination.md` (Phase 5, docs + env-table sync)
- **Assumed:** The plan treats `CLAUDE.md` as one of four mirrored env-var tables to be updated "in one commit so they cannot drift." It is **gitignored** (`.gitignore:20`) and `git log --all -- CLAUDE.md` is empty — it has never been tracked in this repository. Its edits are real on disk but will not appear in the PR diff and will not reach anyone who clones the repo.
- **Chose:** Updated it anyway (it is the file agents in this working copy actually read, so a stale copy here misleads every future session), and recorded the limitation rather than changing repo policy. **Did not un-ignore it** — adding a ~370-line instruction file to the tracked tree is a visible repo-policy decision outside this feature's scope, and it may be ignored deliberately.
- **Alternatives:** Remove `.gitignore:20` and commit `CLAUDE.md` so the fourth mirror actually ships (makes the anti-drift guarantee real, but is a repo-policy change the owner should make deliberately); or stop treating it as a mirror and delete the env table from it (loses the local reference).
- **Blast radius if wrong:** Low but persistent. Three tracked tables stay in sync; the fourth silently diverges for every clone. A future contributor reading a fresh checkout's `CLAUDE.md` — if they create one — would not see the two page-size variables at all.
- **Status:** CONFIRMED (2026-08-30) — repo owner's call: `CLAUDE.md` **stays gitignored**. Accepted
  consequence: three tracked env-var tables stay in sync and the local copy silently diverges for anyone
  cloning fresh. Not revisited unless the file is ever tracked.

## Quorum `threshold.value = 0` means two different things in the two layers
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 3, quorum threshold unification — **carved out of the phase's own differential-matrix criterion**)
- **Assumed:** Both layers gate the policy threshold on `value > 0.0` (`QuorumMode::effective_threshold`, `evaluate_quorum_commitment_outcome`) and then diverge *outside* that gate: the mode falls back to the ApprovalRequest's `required_approvals`, while the evaluator applies no approval bar at all. So a policy with `threshold.value: 0` bound to a session whose ApprovalRequest asked for 3 approvals gates `commitment_ready` at 3 but passes the evaluator at 0. The ceil-and-floor fix lives *inside* the gate and does not touch this.
- **Chose:** Left it, and carved `value = 0` out of the differential matrix explicitly (the test names the carve-out and says why, so nobody widens the grid and then "fixes" the failure by weakening the assertion). The two fallbacks are arguably both correct for their own layer — the mode has a request to fall back to and the evaluator does not, and an inert rule *should* mean "the mode's built-in rule stands". Unifying it means deciding what an absent policy bar means, which is a semantics question for RFC-MACP-0012, not a rounding bug.
- **Alternatives:** Pass `required_approvals` into the evaluator so both layers use the same fallback (changes a public signature and makes the evaluator depend on mode state); or treat `value: 0` as an explicit "no approvals required" bar in both layers (the degenerate case the floor-to-1 fix exists to make unreachable — strictly worse).
- **Blast radius if wrong:** Low and bounded in the safe direction. The mode is the stricter of the two: it will not call the evaluator until its own bar is met, so the evaluator's laxer reading can only fail to add a constraint, never remove one. It cannot produce a commitment the mode would have refused.
- **Status:** UNCONFIRMED (2026-09-10)

## The quorum mode silently tolerates a rules object the evaluator rejects
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 3 — named in the phase's edge cases as explicitly NOT in scope to unify)
- **Assumed:** `QuorumMode::effective_threshold` parses the bound policy's `rules` with `serde_json::from_value(...).unwrap_or_default()`, so a rules object that fails to deserialize (a type error — `"threshold": "majority"`) yields the schema defaults and an inert threshold, and the mode proceeds on the ApprovalRequest's `required_approvals`. The evaluator's `parse_rules` on the same object **denies the commitment**. A session can therefore be "ready to commit" by the mode's reckoning and then refused with `POLICY_DENIED` at the last step.
- **Chose:** Left both behaviours as they are. The refusal is fail-closed and nothing can be sealed on a policy neither layer understood, so the defect is a confusing error, not a governance hole — and after Phase 2 (registration-time shape validation) and Phase 3 (wildcard policies now validated against every mode's schema) the only way to reach it is a directly-constructed `PolicyDefinition` or a policy file edited under a running session.
- **Alternatives:** Make the mode propagate the parse error (`MacpError::InvalidModeState`-ish) so the failure surfaces at the first message rather than at commitment — better feedback, but it converts what is today a late refusal into an early one for every message in the session, which is a wider wire-visible change than the rounding fix warrants.
- **Blast radius if wrong:** Low. No commitment seals; the operator sees `POLICY_DENIED` where `INVALID_PAYLOAD` would have been clearer.
- **Status:** UNCONFIRMED (2026-09-10)

## A zero-ballot quorum decline is refused even though RFC-MACP-0011 §4a permits it
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 3, acceptance criterion 6 — "close it or move it to Blocked; do not leave it silent")
- **Assumed:** §4a makes a session eligible for a negative `Commitment` when `remaining_eligible + approvals < required_approvals`. Read literally, that is satisfied with **no ballot cast at all** whenever the bar exceeds the participant pool: `0 + N < required`. The runtime already refuses `required_approvals > participants` on the `ApprovalRequest` itself, so the only way to reach that state is a *policy* threshold, which §6 says replaces `required_approvals` but which nothing held to the same domain. The effect was that a coordinator could seal a binding `quorum.rejected` before anyone voted.
- **Chose:** Two guards, both narrower than they look. (1) The `ApprovalRequest` is refused when the effective threshold falls outside `1..=participants` — the domain the field it *replaces* already had to satisfy. (2) `commitment_ready`'s unreachable-threshold branch additionally requires at least one ballot, covering the replay path where an edited policy rebinds a larger threshold to a session whose request was already accepted. Guard (2) is a deliberate deviation from the literal §4a formula, and it is provably confined to the misconfigured case: with zero ballots the formula reduces to `participants < required`, which is unreachable once guard (1) holds. Every §4b scenario the RFC actually describes (all-abstain, or abstentions plus rejections) has at least one ballot behind it, and both are covered by tests.
- **Alternatives:** Clamp the effective threshold to `participants.len()` in both layers — rejected: it silently *loosens* a governance bar (an operator's "5 approvals" becomes 3), which is the wrong direction for a fail-closed runtime. Or refuse the policy at `SessionStart` instead of at `ApprovalRequest` — blames the right party but is a larger wire-visible change touching `runtime.rs`, and the participant count is already known at request time. Or leave it and record it — rejected because a binding negative commitment with no participation is exactly the "confident wrong answer" the issue reports.
- **Blast radius if wrong:** Moderate, in the fail-closed direction. A deployment whose quorum policy sets a threshold above a session's participant count now gets `INVALID_PAYLOAD` on the `ApprovalRequest` instead of a session that could only ever decline. No such policy can produce a positive outcome, so nothing that worked stops working — but the failure moved from "declines" to "refuses", which is visible. Reversal is a small edit to one match arm.
- **Status:** UNCONFIRMED (2026-09-10)

## Wildcard (`mode: "*"`) policies are now held to every mode's schema
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 3 — but **no acceptance criterion of Phase 3 covers this**. An earlier revision of this entry cited "Phase 3, item 4"; that is wrong, Phase 3's criterion 4 is the differential `{type} × {value} × {participant count}` threshold matrix. The wildcard fail-open reached Phase 3 only through the Phase 2 pinning test's own comment — `quorum_threshold_constraints_apply_to_wildcard_policies` in `crates/macp-policy/src/registry.rs`, renamed from `..._do_not_apply_to_...`, which pinned the fail-open and named Phase 3 as the place to close it. Phase 3's Files list records the resulting registry edit as a justified scope expansion, not as a planned criterion.)
- **Assumed:** `validate_rules_for_mode("*")` deserialized against `DecisionPolicyRules` only, calling it a "superset" it is not: it has no top-level `threshold`, and with no `deny_unknown_fields` a Quorum `threshold` inside a `"*"` policy was silently dropped. `validate_conditional_constraints` then gated the quorum checks on an exact mode match, and `Runtime::handle_session_start` bound the policy to a quorum session anyway — so `effective_threshold` read a value no layer had validated. This was plan option (a): validate against every mode's schema.
- **Chose:** Option (a). A `"*"` policy binds to every mode's sessions and every mode's evaluator re-parses the same rules through its own struct, so holding it to all five schemas and all their constraints is the only reading of `"*"` that is not a hole. In practice it is also the smallest change: `validate_conditional_constraints` has exactly three families (decision, quorum, all-mode commitment) and the decision one already ran for `"*"`, so option (b) — "validate the quorum block whenever `threshold` is present" — would have been the same behaviour with a narrower justification.
- **Alternatives:** Option (c), leave it open now that the floor-to-1 defuses the severe case, and document it. Rejected: the fail-open is a hole through *every* quorum constraint Phase 2 added, not just the rounding one, and "use `mode: "macp.mode.quorum.v1"`, not `"*"`" is documentation asking operators to avoid a trap rather than removing it.
- **Blast radius if wrong:** Low, and the plan's stated worry ("may refuse policies that register today") did not materialise in any test. Since no struct uses `deny_unknown_fields`, a field one mode's schema does not know is still ignored rather than refused — the built-in `policy.default`'s Decision-shaped rules register unchanged, and the only new refusals are values that are out-of-domain for the mode that owns them. What *would* break is a wildcard deliberately carrying a knowingly-invalid block for a mode it never expected to be used with.
- **Status:** UNCONFIRMED (2026-09-10)

## `EffectiveThreshold` is deliberately not `#[non_exhaustive]`
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 3, quorum threshold unification — the new `macp-core` type the shared resolver introduced)
- **Assumed:** Every other public enum and public-field struct `macp-core` exposes across a crate boundary carries `#[non_exhaustive]` *and* a rustdoc sentence giving its reason — six of them: `mode.rs:9` (`ModeResponse`), `mode.rs:32` (`MessageContext`), `session.rs:54` (`Session`), `policy/mod.rs:24` (`PolicyDecision`), `policy/mod.rs:118` (`CommitmentMode`), `error.rs:3` (`MacpError`). `EffectiveThreshold` departs from all six and, until now, said nothing about the departure — so a reader could only read the omission as an oversight.
- **Chose:** Keep it exhaustive, and document why in the enum's own rustdoc rather than only here. `#[non_exhaustive]` binds every crate *except* the defining one, and this enum has exactly two consumers, both outside `macp-core`: `QuorumMode::effective_threshold` (`crates/macp-modes/src/mode/quorum.rs:92-94`) and `evaluate_quorum_commitment_outcome` (`crates/macp-policy/src/evaluator.rs:712-725`). Their compile-time exhaustiveness **is** the guarantee the whole #145 fix buys — one rule, one interpretation, provable at build time. Adding the attribute would force a `_` arm at both, and a fail-closed `_` arm is strictly worse for a governance kernel than a build failure: a future variant would silently decline instead of failing to compile, which is the same class of silent mis-handling as #145 itself (a `_` arm reinterpreting `weighted` as a raw approval count is precisely what that fix deleted).
- **Alternatives:** Add `#[non_exhaustive]` for consistency with the other six and accept fail-closed `_` arms (rejected above); or add it and have both call sites `panic!`/`unreachable!` in the `_` arm to recover the loudness (trades a compile error for a runtime abort in a kernel that must not panic on policy data); or leave it exhaustive and undocumented (the status quo this entry exists to end — an unexplained departure from six documented precedents reads as an oversight and invites someone to "fix" it).
- **Blast radius if wrong:** Low and bounded to release mechanics. Adding a variant later is a breaking change for downstream matches, but it is **not** a silent one: `enum_variant_added` is a major `cargo-semver-checks` lint and `release-plz.toml:20` sets `semver_check = true`, so it blocks the release PR rather than shipping. The residual cost is release coordination (a deliberate minor bump on 0.x, with both in-tree call sites updated in the same commit), not an undetected break. If the enum ever grows a third consumer outside this workspace, revisit — the trade is sound while every consumer is in-tree.
- **Status:** UNCONFIRMED (2026-09-11)

## A negative weighted total fails the round, which moves one decline from DENY to ALLOW
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 4, the negative-weight evaluator hole — **the phase's stated premise was wrong against the code; see below**)
- **Assumed:** The plan describes the defect as "`evaluator.rs:388-390` returns `VotingResult::NoVotes` when `weighted_total == 0.0`; change it to `< 0.0`", and its criteria 1-3 use `weights: {"a": 1.0, "b": -1.0}`. Both are wrong arithmetically. That map sums to **exactly 0.0** with both agents voting, i.e. the schema-legal case the plan itself defers to spec issue #98 item 3. And a *negative* total never matched `== 0.0` at all — it fell straight through to `weighted_approve / weighted_total`, where a **negative denominator inverts** `ratio >= threshold`. Demonstrated: `{fraud: 1.0, growth: -2.0}` with fraud REJECTing and growth APPROVing gave `-2.0 / -1.0 = 2.0 >= 0.5` → `Passed` — a positive commitment **allowed** on a reject from the only voter whose weight is in-schema. The hole was an inverted comparison, not a "no votes" misreport.
- **Chose:** Ship the `< 0.0 => Failed` short-circuit as planned (the fix is right even though the diagnosis was not), and correct the account of it in the code, the `evaluate_decision_commitment_outcome` rustdoc table, and the tests. Kept the `== 0.0 => NoVotes` branch untouched, deferred to spec #98 item 3. Wrote the criteria-1-3 tests around a genuinely negative map (`{fraud: 1.0, growth: -2.0}`) and asserted the `VotingResult` variant directly via `check_voting_algorithm`, not only the `PolicyDecision` — necessary, because two of the three rounds were already denied before the change and a `PolicyDecision`-only assertion would have passed against the unfixed code. Every new test was mutation-checked: removing the branch reddens exactly the three negative tests, removing the `== 0.0` branch reddens only the zero test, removing the `:337` short-circuit reddens the guard test plus the two §4.1 tests it protects.
- **Alternatives:** Refuse the round with an error rather than `Failed` (out of scope — `check_voting_algorithm` has no error channel and every caller treats its result as a governance outcome); or clamp the total to zero and fall into `NoVotes` (keeps the decline direction unchanged but re-launders out-of-schema data as "nobody voted", the same silent reinterpretation the #145 fix removed).
- **Blast radius if wrong:** Bounded to out-of-schema policies — `voting.weights[*]` is `minimum: 0`, so registration already refuses these and only a directly-constructed `PolicyDefinition` reaches the arm. **Not purely a tightening.** In the approve direction it is one (`Passed` → `Failed`: a positive commitment that used to seal is now denied). In the decline direction it is a fail-open: on that same round a decline moves **DENY → ALLOW**, because a decline over `Passed` was refused while a decline over `Failed` is permitted once the universal reject-floor (`reject_count > 0`) is met. That is the intended outcome — the round is genuinely decided and an explicit reject backs the decline — but RFC-MACP-0012 §4.1's no-result branch is conditioned on no decisive vote having been *cast*, which is false here, so this **fills a gap §4.1 does not address** rather than moving toward conformance. It must be stated as a direction change in the release notes, not as a tightening.
- **Status:** UNCONFIRMED (2026-09-11)

## `supermajority` silently substitutes 2/3 for an out-of-domain threshold
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 4 edge cases — "Leave it; record it in `ASSUMPTIONS.md`")
- **Assumed:** `check_voting_algorithm`'s `supermajority` arm computes `if threshold > 0.5 { threshold } else { 2.0 / 3.0 }`. A policy asking for a 40% supermajority is therefore silently *raised* to 66.7% rather than refused, and the reported reason names the substituted value with no indication that it is not what the policy said. Unreachable through the registry: `registry.rs:445` refuses `supermajority` with `threshold <= 0.5` at registration, so only a directly-constructed `PolicyDefinition` reaches it.
- **Chose:** Left it. Not touched in Phase 4, which is confined to the weighted arm. The substitution fails **closed** (it can only raise the bar, never lower it), the reachable path is already refused at the door, and changing a silent substitution into a refusal inside the evaluator would move a policy-authoring error from commitment time to a place with no error channel — `check_voting_algorithm` returns a governance outcome, not a `Result`.
- **Alternatives:** Return `Failed` with an "out-of-domain threshold" reason (fails closed and is honest, but converts a conservative bar into a hard block for any consumer driving `macp-core` + `macp-modes` with its own evaluator and no registry); or delete the clamp and use `threshold` as given (fails **open** — a 40% "supermajority" — strictly worse).
- **Blast radius if wrong:** Very low. One unreachable branch whose only effect is a stricter bar than requested, and a reason string that under-explains itself.
- **Status:** UNCONFIRMED (2026-09-11)

## `unanimous` passes by vacuous truth on an empty participant list
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 4 edge cases — "record, do not fix here")
- **Assumed:** The `unanimous` arm's predicate is `participants.iter().all(|p| ...voted APPROVE)`, which is `true` for an empty slice. With at least one non-abstain ballot (needed to clear the `:337` short-circuit) and no REJECT among them, a zero-participant unanimous round returns `Passed`. Unreachable through the server: strict `SessionStart` requires a non-empty `participants` list for every standards-track mode, so an empty slice only arrives from a direct library call.
- **Chose:** Left it. Fixing it means deciding what `unanimous` means on an empty tally, which is spec issue #98 item 1 (RFC-MACP-0012 §4.1) and blocked — the same blocker that holds issue #147. Phase 4 deliberately did not touch the `unanimous` arm, and a local fix here would pre-empt the amendment and contradict the two §4.1 tests the phase is required to leave green (`all_abstain_returns_no_votes`, `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum`).
- **Alternatives:** Return `Failed` when `participants.is_empty()` (the honest reading, but it is exactly the §4.1 change #98 must ratify first); or assert non-empty participants at the function boundary (moves an unreachable case into a panic in a kernel that must not panic on policy data).
- **Blast radius if wrong:** Very low while unreachable from the wire. It becomes live for any consumer that drives the evaluator directly with no participants, and it will be revisited as part of the blocked #147 work rather than in isolation.
- **Status:** UNCONFIRMED (2026-09-11)

## The public effective-threshold accessor answers three questions in three layers, not one `Option`
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 5, publish the corrected effective threshold — the plan's own signature suggestion had to be rejected; see below)
- **Assumed:** The plan specifies `effective_threshold_for_session(&Session) -> Option<u32>` with `None` meaning "no accepted `ApprovalRequest`". After Phase 3 the inner function already returned `Option<u32>` with `None` meaning **unsatisfiable threshold**, so that signature would have collapsed two unrelated answers — "this session has no request yet" and "this policy can never be satisfied" — into one value, which is the single thing a caller reading a governance bar cannot afford. Decoding `session.mode_state` adds a third outcome the plan does not mention at all: the state may not decode.
- **Chose:** `pub fn effective_threshold_for_session(&Session) -> Result<Option<ApprovalThreshold>, MacpError>` over a new two-variant `pub enum ApprovalThreshold { Approvals(u32), Unsatisfiable }`, with the request-level `pub fn effective_threshold(&Session, &ApprovalRequestRecord) -> ApprovalThreshold` sharing that enum. One layer per question, and no representable-but-impossible state: `Err` = undecodable state (which also catches a session of another mode, since `QuorumState`'s fields are not `#[serde(default)]`), `Ok(None)` = no request accepted, `Ok(Some(_))` = the resolved bar. The policy's `Inert` case is resolved *before* it reaches the caller, to `Approvals(request.required_approvals)`, because handing back `EffectiveThreshold::Inert` would hand back the fallback rule and re-create the 15-line mirror issue #146 exists to delete. `ApprovalThreshold` is deliberately exhaustive, for the reason already recorded for `EffectiveThreshold` above.
- **Alternatives:** `Option<EffectiveThreshold>` (the plan's second option) — rejected twice over: it widens `macp-core`'s `EffectiveThreshold` exposure into a second crate's public API, and its inner `Inert` variant would be unreachable through this path, i.e. an impossible state the caller must still match. A flat three-variant enum with a `NoRequest` member — rejected because the request-level form then carries a variant it can never return, and because `Option::map` is exactly the composition the two functions have. `Result<u32, Reason>` — rejected: it makes the ordinary "no request yet" case an error, and folds two non-error outcomes into an error channel. Keeping `effective_threshold` private and documenting `decode_mode_state` as the route — rejected: that is the reconstruct-the-internals path the issue asks to remove.
- **Blast radius if wrong:** Additive only — `cargo semver-checks check-release` on `macp-core`, `macp-modes` and `macp-runtime` is clean (exit 0, no major lints). The cost of being wrong is API churn: narrowing `Result<Option<_>, _>` later, or adding an `ApprovalThreshold` variant, is a breaking change, though `enum_variant_added` and signature lints are majors that `release-plz.toml`'s `semver_check = true` blocks on rather than shipping silently. If a third outcome for the session-level form ever appears, it belongs in a new `Ok` variant, not in a new error.
- **Status:** UNCONFIRMED (2026-09-11)

## The rev-2 scaffolding branch cannot be *literally* identical to the rev-1 branch
- **Plan:** `plans/backlog-closeout-2026-09.md` (Phase 9, "a `>= 2` branch identical to `>= 1`")
- **Assumed:** The phase is specified as adding a `session.semantics_rev >= 2` branch whose body is
  identical to the existing one, so that the behavior change is a separate commit. Written literally
  — `if rev >= 2 { now - offered_at } else { now - offered_at }` — that is a `clippy::if_same_then_else`
  error under the repo's `-D warnings` gate, so the instruction is not directly expressible. (The
  same gate's `clippy::assertions_on_constants` also refuses `assert!(CURRENT_SEMANTICS_REV >= 2)`
  in a test.)
- **Chose:** Put the rev gate in a named function, `HandoffMode::implicit_accept_elapsed_ms`, whose
  `>= 2` arm delegates to a second named function, `rev2_elapsed_ms`, that today returns the same raw
  difference the `<= 1` arm computes inline. Syntactically distinct, so clippy is satisfied; the
  seam is a single small function the suspension-correction term is subtracted inside, which is a
  smaller diff than an inline branch would be. The constant check became
  `const _: () = assert!(..)`, i.e. a compile-time assertion rather than a runtime one.
  `HandoffOfferRecord.suspended_ms_at_offer` is recorded on **every** offer, not only rev >= 2 ones:
  the field is serialized regardless of value (so rev-gating would not preserve the old
  `mode_state` bytes anyway) and it is read only under the rev >= 2 arm, so recording it everywhere
  is behavior-neutral and keeps the value trustworthy wherever it is later consulted.
- **Alternatives:** `#[allow(clippy::if_same_then_else)]` on an inline identical branch (honest about
  the intent, but parks a suppression in a hot path and the next editor has to decide whether it is
  still needed); a rev-2 arm that subtracts an explicitly-zero named term (same lint, one indirection
  later); or skipping the branch entirely and landing it with the semantics change (rejected — that
  is exactly the un-bisectable bundle this phase exists to avoid).
- **Blast radius if wrong:** None observable. Both functions are private, the arithmetic is identical
  on every input, and three replay fixtures plus a rev-1-vs-rev-2 differential test pin that
  equivalence; the cost of being wrong is one extra function to inline later.
- **Status:** UNCONFIRMED (2026-09-11)

## Discharging Phase 10's acceptance criterion 3 when no conformance fixture exists
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 10)
- **Assumed:** the criterion's intent is "a rev-1 history containing an implicit accept still replays
  byte-faithfully", not literally "the function named `assert_replay_equivalence` executes over such
  a history". The criterion as written is **unsatisfiable**: `assert_replay_equivalence`
  (`tests/conformance_loader.rs:356`) is called only from the vendored-fixture loop at `:520`, and no
  fixture in `tests/conformance/` exercises an implicit accept — and `tests/conformance/` is vendored
  from the spec repo and byte-diffed by the `conformance-oracle` CI job, so one cannot be added here.
- **Chose:** discharge the intent through the real `replay_session` path instead — a differential
  legacy-log fixture (`src/replay.rs:1010,1036,1063`) splicing real-shaped `SessionSuspend`/
  `SessionResume` `Internal` entries around an implicit accept, asserting a rev-1 history still
  implicitly accepts and the same entries at rev 2 do not. The pre-existing
  `current_rev_handoff_history_replays_identically_to_rev1` continues to cover byte-level `mode_state`
  equality for unsuspended histories.
- **Alternatives:** (a) add a `tests/conformance/` fixture — blocked, vendored and CI-byte-diffed;
  (b) declare the criterion undischargeable and stop the phase — rejected, the criterion's *intent* is
  both meaningful and testable, and the third of three plan errors this phase found is a drafting
  error in my own acceptance criterion, not a gap in the work.
- **Blast radius if wrong:** test-only. Satisfying the criterion literally requires a fixture PR in
  the spec repo, which is read + issues only under the current authorization.
- **Status:** UNCONFIRMED (2026-09-11)

## Updating `CURRENT_SEMANTICS_REV`'s rev-2 doc bullet outside Phase 10's Files list
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 10)
- **Assumed:** leaving Phase 9's now-false wording ("currently identical to revision 1 — nothing
  observable differs") in `crates/macp-core/src/session.rs:28` is worse than touching one file the
  phase's Files list does not name. That bullet list is the **only** place semantics revisions are
  documented, so a stale entry there is the single most misleading place for one.
- **Chose:** rewrote the rev-2 bullet in the same commit as the behaviour change. No code change in
  that crate; `cargo doc` clean, no API change.
- **Alternatives:** defer to Phase 13 (the G4 docs phase) — rejected, because Phase 13's Files list
  scopes to `docs/` and `CLAUDE.md`, so a slip or a narrow reading there ships a doc that contradicts
  the code.
- **Blast radius if wrong:** doc comment only.
- **Status:** UNCONFIRMED (2026-09-11)

## Omitting the in-flight suspension term while the implicit-accept check is lazy-only
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 10)
- **Assumed:** `session.accumulated_suspended_ms` alone is complete for the lazy path, and an
  in-flight term (`now_ms - suspended_at_ms` for a currently-suspended session) would be dead code.
  The reason is `macp_modes::step::check_preconditions` (`crates/macp-modes/src/step.rs:57-58`),
  which returns `SessionNotOpen` for **any** message when `state != Open` — so by the time
  `implicit_accept_elapsed_ms` runs, every pause that has occurred is already banked by
  `Session::resume` (`session.rs:189`). The plan states the formula but never states this reason.
- **Chose:** omit the term; document the reason at the site and leave a forward pointer to
  `Session::suspend_cap_exceeded` (`session.rs:213-221`), which is the existing precedent for adding
  an in-flight term when a caller *can* observe a suspended session.
- **Alternatives:** add the term now for symmetry — rejected: it is untestable today, and untested
  arithmetic inside a security-relevant deadline is worse than a documented omission.
- **Blast radius if wrong:** **Phase 12 (eager sweep) must resolve this.** A sweep running outside
  the message path *can* observe a suspended session, at which point either the in-flight term is
  added or the sweep must skip suspended sessions entirely. RFC-MACP-0010 §5.1(1) arguably implies
  the latter is correct. The doc comment says so at the site so the constraint travels with the code.
- **Status:** UNCONFIRMED (2026-09-11)

## Synthetic accept stands even when the triggering message is later rejected
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11e)
- **Assumed:** appending the synthetic `HandoffAccept` to accepted history while the message that
  *triggered* its observation is then rejected does not violate the freeze-profile invariant
  "rejected messages must not mutate accepted history or dedup state" (`CONTRIBUTING.md:41-44`,
  tracked; `CLAUDE.md:74`, gitignored).
- **Chose:** proceed, on three grounds established by an independent Opus reverify rather than by
  assertion. (1) **In-tree precedent:** `Precheck::Expired` (`src/runtime.rs:641-651`) already
  appends a durable `TtlExpired` entry, mutates `session.state`, saves the snapshot (`:649`), and
  *then* returns `Err` — a rejected message already causes a runtime-observation append plus a state
  mutation, shipped and blessed. The genuine delta is only that this observation lands in **accepted
  history** (`EntryKind::Incoming`, consuming an accepted ordinal per `log_store.rs:125-134`) and is
  **published to `StreamSession`**. (2) **The obvious alternative is non-conformant:** restricting
  synthesis to accepted triggers inverts RFC-MACP-0010 §5.1(4) — a late explicit `HandoffAccept`
  would be validated against an unaccepted offer and accepted, with the synthetic never emitted,
  while §5.1(2) requires the synthetic before evaluating *any* subsequent message against the
  offer's acceptance state. (3) **The dedup half is preserved exactly and no test needs weakening** —
  verified against `runtime.rs:1347`, `step.rs:282`, `coordination_library.rs:113` and
  `runtime.rs:1874`, all of which are in-memory-only and never observe the log.
- **Alternatives:** synthesize only for accepted triggers (rejected — non-conformant, above);
  defer synthesis to the eager sweep only (rejected — §5.1(2) makes lazy the MUST and eager the
  SHOULD).
- **Blast radius if wrong:** a client submitting a malformed message observes accepted history
  change as a side effect, and so does every `StreamSession` subscriber. The rejection class is
  **wide**, not an edge case: a late explicit accept, an unknown `handoff_id`, a mismatched
  `mode_version`, a duplicate offer. Mitigations required by the plan: `CONTRIBUTING.md` must be
  amended in the same PR to name the carve-out, and an acceptance criterion asserts the rejected
  trigger consumes **no** dedup slot and can be retried successfully.
- **Status:** UNCONFIRMED (2026-09-11)

## Persisted suspension intervals, with a cycle cap, rather than a derived deadline
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11b)
- **Assumed:** the synthetic accept's timestamp must be the *exact* deadline, computed by an interval
  walk. RFC-MACP-0010 §5.1(3) states the walk itself — "offer acceptance time + timeout + suspended
  time **within the window**" — so the naive `offered_at + timeout + banked_at_observation` is wrong
  whenever a suspend/resume pair lands *after* the true deadline but *before* observation. That is
  reachable: suspend/resume are RPCs (`src/runtime.rs:851`, `:905`), not session-scoped messages, so
  `step::check_preconditions`' non-`Open` rejection does not gate them. No existing state can express
  the walk, because the checkpoint fast path replays only `&log_entries[idx + 1..]` (`replay.rs:77`)
  and so cannot see pre-checkpoint pauses.
- **Chose:** a persisted interval list on `Session`/`PersistedSession`, recorded in `resume()`,
  rebuilt free by replay, plus a new `SessionBuilder::suspension_intervals` setter (required because
  `Session` is `#[non_exhaustive]` and `macp-storage` restores fields through the builder,
  `registry.rs:97-138`) — **and a `MAX_SUSPENSION_CYCLES` count cap enforced in `Session::resume`,
  gated `semantics_rev >= 2`.**
- **Alternatives:** unbounded list (rejected — see blast radius); log scan (rejected —
  checkpoint-blind); per-offer incremental deadline (rejected, and the reverify confirmed the
  rejection sound: `resume_session` and replay's `SessionResume` arm both mutate the session without
  mode dispatch, so it needs a new `Mode::on_resume` seam wired in live and replay lockstep plus a
  new `mode_state` writer on a non-message event); naive formula (rejected — forecloses Phase 12's
  byte-identity permanently).
- **Blast radius if wrong:** the cap exists because the unbounded version is an **amplification
  class, not noise**. `SuspendSession`/`ResumeSession` are entirely un-rate-limited
  (`src/server.rs:983-1006`, `:1045-1066` — auth and authority only, unlike `send` at `:249-251`);
  `MAX_SUSPEND_MS` bounds accumulated **duration** only (`session.rs:183-198`), so N one-millisecond
  cycles accrue ~0 against a 7-day budget and no cycle counter exists; and each cycle already writes
  two full `PersistedSession` snapshots (`storage/file.rs:61-66`, `to_vec_pretty`), so an in-snapshot
  vec turns constant-size writes into O(N), i.e. **O(N²) total bytes, triggerable by the session's
  own initiator.** A cap that force-expires corrupts nothing — it is the posture `Session::resume`
  already takes at `session.rs:191-194`. Note 11b therefore **cannot** claim "zero behaviour change".
- **Status:** UNCONFIRMED (2026-09-11)

## The `implicit` payload flag as discriminator, guarded by a mode-trait boundary hook
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11c/11d)
- **Assumed:** because replay re-dispatches the `Incoming` synthetic entry into the handoff mode —
  which must therefore *accept* well-formed implicit accepts at rev ≥ 2 — the payload's own
  `implicit` flag can serve as the provenance discriminator, made trustworthy by a defaulted
  `Mode::validate_client_envelope` hook that rejects client-submitted ones at rev ≥ 2. No persisted
  `LogEntry` discriminator is added.
- **Chose:** the hook. The reverify judged this **correct and decisive** on downgrade posture:
  `src/replay.rs`'s `_ => {}` arm makes an *unrecognized persisted discriminator* replay as a
  **silent no-op**, whereas an old binary meeting an unexpected `implicit: true` fails **loudly**
  through `replay_entry`'s `?`. Self-describing data loses here precisely because the reader is the
  thing that is stale.
- **Alternatives:** a persisted `EntryKind`/`LogEntry` discriminator with a forked replay arm
  (rejected — silent-no-op downgrade, above); `EntryKind::Internal` (foreclosed by RFC-MACP-0010
  §5.1(2), which requires accepted history by "the same construction as" the §7.5 envelopes).
- **Blast radius if wrong:** smaller than first framed. The reverify established that a bypassed
  hook grants **no authority** — the accept arm still requires `env.sender ==
  offer.target_participant`, and `authorize_sender` already gates senders, so a forger must already
  *be* the target, who could accept explicitly anyway. **The `implicit` flag is a provenance label,
  not a capability.** The real residual: `step::validate_message` is **not** on the runtime's own
  path (`process_message` calls `authorize_sender` and `on_message_at` directly), so the canonical
  durable-consumer example bypasses the hook by construction with no compile-time forcing. Both live
  entry points are covered (`Send` → `server.rs:868`, `StreamSession` → `:449`), and replay only
  re-reads entries that already passed the hook, so the guarantee holds for *this* runtime. The
  rustdoc hazard must therefore use the runtime itself as the worked example, not "a library
  consumer".
- **Status:** UNCONFIRMED (2026-09-11)

## Retiring the interim implicit-accept path fail-loud rather than fail-open
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11e)
- **Assumed:** gating the interim in-commitment-handler check to `semantics_rev < 2` is safe, so a
  rev-2 history lacking the synthetic entry **fails replay** rather than silently resolving.
- **Chose:** fail loud. Safe *only* because phases 10–13 ship as one PR and one release, so no rev-2
  histories exist in the wild — every session on published 0.7.5 is rev 1. This is also why Phase 10
  was judged not independently shippable: releasing it alone would publish a `semantics_rev = 2`
  whose meaning Phase 11 then changes, leaving one revision number with two meanings.
- **Alternatives:** keep the interim live at rev 2 as a fallback (rejected — two code paths could
  resolve the same session differently, and the fallback would mask a missing synthetic entry, which
  is the one thing replay must not hide).
- **Blast radius if wrong:** if any rev-2 history escapes before 11e lands, it becomes unreplayable —
  `replay_session` errors and `src/main.rs:386-391` skips the session entirely. Bounded by the
  single-release constraint, which must therefore be honoured.
- **Status:** UNCONFIRMED (2026-09-11)

## The synthetic commit deliberately skips `record_participant_activity`
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11e)
- **Assumed:** the target performed no activity, so the synthetic accept should not count as theirs.
- **Chose:** skip it, mirroring replay exactly — verified: `replay_entry` (`src/replay.rs:92-137`)
  never calls it for any entry kind, so skipping makes the synthetic entry's live and replay
  behaviour identical.
- **Alternatives:** record it (rejected — live and replay would then diverge, which is the failure
  class 11a exists to close).
- **Blast radius if wrong:** the sole consumer is informational — `SessionMetadata.participant_activity`
  (`src/server.rs:165-177`); nothing gates TTL, liveness or authorization on it. Consequence to note
  in the changelog: the target's `message_count` will not include the synthetic accept.
- **Status:** UNCONFIRMED (2026-09-11)

## Counting granularity of the widened `validate_replay_consistency`
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11a)
- **Assumed:** the two suspension fields count as two separate mismatches, not one grouped mismatch.
  The plan is **internally inconsistent** here: its Approach says "three warn-only comparisons"
  (→ separate) while criterion 2 says "a suspension-state mismatch", singular (→ grouped).
- **Chose:** the Approach field's wording — three independent `if` blocks, three independent
  increments, three distinct warn lines, for finer diagnostics. The existing bound-versions
  comparison is grouped, so this is a departure from the neighbouring style, taken deliberately.
- **Alternatives:** group `accumulated_suspended_ms` + `suspended_at_ms` into one counted mismatch,
  matching the bound-versions precedent. The new test asserts exact counts, so switching later costs
  one test line.
- **Blast radius if wrong:** none that decides anything. `recovery_replay_mismatches`
  (`src/main.rs:350`) is a log field plus a metric (`record_replay_mismatch`); nothing branches on
  the number and no test in `tests/` or `integration_tests/` asserts on it. Grouping changes a
  reported magnitude, never an outcome.
- **Status:** UNCONFIRMED (2026-09-11)

## `cancel_session` reads a clock solely to stamp its log entry
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11a)
- **Assumed:** unlike suspend/resume, `cancel_session` has no session-mutation clock its log entry
  must agree with — `Session::cancel()` takes no timestamp — so the new read exists only to stamp
  the entry, and one read placed after the terminal-state early return is the right shape.
- **Chose:** a single `Utc::now()` immediately before the payload build, so a no-op cancel on an
  already-terminal session does not read the clock at all.
- **Alternatives:** thread the clock down from `maybe_expire_session`'s existing read (it is called
  from `cancel_session`, so one read could serve both) — rejected as a wider refactor than 11a
  authorizes, and it would change the expiry predicate's relationship to its own clock.
- **Blast radius if wrong:** one extra `Utc::now()` per `CancelSession` RPC. `SessionCancel` replay
  only sets terminal state (`src/replay.rs:145-147`), reading neither timestamp, so nothing
  downstream observes the value.
- **Status:** UNCONFIRMED (2026-09-11)

## A real 5 ms sleep in `suspend_resume_entries_share_the_session_mutation_clock`
- **Plan:** plans/backlog-closeout-2026-09.md (Phase 11a)
- **Assumed:** a real sleep is acceptable in a `tests/` integration test to make the banked span
  non-zero, because the alternative needs a clock-injection seam this phase may not add.
- **Chose:** `tokio::time::sleep(5ms)` plus `assert!(accumulated_suspended_ms > 0)` so the equality
  cannot pass vacuously as `0 == 0` — the specific vacuity trap this plan has hit three times.
- **Alternatives:** no sleep (the equality holds at zero but proves nothing about the arithmetic);
  `tokio::time::pause()` with a virtual clock — rejected because `suspend_session`/`resume_session`
  call `chrono::Utc::now()` directly, which tokio's test clock does not virtualize, so it would
  require a clock-injection seam outside 11a's scope.
- **Blast radius if wrong:** 5 ms on one test; the file's measured runtime is unchanged at 0.01 s.
  Worth noting the honest limit of what this test proves: it pins the invariant deterministically,
  but as a *differential* signal against the pre-fix code it only fires when a millisecond tick
  lands between the two clock reads — measured at ~0.3-0.5% (1 red in 300 runs), with 800/800
  passing once fixed. **The injected-clock signature, not the test, is the real guarantee**, and the
  plan's claim that the test "could only fail on a clock tick" was right in kind but understated:
  it undersold a signal that does exist, rather than overselling one that does not.
- **Status:** UNCONFIRMED (2026-09-11)
