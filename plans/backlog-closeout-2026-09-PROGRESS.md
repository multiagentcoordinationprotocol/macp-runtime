# PROGRESS — macp-runtime backlog closeout

**Plan:** `plans/backlog-closeout-2026-09.md` · **Base:** `999890e` (v0.7.4), clean tree apart from
untracked `.phase5-live-restart.log` · **Built:** 2026-09-10 by four Opus fan-out readers.

## Repo map — read this instead of re-scanning

### Policy evaluation (G2)
| Path | Purpose |
|---|---|
| `crates/macp-policy/src/evaluator.rs` | The whole voting path. `:74-78` rustdoc outcome table · `:205` the `none` check — an if/else, not a short-circuit · `:241-252` the `NoVotes` arm (pushes no *allow* reason; its `!outcome_positive` half is a DENY) · `:306` `enum VotingResult` (private) · `:315` `check_voting_algorithm` (private) · `:326-328` the front-of-dispatch `NoVotes` · `:330` `match algorithm` · `:331` majority · `:347` supermajority (`:348-352` the silent `2.0/3.0` clamp) · `:368` unanimous · `:386` weighted (`:388-390` second `NoVotes`) · `:406` plurality · `:424` `_` fail-closed · `:464` `compute_weighted_votes` (private, `:477` unsigned weight) · `:676-747` `evaluate_quorum_commitment_outcome` (`:706`/`:710` the two `.ceil()`s, `:712-720` decline not gated) |
| `crates/macp-policy/src/registry.rs` | `:53` `register` · `:147` `load_from_dir` (funnels through register) · `:173-195` `validate_definition` · `:209-228` reserved namespace · `:255-275` `validate_rules_for_mode` (serde only; unknown mode → `Ok`) · `:278-312` `validate_conditional_constraints` — the three checks at `:285`, `:290`, `:302`. **Phase 2 lands here.** · `:725` `register_valid_quorum_rules_succeeds` (passes a float for an integer field) |
| `crates/macp-policy/src/defaults.rs` | `:32` `STD_POLICY_PREFIX` · `:127` `canonical_std_policy` · `:197-206` `std_policies_all_require_vote_quorum` |
| `crates/macp-core/src/policy/rules.rs` | `:19-47` decision voting rules (`threshold: f64`, no bounds) · `:230` `QuorumPolicyRules` · `:241-257` `QuorumThreshold` (`value: f64`, schema says integer) |
| `crates/macp-core/src/policy/mod.rs` | `:59-73` `CommitmentRules` · `:120-137` `CommitmentMode` (`#[non_exhaustive]`) · `:149-153` `PolicyEvaluator` trait, one live method |
| `crates/macp-modes/src/mode/quorum.rs` | `:21-29` `ApprovalRequestRecord` · `:38` `QuorumState` · `:45` `QuorumMode` · `:49` inherent impl · `:67-83` `effective_threshold` (**Phase 3/5**) · `:85-101` `commitment_ready` (`:100` the predicate) · `:104` `impl Mode` · `:119-121`/`:144` the input guards · `:221` the only `commitment_ready` call site (`:219` is the match-arm header) |
| `crates/macp-modes/src/mode/util.rs` | `:119-157` `enforce_commitment_policy`, the single shared policy call site (`:125-127` no-policy short-circuit) · `:166` `decode_mode_state` (pub) |

**Reach:** only `macp.mode.decision.v1` reaches `check_voting_algorithm`. Quorum goes to
`evaluate_quorum_commitment_outcome` (§4.2 path). `multi_round`/`passthrough` never call
`enforce_commitment_policy`.

### Handoff / semantics_rev / replay (G4)
| Path | Purpose |
|---|---|
| `crates/macp-modes/src/mode/handoff.rs` | `:23-36` `HandoffOfferRecord` (no implicit/explicit marker; `outcome_reason` string is the only one) · `:137-141` rev≥1 clock source · `:201-205` rev≥1 `offered_at_ms` source · `:210-231` `HandoffContext` accepted at any disposition · `:233-242` client `implicit:true` rejection · `:284-302` the interim lazy check (`:298` the un-suspension-corrected comparison, `:302` the load-bearing `outcome_reason` string) · tests `:632-660` `client_submitted_implicit_accept_is_rejected`, `:1193`, `:1243`, `:1279`, `:1310` |
| `crates/macp-core/src/session.rs` | `:18-28` `CURRENT_SEMANTICS_REV = 1` + the revision bullet list · `:81-86` `suspended_at_ms` / `accumulated_suspended_ms` · `:87-89` `semantics_rev` · `:135` default · `:153-191` suspend/resume · `:206-214` `suspend_cap_exceeded` · `:225-229` `apply_mode_response` |
| `crates/macp-core/src/mode.rs` | `:25-44` `MessageContext { accepted_at_ms }`, `#[non_exhaustive]` |
| `crates/macp-modes/src/mode/mod.rs` | `:44-58` defaulted `on_message_at` — the rev-1 churn-avoidance pattern |
| `src/runtime.rs` | `:94` lifecycle broadcast capacity 64 · `:229-233` `publish_accepted_envelope` (2 callers: `:591`, `:728`) · `:247-249` `make_incoming_entry` · `:266-289` `make_internal_entry` (callers `:312`, `:816`, `:875`, `:933`, `:1106`) · `:470`/`:542` semantics_rev recording · `:586-590` the FIFO/mutex comment · `:660-667` live `accepted_at_ms` (`:640-646` is the `Precheck::Expired` arm) · `:1086-1130` the sweep locking pattern to copy (map read `:1092-1098`, `state != Open` filter `:1103`) |
| `src/replay.rs` | `:118-123` post-terminal skip (still consumes dedup) · `:126-133` replay `MessageContext` — **same value as live** · `:139-168` `EntryKind::Internal` handling (`:167` silent `_ => {}`) · `:183-222` `validate_replay_consistency` (does **not** compare `mode_state`) · `:151-166` suspend/resume replay · `:290-292` semantics_rev recovery · `:776` the rev-preservation test |
| `src/main.rs` | `:464-491` the cleanup loop (Phase 12 hooks here) |
| `tests/conformance_loader.rs` | `:356-386` `assert_replay_equivalence` — byte-exact `mode_state`, set-exact `seen_message_ids`. The strictest gate in the repo. |

**No precedent:** every runtime-emitted entry today is Internal, senderless, message-id-less, not
mode-dispatched on replay, not published, not counted in accepted ordinals. Phase 11 inverts all six.

### Server / watch_sessions (G3)
| Path | Purpose |
|---|---|
| `src/server.rs` | `:118` the ONLY `message_id` validation (non-empty) · `:164-192` `session_to_metadata` · `:560-634` StreamSession subscribe-window dedupe · `:1220-1222` the stream type · `:1263-1362` `list_sessions` — **the pattern to mirror** (`:1289-1302` clamping, `:1335-1338` cursor from `page_ids.last()`) · `:1363-1439` `watch_sessions` (`:1372` subscribe-before-snapshot — must stay above `:1374`; `:1380` `get_all_sessions`; `:1380-1392` the snapshot loop; `:1398-1402` `Lagged` → `RESOURCE_EXHAUSTED`; `:1422-1426` the `synced` dedup) |
| `crates/macp-storage/src/registry.rs` | `:151` `RwLock<HashMap<..>>` (no ordered index) · `:263-274` `get_all_sessions` — the deep clone · `:276-321` `session_ids_after` + its not-a-snapshot doc · `:445-529` its tests |
| `crates/macp-auth/src/security.rs` | `:54/:58` page-size constants · `:85-97` `SecurityLayer` (5 private + 3 pub fields → a new pub field is NOT a semver break) · `:165-198` `resolve_list_sessions_page_sizes` + its named-struct guard · `:245-248` env read |
| `src/pagination.rs` | crate-private cursor codec — zero public-API delta |

### CI / release
| Path | Purpose |
|---|---|
| `.github/workflows/ci.yml` | `:93-94` root `cargo metadata --locked` (**Check (MSRV)**, what #153 trips) · `:517-533` the integration lock guard + its carve-out · `:295-325` the lockstep assertion · `:555-629` conformance oracle (byte-diffs `tests/conformance/` both ways) |
| `.github/workflows/release-plz.yml` | `:69-74` the `workflow_call` into publish · `:92-389` `sync-integration-lock` (`:105` the `prs_created` guard, `:375-389` the new-head-SHA notice) |
| `release-plz.toml` | `:20` `semver_check = true` · `:27` `publish = false` · `:45-65` the `macp` version group |

**12 required contexts, `strict: true`, zero required reviews:** Check (MSRV), Format, Clippy,
Rustdoc, Test, Build, Coverage, Crate Dependency Isolation, Conformance oracle, Feature-gated code,
Integration, Docker Image Build (gate). **Security Audit and All Checks Passed are NOT required.**

### Build footprint
`target/` 2.3G · `integration_tests/target/` 1.2G when warm. Both **empty at planning time** (pruned),
so phase 1's build is fully cold. 55Gi free. See the plan's Open question 2.

## Environment constraints (every phase)
- Prefix every cargo command `RUSTC_WRAPPER=""`. Never `sccache --stop-server`.
- Never `pkill`. Live runtimes use scratch port 50123; kill only the PID you started.
- `CLAUDE.md` is gitignored (`.gitignore:20`) — its edits never appear in a PR diff. Say so in the
  phase report. The three **tracked** env tables are `docs/deployment.md`, `README.md`, `docs/API.md`.
- `tests/conformance/` is vendored and byte-diffed by CI — never edit it here.
- Spec repo (`../multiagentcoordinationprotocol`): **read freely, file issues, nothing else.**

## Phase log
- 2026-09-10: Plan written (Opus, 4 fan-out readers).
- 2026-09-10: Reverify round 1 (fresh Opus) → **REVISE**: 5 BLOCKER, 9 SHOULD-FIX, 6 NICE-TO-HAVE.
  All applied to the plan; see its `## Plan review` section. Material changes: Phase 4 narrowed to the
  out-of-schema negative-weight case (it was shipping work the plan itself deferred, and the flip is
  fail-open for declines); Phase 7 redesigned from paged chunking (O(N²/k)) to a single id pass, which
  removed its env var and collapsed Phase 8; Phase 2 gained `voting.quorum` constraints, a `count`
  alias decision, and a dry-run mode. Repo-map line numbers corrected (~20 drifts).
- 2026-09-10: `/drive` preflight clear — lock written, 79Gi free, nothing to prune, autocompact on.
  Starting Phase 1.
- 2026-09-10: **Phase 1 DONE** (`c0a2250`, branch `dependabot/cargo/major-updates-159dc18219`).
  Executor: Opus. **Verifier: NONE — deliberate.** A one-line dev-dependency version bump with
  `cargo metadata --locked`, `cargo check --all-targets`, `cargo fmt --check` and
  `cargo clippy --all-targets -- -D warnings` all green locally is tier-1 reversible under /drive's
  autonomy ladder; the real verification is CI compiling the bench, which the `--locked` guard had
  always pre-empted. Recorded as a call, not an omission.
  Files: `Cargo.toml` (line 115 only). `integration_tests/Cargo.lock` confirmed unchanged and
  criterion-free (`grep -c` → 0); CI's own second guard
  (`cargo metadata --manifest-path integration_tests/Cargo.toml --locked`) run locally, exit 0.
  Proven rather than assumed: `benches/replay_bench.rs` compiles against criterion 0.8.2 with zero
  warnings — no bench source edited. Shipping: rides PR #153, CI running at head `c0a2250`.
- 2026-09-10: **Phase 2 executed** (`7f55ccb`, branch `feat/policy-schema-conformance`, +1126/-15).
  Executor: Opus. Verifier: fresh Opus → **GAPS (0 BLOCKER, 5 SHOULD-FIX, 6 NICE-TO-HAVE)**, round 1.
  Fixer (Opus) dispatched for all 5 SHOULD-FIX + 4 NICE-TO-HAVE. Accumulating toward the G2 PR —
  verifier agreed with the plan's call and gave three reasons, the strongest being that shipping
  Phase 2 alone would advertise #145 closed while the `"*"`-mode fail-open leaves the fractional
  threshold reachable end to end; Phase 3 is what makes it unreachable.
  Gate at the time of the verdict: 25 test binaries green, macp-policy 134→161 tests, tier-1 118
  passed, fmt/clippy/rustdoc/doc-tests clean, both lockfiles `--locked` green (no dependency added).

  **Five things the plan got wrong, found by executing it** (all now corrected in the plan file or
  carried as deliberate deferrals):
  1. Acceptance criterion 9 as written was **not implementable**. `std_policies()` cannot go through
     `validate_definition` — `policy.default` is refused by the reserved-id guard first, and all four
     built-ins would fail the duplicate check anyway because `PolicyRegistry::new()` inserts them
     **directly into the HashMap, bypassing `register` entirely** (`registry.rs:74-81`). The plan's
     stated hazard ("a bad validator takes the process down at startup") is therefore **false** — the
     real failure mode is a silent inconsistency, a registry serving a policy it would itself refuse.
  2. Criterion 11's premise was wrong. `{"value": 75.0}` is a **legal** JSON Schema 2020-12 `integer`
     (`integer` matches any number with zero fractional part). The check that actually bites is
     `value.fract() != 0.0`. Verified empirically both ways.
  3. `docs/policy.md`'s quorum example spelled the field `threshold_type` (the Rust name) where serde
     renames it to `type`, so the documented example silently fell back to the default `n_of_m`.
  4. **A reachable fail-open the plan's constraint list does not close**, deliberately left for
     Phase 3: quorum `threshold` is validated only for `mode == "macp.mode.quorum.v1"`, but a policy
     registered with `mode: "*"` binds to quorum sessions (`runtime.rs:424`) and is validated against
     the *Decision* schema only. Verifier reproduced it end to end: `"*"` + `threshold:
     {"type":"weighted","value":0.5}` registers Ok, `effective_threshold` computes `0.5 as u32 == 0`.
     Net effect is masked by the evaluator's `.ceil()` to 1, so it commits with 1 approval where the
     ApprovalRequest demanded N — exactly the divergence Phase 3 unifies.
  5. `MACP_POLICIES_DRY_RUN` required **new public API** (`PolicyRegistry::validate_dir`,
     `PolicyFileOutcome`) because `load_from_dir` stops at the first rejection. Additive, so
     cargo-semver-checks will not block, but it is public surface the plan did not anticipate — and it
     lands in two surfaces (`macp_policy::registry::*` and `macp_runtime::policy::*` via `lib.rs:32`).

  **Two operational risks recorded for Phase 6's changelog, neither a code defect:**
  - A deployment whose `MACP_POLICIES_DIR` holds a now-invalid file **will refuse to start**. Correct
    fail-closed behaviour, and the reason dry-run exists, but it is an operational breaking change and
    a `feat:` commit alone produces a minor bump with no upgrade note.
  - The obvious operator workaround **silently voids governance**: deleting the offending file makes
    `replay.rs:317-318` re-resolve `policy_version` to `None` via `.ok()`, and
    `util.rs:125-127` then short-circuits — in-flight sessions bound to that policy lose their policy
    enforcement with no error. Pre-existing path; this phase is what makes it reachable.

  **Wire-visible change with a downstream consequence:** three pre-existing rejections gained the
  `INVALID_POLICY_DEFINITION:` prefix. `macp-control-plane/src/controllers/runtime.controller.ts:120`
  branches on that prefix to return HTTP 400 rather than 200, so those three now surface as 400.
  Almost certainly the intended alignment, but nobody had flagged it.
- 2026-09-10: **Phase 2 DONE** — round 2 verdict **PASS** at `46de083`. Fixer closed all 5 SHOULD-FIX
  + 4 NICE-TO-HAVE; one instruction was reversed mid-flight on my call (see below). Verifier re-ran
  every gate independently: **719 tests passed** across 25 binaries, fmt/clippy clean, both lockfiles
  `--locked` green, tier-1 policy suite 24 passed. All 12 acceptance criteria met.
  **Reversal, decided by the orchestrator and logged here rather than left implicit:** the fixer
  initially prefixed all six un-prefixed rejections with `INVALID_POLICY_DEFINITION:`. I had it revert
  the prefix on the duplicate-registration case only. A duplicate `policy_id` is a **conflict**, not a
  malformed definition — the descriptor may be entirely valid — and stretching RFC-MACP-0012's code to
  cover a namespace collision makes it mean two things. Deciding factor: it was the one change of the
  six with a cross-repo behaviour change and **no test anywhere that would catch it**
  (`macp-control-plane/src/controllers/runtime.controller.ts:120` branches on the prefix to return
  HTTP 400; its own spec mocks an un-prefixed string so it stays green either way). Now pinned by a
  negative assertion at `registry.rs:639`. Phase 2 ships with zero unflagged cross-repo change.
  Two fixer findings that corrected the verifier: `--exact` alone does NOT make libtest fail on a
  zero-match filter (it prints `0 passed` and exits 0), so the CI parity step also asserts
  `test result: ok. 1 passed`; and the `refuse()` helper was never at risk because it constructs its
  own registry, which is stronger than "every call site uses a fresh one".
- 2026-09-11: **Phase 1 SHIPPED.** PR #153 merged as `537c079` on `main` — it self-merged on green
  (dependabot had auto-merge enabled). All 12 required contexts + "All Checks Passed" green, and this
  run genuinely compiled the bench against criterion 0.8.2. **G1 complete.**
  CI incident worth recording: the first run's "Feature-gated code" job **hung for 34 minutes** on
  `Backend smoke test through gRPC (rocksdb)` against a ~5-minute historical norm, with every compile
  step already green — i.e. a wedged gRPC smoke test, not a criterion problem. Cancelled and re-ran
  `--failed` (the other 14 jobs kept their green status rather than burning a full cycle). The re-run
  passed. Note the second run was slower only because cancelling invalidated the cargo cache and it
  rebuilt rocksdb from source — that was NOT a second hang, and I corrected an earlier claim that it
  was. The original 34-minute stall has one data point; if it recurs it deserves its own issue.
  Consequence for G2: `main` is now `537c079`, branch protection is `strict: true`, so
  `feat/policy-schema-conformance` (cut from `999890e`) must be rebased before its PR can merge.
- 2026-09-11: **Phase 3 committed** (`23d0ad2`). Executor: Opus. **Verification was done in two parts**
  because the machine suspended twice, killing the verifier agent mid-run both times (API error, not
  agent failure). Rather than keep respawning long agents into an unstable environment, the
  orchestrator verified the three highest-risk items directly — recorded here so the split is visible:
  1. **Float→int casts in the new shared resolver** (`crates/macp-core/src/policy/rules.rs:305-336`):
     CORRECT. `value.partial_cmp(&0.0) != Some(Greater)` routes NaN, `-0.0`, negatives and `0.0` to
     `Inert` on one path (NaN yields `None`, so it cannot slip through); `percentage` with
     `total_participants == 0` returns `Unsatisfiable`; the unrecognised-type arm fails closed; and
     `(required.ceil() as u32).max(1)` is safe because Rust float→int casts **saturate** (an absurd
     `1e300` becomes an unmeetable-but-finite `u32::MAX`, not a wrapped small bar). The code comments
     state each of these reasons, so the invariants are documented where they are enforced.
  2. **RFC-MACP-0011 adjudication:** the executor's reframing is CORRECT, and the plan's premise was
     wrong. §4a's own formula — "eligible for negative `Commitment` when
     `(remaining_eligible + current_approvals) < required_approvals`" — is **literally satisfied at
     zero ballots** when `required > participants`, so the zero-ballot decline is not a violation and
     a naive reject-floor would have been wrong. The real defect is §6: the policy `threshold`
     *replaces* `required_approvals`, a field the mode already constrains to `1..=participants`
     (`quorum.rs:144`), and nothing held the replacement to that domain. Refusing at ApprovalRequest
     time is therefore the root fix, not symptom management. The `counted > 0` guard on
     `commitment_ready`'s unreachable branch cannot break a legitimate §4b decline, because every §4b
     case ("all eligible participants have abstained", "abstentions and rejections make the threshold
     unreachable") involves ballots actually cast.
  3. **The replay break is REAL**, confirmed at `src/replay.rs:133` — `mode.on_message_at(...)?`
     propagates, so a persisted quorum session whose bound policy threshold now falls outside
     `1..=participants` fails to replay (skipped with a warning, or fatal under
     `MACP_STRICT_RECOVERY`, `src/main.rs:258`). **Orchestrator judgment: a changelog entry is
     sufficient, and Phase 6 owns it** — but it must tell operators how to DETECT it, not just that it
     exists. The mitigation already in the code is the `tracing::warn!` at `quorum.rs:200-212`, which
     names the session, the policy id, the computed threshold and the participant count. Reaching the
     state requires a policy registered before Phase 2's validation existed, and such a session could
     only ever have sealed a negative outcome with zero approvals — i.e. it was already broken. Losing
     it loudly on replay is better than replaying it into a state that seals nothing legitimately.
- 2026-09-11: **Phase 3 verification completed** → **PASS**, 3 SHOULD-FIX (all documentation).
  Verifier ran `cargo semver-checks check-release` rather than reasoning: **exit 0, purely additive**
  (196 checks pass / 58 skip per crate). Confirmed **Phase 5 was NOT pre-empted** —
  `QuorumMode::effective_threshold` is still private, so Phase 5 still owes all five criteria.
  Three doc gaps closed in `72c9e2c`: the undocumented `#[non_exhaustive]` omission on
  `EffectiveThreshold` (it departs from six documented precedents in the same crate, so the reason now
  sits in rustdoc plus an ASSUMPTIONS entry), an ASSUMPTIONS miscitation, and two hard-coded "v0.7.5"
  strings in `docs/policy.md` written before release-plz has computed the number.
  Plan file corrected in three places where execution proved it wrong: Phase 3's Files list (too
  narrow — the shared resolver legitimately required `macp-core` and the registry), Phase 3's
  criterion-6 premise, and two stale passages in Phase 5's text.
- 2026-09-11: **Phase 4 DONE** (`3d73258`). Executor: Opus. Gate: **734 passed / 0 failed**, fmt and
  clippy clean. Diff purely additive (270 insertions, 0 deletions). All 6 criteria tested; every new
  test mutation-checked (removing `< 0.0` reddens exactly the three negative tests; removing `== 0.0`
  reddens only the zero test; removing the short-circuit reddens the guard test and both protected
  §4.1 tests). Nothing in the exclusion list touched — the short-circuit stands, `unanimous`
  unchanged, no dead zero-denominator branch added to `supermajority`.
  **The plan's diagnosis was wrong twice** (full detail in the plan's Phase 4 correction block): the
  `{"a":1.0,"b":-1.0}` example used throughout sums to exactly `0.0` — the deferred case — and a
  negative total never returned `NoVotes` at all, it fell through to a division whose negative
  denominator INVERTS the comparison and could report `Passed`. So the real defect was more severe
  than described: it allowed a positive commitment over a reject from the only in-schema voter.
  Sharpest part: `PolicyDecision`-only assertions would have passed against the UNFIXED code for two
  of three criteria, because those rounds were already denied for the wrong reason — so the tests
  assert the `VotingResult` variant directly. Fourth instance in this repo of a green signal not
  measuring what it appeared to measure.
  Three ASSUMPTIONS entries appended: the decline-direction trade, plus the two "record, do not fix"
  items (the `supermajority` silent 2/3 substitution, and `unanimous` passing vacuously on an empty
  participant set).
- 2026-09-11: **Phase 5 DONE** (`e246db5`). Executor: Opus. Gate: **739 passed / 0 failed**, fmt,
  clippy and `RUSTDOCFLAGS="-D warnings" cargo doc` all clean, both lockfile guards pass.
  `cargo semver-checks check-release -p macp-core -p macp-runtime -p macp-modes`: exit 0, nothing
  flagged. Closes #146 — downstream can delete its mirror.
  **Fourth consecutive phase to find a real plan error, and this one was structural:** every
  signature the plan considered had exactly two outcomes, but the session-level accessor must decode
  `session.mode_state` and that decode can fail — a third outcome no listed option could express.
  `QuorumState`'s fields are not `#[serde(default)]` (verified empirically), so a wrong-mode session
  errors rather than silently reporting "no request" — a useful property neither the plan nor issue
  #146 accounted for. Forced a `Result<Option<ApprovalThreshold>, MacpError>` layering.
  Rustdoc carries the non-monotonicity warning with a concrete counterexample (3 participants,
  `required=3`: ready at 0, ready at 1, **not** ready at 2, ready at 3) — which is precisely why the
  downstream reporter's binary search over `commitment_ready` returned a confident wrong answer and
  they had to use a linear sweep.
- 2026-09-11: **Phases 6 + 14 DONE** (`0b4bfc5` docs/upgrade path, `072d159` tracked-record
  corrections). Gate: **739 passed / 0 failed** — unchanged, as doc-only work should be. fmt/clippy
  clean. **G2 is code-complete: phases 2, 3, 4, 5, 6, 14.**
  `docs/deployment.md:17-56` now carries "Upgrading into registration-time policy validation", the
  human-facing note for the three operational breaks G2 ships. It exists because `CHANGELOG.md` is
  generated by release-plz from commit subjects and structurally cannot carry this detail, and
  rewriting five final commits to force it in would require a force-push.
  **Fifth consecutive phase to find a real plan error, two findings:**
  1. **Phase 6 criterion 4 ("#145/#146/#148 closed") overreaches on #148 — orchestrator override,
     the executor recommended closing and I decided against it.** The reporter's defect 2 is
     specifically "cancelling weights return NoVotes on a full ballot set: `{a:1.0, b:-1.0}` with both
     voting" — and that example sums to **exactly zero**, the schema-legal case this plan deliberately
     defers to spec #98 item 3. Closing #148 would advertise as fixed the precise scenario they
     reported. **#148 stays OPEN** with a comment naming both fixed halves and the upstream residue;
     it closes when the schema moves. #145 and #146 do close.
  2. **Phase 6's `CLAUDE.md` work was half-done and my brief inherited the wrong state** — I asked the
     executor to *confirm* the stale `v0.5.0` header was fixed; it never was, nor was the second false
     claim the plan itself names (the bare `schemas/json/policy/*.schema.json` path, with no
     `schemas/` directory in this repo). Both fixed; the header now points at
     `[workspace.package].version` so it cannot go stale again. **The mechanism is the lesson: because
     `CLAUDE.md` is gitignored, a "done" claim about it can never be checked against a diff.** That is
     how it survived, and it is the fifth instance in this repo of a green signal not measuring what
     it appeared to measure.
  Two items correctly punted to the orchestrator: Phase 6's criteria 2 and 3 (release-PR lockstep,
  seven crates on crates.io) are unreachable without push/merge. One new gap recorded, out of scope:
  `MACP_POLICY_SCHEMAS_DIR` — which the CI parity test reads to find the spec schemas — is documented
  in no env table, so a contributor cannot run that test locally without reading the test body.
