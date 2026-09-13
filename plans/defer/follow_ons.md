# Actionable Follow-ons (post-v0.5.0)

**Created:** 2026-07-05, at the close of the improvement-plan execution
(v0.5.0 released; all seven crates on crates.io). These items are **not
blocked** — they are scoped, ready work that deliberately did not gate the
release. Distinct from `README.md`'s hard-blocked table.

Ordered by value:

## 1. Handoff implicit-accept timer (runtime implementation) — **DONE** (2026-09-13)
Shipped in G4 (`plans/backlog-closeout-2026-09.md` phases 9–13, released as
0.8.0). Every clause below landed as written:

- **Synthetic accept in accepted history** — `Runtime::synthesize_due_accept`
  (`src/runtime.rs`) appends an `EntryKind::Incoming` entry, so it consumes an
  accepted ordinal and is published to `StreamSession` subscribers.
- **Timing from the offer's recorded acceptance time, excluding suspended
  time** — `HandoffOfferRecord.offered_at_ms` plus
  `suspended_ms_at_offer`, with the deadline walked through
  `Session::unsuspended_deadline` so a pause *after* the deadline cannot move
  a timestamp already fixed.
- **Eager sweep + lazy emission** — `Runtime::sweep_due_synthetic_accepts` on
  the background maintenance loop (`src/main.rs`, ordered *after*
  `cleanup_expired_sessions` so TTL expiry keeps precedence), plus the
  before-dispatch call in `process_message`. Not "before commitment"
  specifically: before **any** session-scoped message, per §5.1(2).
- **Runtime-emitted envelope** — sender = the offer's target, `implicit: true`,
  `message_id` = `implicit-accept:<handoff_id>`, `timestamp_unix_ms` and the
  entry's `received_at_ms` both fixed at the computed deadline.
- **Client-submitted implicit accepts rejected** —
  `HandoffMode::validate_client_envelope` refuses `implicit = true`
  (`InvalidPayload`) and reserves the whole `implicit-accept:` `message_id`
  prefix for every message type (`InvalidEnvelope`).
- **`semantics_rev = 2` gate** — `macp_core::session::CURRENT_SEMANTICS_REV`;
  rev ≤ 1 sessions keep the A6 interim in-`Commitment` inference and their
  wire behavior is byte-identical to earlier releases.

Two consequences that are deliberate and documented rather than oversights, so
they are not re-opened as defects: the target's `message_count` in
`SessionMetadata.participant_activity` does **not** advance for the synthetic
entry (replay records activity for no entry kind, so crediting it live would
fork live from replay), and a *rejected* trigger message can now leave a
runtime-originated entry in accepted history — the freeze-profile carve-out
argued in full on `Runtime::synthesize_due_accept`. The dedup half of that
invariant is untouched: the rejected message's own `message_id` stays free.

Docs: `docs/modes.md` (Handoff → Implicit accept, Revision gating),
`docs/API.md` (Send → reserved `message_id` namespace; Background
maintenance), `docs/deployment.md`, `docs/architecture.md`.

## 2. `watch_sessions` initial-sync memory bound — **DONE** (2026-09-13)
Both halves have now shipped; re-verified against `src/server.rs` and
`src/watch_sync.rs` at HEAD.

The `list_sessions` half shipped first: `page_size` capping + opaque
`page_token` / `next_page_token` are implemented in `server.rs`
`list_sessions` (keyset cursor over session IDs, bounded by
`MACP_LIST_SESSIONS_{DEFAULT,MAX}_PAGE_SIZE`). The original "replace the
documented server-side cap" clause was **stale** and was dropped: `docs/API.md`
documented no cap and the handler applied none, so there was nothing to
replace. Proto fields for `list_sessions` shipped in 0.1.6 (spec PR #51);
master plan §3.4.

The `watch_sessions` half shipped in `882beeb` — *perf(server): bound the
WatchSessions initial sync* (#161). The memory was never in the event count:
it was in the up-front `SessionRegistry::get_all_sessions()`, which deep-cloned
every `Session` into one `Vec` that then stayed resident for the whole sync —
a sync paced by the client's reads, times up to `MACP_MAX_CONCURRENT_STREAMS`
concurrent streams, i.e. a copy of the registry per stream.
`crate::watch_sync::InitialSync` replaces it with a one-time snapshot of the
registry's shared `Arc<Mutex<Session>>` **handles**, locked and cloned one at a
time, so peak resident `Session` clones is **one** regardless of registry size —
structurally, since `InitialSync` has no `Session` field. Deliberately *not*
paged through `session_ids_after`: that primitive scans every key per call, so
driving the traversal through it would be O(N²) whole-map scans.

The chunking / documented-cap option this item floated was correctly **not**
taken: `core.proto` requires the initial sync to carry every session currently
in the registry, so a truncating cap would have been non-conformant, and
`WatchSessionsRequest` is empty in the proto so there is nothing to paginate
without an upstream field. The emission is a lazily polled
`async_stream::try_stream!`, already backpressured by HTTP/2 with no unbounded
channel on the path, so the event *count* was never the memory problem it was
written up as. One residual O(N) allocation is accepted and documented in
place: the `synced` `HashSet<String>` of session IDs, bounded by the registry
size at subscribe time and read-only afterwards. The same commit also bounded
the lifecycle events arriving *during* the sync
(`watch_sync::PENDING_EVENT_LIMIT`, 16x the 64-event bus), which otherwise made
the first post-sync `recv()` return `Lagged` and kill the stream.

## 3. Multi-round JSON client fallback removal (one release after 0.5.0)
Per master §4.5 step 5: stop advertising/accepting JSON `Contribute` from
NEW clients one release later. NOTE the A7 design decision: the JSON-first
*parse order* is permanent (replay safety — see the A7 commit / BUILD_STATUS
2026-07-05); "removal" therefore means documentation + example/SDK-level
deprecation, not changing the parse path for existing histories.

## 4. Persistent file handles for active session logs
B3 deferred piece: FileBackend opens/fsyncs/closes per append. D1/D2
benchmarks exist (fsync ~12ms dominates; 8-session contended send 60ms).
Keep a handle per active session log; measure against the benches.

## 5. Snapshot debounce / latest-wins persistence
D2's noted future lever: 2 fsyncs per message (durable append + snapshot);
log is authoritative, snapshots best-effort — debouncing snapshots halves
the fsync load on the hot path. Needs care with shutdown/crash windows
(replay covers, but recovery time grows).

## 6. Windowed dedup set for long-lived sessions
`seen_message_ids` grows per accepted message and re-serializes on every
snapshot (master §3.2). Dedup is normative — a windowed design needs spec
coordination first (file upstream before implementing).

## 7. Built-in recommended policies — **DONE** (2026-09-10)
Master plan §4.2's companion: ship recommended governance profiles as
pre-registered built-ins once the spec repo reserves the identifiers and pins
their canonical rule definitions (filed as spec issue #55).

**Shipped.** The reservation landed as RFC-MACP-0012 §2.2/§5.2, which assigns
the `policy.std.` prefix rather than the bare `policy.` names this item
anticipated. `crates/macp-policy/src/defaults.rs` pre-registers
`policy.std.majority`, `policy.std.supermajority` and `policy.std.unanimous`
(`STD_POLICY_PREFIX` at `:32`, `canonical_std_policy` at `:127`), and the
collision risk this item worried about is closed from the other direction
too: `PolicyRegistry::validate_reserved_namespace` refuses any registration
under the prefix that is not the canonical definition for that exact
identifier, and refuses unregistration of the whole namespace. Covered by
tier-1 policy-registry tests and `std_policies_all_require_vote_quorum`.

## 8. Small/cosmetic
- Duplicate-SessionStart ack during a failed start's rollback window can
  report `duplicate=true` with a non-Open state (adversarial-review
  advisory; self-corrects on retry).
- Disk-GC sweep loads each stored session per cycle — O(stored sessions)
  I/O; optimize only if session counts grow very large (change-review D6).
- ~~Tier-1 suite has no suspend/resume RPC coverage (noticed during the
  max_suspend_ms work; runtime/core level is covered).~~ **DONE** —
  `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` covers the
  RPC pair with three tests: `suspend_resume_lifecycle`,
  `suspend_from_non_initiator_rejected`, `suspend_unknown_session_not_found`.
- Propose the `rules.audit` block (E3's audit-verbosity vocabulary) upstream
  — currently runtime-specific, harmless to other implementations.
- Upstream: `abstention.counts_toward_quorum` wording references a
  participation-quorum concept that schema_version ≤2 no longer has
  (flagged in spec PR #48 for a future schema_version alongside any real
  participation-quorum field).

## 9. `SessionSuspend`/`Resume`/`Cancel` are emitted as `Internal`, not accepted history
**Confirmed non-conformance, found 2026-09-11 while settling Phase 11 of
`plans/backlog-closeout-2026-09.md` against the RFC text** — not a
speculative cleanup.

RFC-MACP-0001 §7.5 puts the runtime-emitted `SessionSuspend`, `SessionResume`
and `SessionCancel` envelopes **in the session's accepted history**. This
runtime emits all three through `Runtime::make_internal_entry`
(`src/runtime.rs:816,875,933`), which writes `EntryKind::Internal`,
`sender: "_runtime"` and an empty `message_id`. `log_store.rs:128` counts
accepted ordinals as `Incoming` only, so these entries are not in accepted
history by this runtime's own definition of the term. They are also not
mode-dispatched on replay, not published to `StreamSession` subscribers, and
`replay.rs:162`'s `_ => {}` arm silently ignores `Internal` types it does not
recognize — so an old binary replaying a newer log diverges with no error.

Why it surfaced now: RFC-MACP-0010 §5.1(2) specifies the handoff synthetic
accept as "the same construction as runtime-emitted `SessionSuspend`/
`SessionResume`/`SessionCancel` envelopes (RFC-MACP-0001 §7.5)". Item 1 above
therefore had to decide `Incoming` vs. `Internal` for the synthetic accept,
and the RFC's analogy settles it as `Incoming` — which is what makes the
existing `Internal` treatment of the other three a divergence rather than a
defensible local choice. Item 1 deliberately scopes itself to the synthetic
accept and does **not** fix these three; changing them is wire-visible
(subscribers begin seeing three envelope types they never saw) and changes
accepted ordinals for every existing session, so it needs its own
`semantics_rev` gate and its own release note.

Not urgent: nothing is known to depend on the current behaviour, and the
gap has existed since these envelopes were introduced. Sized as its own
phase whenever it is picked up, not as a rider on other work.

## 10. `make_internal_entry`'s second clock read can drop a session at startup
**Found 2026-09-11 by the Phase 10 verifier of
`plans/backlog-closeout-2026-09.md`, with the consequence analysis corrected
upward from the executor's first read.**

`RuntimeCore::suspend_session` (`src/runtime.rs:870`) and `resume_session`
(`:923`) each take their own `Utc::now()` for the session mutation, while
`make_internal_entry` (`:272`) takes a **second** `Utc::now()` for the log
entry. So live `accumulated_suspended_ms` and the value replay reconstructs
from recorded `received_at_ms` can differ.

**The window is genuinely tiny, and for a better reason than "it's fast":**
between the two reads there is no `.await`, no lock acquisition and no I/O —
two `to_string()`s and one `prost::encode_to_vec`, with the session mutex
already held. It is not widened by fsync latency, lock contention or tokio
scheduling, only by an OS preemption landing between two adjacent synchronous
statements. Better still, the two errors **cancel**:
`replay_banked − live_banked = δ_resume − δ_suspend`, a difference of two
identically-shaped windows rather than a sum. Realistic bound: **±1 ms** from
millisecond truncation at a tick boundary.

**But the consequence is worse than a 1 ms deadline shift.** Since Phase 10
this value feeds an implicit-accept accept/reject decision. If a 1 ms flip
lands — only when unsuspended elapsed sits within 1 ms of
`implicit_accept_timeout_ms` — the live session **Resolved** while replay
yields `InvalidPayload`, so `replay_session` returns `Err`, `src/main.rs:385`
logs `"failed to replay session; skipping"`, and **the session is never
inserted into the registry**: it silently vanishes on restart. Under
`MACP_STRICT_RECOVERY=1` startup aborts instead. `validate_replay_consistency`
never runs in that arm, and would not catch it anyway — it compares neither
`mode_state` nor `accumulated_suspended_ms` (see item 11). Probability tiny,
severity high.

**The fix is one line and kills the class for TTL banking too:** thread the
already-read `now_ms` into `make_internal_entry` as a parameter, exactly as
`make_incoming_entry(env, accepted_at)` already does (`runtime.rs:247`, `:540`,
`:669`). Deferred out of Phase 10 only because Phase 11 already touches
`runtime.rs`; it should land there.

Related, same code path, much smaller: `SessionResumePayload.banked_ms`
(`runtime.rs:924-931`) is computed from the live clock, while replay recomputes
banking from `received_at_ms` (`replay.rs:159-166`) and **ignores the field
entirely**. The log therefore persists a `banked_ms` that can disagree with the
value replay derives. Dead and mildly misleading — either consume it on replay
or document it as informational.

## 11. `validate_replay_consistency` compares neither `mode_state` nor suspension state
Pre-existing, but its priority rose on 2026-09-11. `src/replay.rs:172-215`
compares session state, dedup **count**, participants and bound versions — not
`mode_state`, and not `accumulated_suspended_ms`. A rev-1 session replayed by a
rev-2 binary can therefore diverge **invisibly** in production.

Phase 11 of `plans/backlog-closeout-2026-09.md` already flags this as
"consider closing in this phase". What changed is that Phase 10 made
`mode_state` divergence a *semantics-revision-gated* possibility rather than a
theoretical one, and item 10 above is a concrete path to it. Worth closing with
Phase 11 rather than deferring again.

## 12. Mode-state records are exhaustively constructible public API — **RESOLVED for the sealed set** (2026-09-13, 0.8.0)
Resolved in G4 as part of the 0.8.0 release — see `DECISIONS.md` D7.
**The general rule is recorded here because it still governs every record not
in the table below**: any new field on a `pub` record with all-pub fields and
no `#[non_exhaustive]` is a `constructible_struct_adds_field` **major** semver
break. `release-plz.toml` sets `semver_check = true`, so it blocks the release
PR rather than failing quietly, and the single `version_group` moves all seven
crates together.

Sealed in 0.8.0 (`#[non_exhaustive]`), so **fields added to these from 0.8.0
forward are additive**:

| Crate | Records |
|---|---|
| `macp-modes` (handoff) | `HandoffOfferRecord`, `HandoffContextRecord`, `HandoffState` |
| `macp-modes` (quorum) | `ApprovalRequestRecord`, `BallotRecord`, `QuorumState` |
| `macp-modes` (proposal) | `ProposalRecord`, `TerminalRejectRecord`, `RejectRecord`, `ProposalState` |
| `macp-modes` (task) | `TaskRecord`, `TaskRejectRecord`, `TaskUpdateRecord`, `TaskCompleteRecord`, `TaskFailRecord`, `TaskState` |
| `macp-modes` (multi_round) | `MultiRoundState` |
| `macp-storage` | `PersistedSession` |

`PersistedSession` was the "not audited as part of D7" sibling this item
flagged. **The audit in Phase 13 found it was not a sibling at all — it is one
of the two breaks 0.8.0 was already forced to take.** Against the published
0.7.6, `cargo semver-checks check-release --workspace --baseline-version 0.7.6`
reports exactly two `constructible_struct_adds_field` failures:

```
macp-modes    field HandoffOfferRecord.suspended_ms_at_offer  crates/macp-modes/src/mode/handoff.rs:87
macp-storage  field PersistedSession.suspension_intervals     crates/macp-storage/src/registry.rs:50
```

D7 was written believing both were on `HandoffOfferRecord`. Sealing
`PersistedSession` therefore spends nothing extra — the major it would have
forced is the major already being taken — and it removes the *next* one.

**Still open — the residue, deliberately left:**

- `macp-storage`'s `PersistedRoot` (`registry.rs`) is unsealed. It mirrors the
  proto `Root` message, which is `{uri, name}` and has not changed, so it is
  not in the growing class. Seal it if a third field ever appears.
- The **enums** in the sealed modules — `HandoffDisposition`, `BallotChoice`,
  `ApprovalThreshold` — are unsealed **on purpose**, not by omission. A
  `#[non_exhaustive]` enum forces a `_` arm in external `match`es, which
  silently reinterprets a future variant as one of today's; for
  `ApprovalThreshold` that is the exact defect class issue #145 was. Adding a
  variant is already the major lint `enum_variant_added`. Do not "finish the
  sweep" by sealing these.
- The **`macp-core` decision types** — `DecisionState`, `Proposal`,
  `Evaluation`, `Objection`, `Vote` (`crates/macp-core/src/decision.rs`) — are
  unsealed **on purpose**. They are not mode-state records in the same sense:
  `macp-core` is vocabulary, and these five are the argument types of the
  public `PolicyEvaluator` trait, i.e. the seam a consumer driving
  `macp-core` + `macp-modes` with its own evaluator sits on. They are also
  literal-constructed across crate boundaries **today** —
  `crates/macp-modes/src/mode/decision.rs` builds all five in production code
  and `crates/macp-policy/src/evaluator.rs` builds `DecisionState` fixtures in
  its tests — and `DecisionState` derives no `Default`, so sealing them without
  first designing constructors would leave a downstream evaluator implementor
  unable to build a fixture for their own trait impl. Sealing them is a real
  design task, not a free sweep; do it with constructors or not at all.
- The proposal, task and multi_round mode-state records **were** swept in
  0.8.0 (table above), on the ground that they are the same class on the same
  evidence — `ProposalState.rejections`, `ProposalState.phase`,
  `MultiRoundState.convergence_type` and `MultiRoundState.converged` all carry
  `#[serde(default)]`, i.e. each was added after the fact and each would be a
  major today — and that 0.8.0 was already being taken, so sealing them cost
  nothing extra. Nothing outside `macp-modes` constructs any of them: audited
  with `git grep` across the workspace, `tests/`, and the separate
  `integration_tests/` workspace that consumes these crates as path deps.

## 13. `validate_replay_consistency` still ignores `ttl_expiry` and `resolution`
Narrowed by Phase 11a of `plans/backlog-closeout-2026-09.md`, which added
`mode_state`, `accumulated_suspended_ms` and `suspended_at_ms` (closing the
worst of item 11). Two persisted, load-bearing fields remain uncompared
(`src/replay.rs:191-262`): `ttl_expiry` and `resolution`
(`crates/macp-storage/src/registry.rs:20,24`). `resolution` is the session's
actual outcome bytes, so a live/replay divergence there is invisible today.
`ttl_expiry` is now largely covered transitively, since it is derived from the
suspension state that *is* compared.

Also worth a one-line comment where it is cheap: the new `mode_state` byte
comparison rests on an undocumented invariant — every mode state serializes
through `serde_json` over `BTreeMap`s (`crates/macp-modes/src/mode/util.rs:162-164`;
decision/handoff/quorum/proposal/multi_round all verified), so key order is
deterministic. A future `HashMap`-backed mode state would emit spurious
warnings. Warn-only, so harmless, but the invariant should be stated at
`src/replay.rs:229-231` rather than rediscovered.

Minor doc staleness from the same change: `docs/change-review-phases-a-e.md:504,513`
enumerates the compared fields as "(state, dedup count, participants, bound
versions)" and claims "diverged state+dedup → 2". Both are now incomplete. That
file is a point-in-time change-review record rather than a living spec, so
leaving it is defensible; a "widened in Phase 11a" note is the tidy option.
## 14. The `count` quorum-threshold alias is now a departure the spec ruled against
**Status changed 2026-09-11 by spec #110** (`1bb30ad`, "close the threshold
vocabulary — remove weighted, pin ceiling rounding"), which closed spec issue
#98 item 4 **against** this runtime.

This runtime accepts `count` as an alias for `n_of_m` in a Quorum-mode
`threshold.type`. That was recorded as a documented local departure while the
vocabulary was open upstream. It is now closed, and the spec's reasoning is
substantive rather than stylistic:

- `count` already names a **participation floor** in Decision's `voting.quorum`,
  whereas Quorum's `threshold` is an **approval bar**. One identifier for two
  different concepts across two modes is the ambiguity #110 removes.
- RFC-MACP-0012 §8 makes policy identity **byte-level `rules` equality**. So the
  alias means two semantically identical policies — one written `count`, one
  written `n_of_m` — compare **unequal forever**, which breaks idempotent
  re-registration and cross-runtime replay equality.

The spec pins the refusal with `invalid-quorum-rules/threshold-type-weighted.json`
alongside `weighted`. **Nothing in our CI gates this**: the parity test
(`enum_lists_match_the_canonical_schemas`) filters `count` out by construction as
one of its documented departures, so the mirror can agree with canonical while the
accepted set does not.

Phase 7 of `plans/spec-99-schema-version-3.md` left the behaviour alone and only
re-labelled the docs (they now say prefer `n_of_m` and expect withdrawal).
**Removing it is a breaking change for any deployment that wrote `count`**, so it
wants its own release and a deprecation note, not a rider. Note the contrast with
`weighted`, whose removal Phase 7 *did* take: `weighted` was always **refused**
here, so deleting the constant that mirrored it changed nothing an operator could
observe beyond an error string.

## 15. `src/replay.rs`'s strict-`SessionStart` call site has the repoint trap untested
Phase 5 of `plans/spec-99-schema-version-3.md` added
`a_promoted_mode_still_gets_canonical_session_start_validation` covering the
runtime-side call site, after measuring that a naive repoint to
`validate_strict_session_start_payload` leaves the whole suite **green** — nothing
else exercises a promoted mode's `SessionStart` payload validation, so the trap
would ship silently.

The **replay-side** call site carries a comment pointing at that test's reasoning
but has no test of its own. Judged genuinely lower-risk and deliberately deferred:
a stored `SessionStart` payload in the log necessarily passed acceptance-time
validation, so relaxing replay for a promoted mode cannot admit anything
acceptance refused. Worth closing anyway, because the asymmetry is invisible from
the code.

## 16. The mid-session checkpoint fast path was untested for its entire life

**Found 2026-09-11** by the Phase 11b verifier (`plans/backlog-closeout-2026-09.md`),
and independently reproduced twice, so it is not a reading error.

`replay_from_checkpoint_restores_state` (`src/replay.rs:632`) does **not** test a
checkpoint. It builds its session with `start_payload_bytes()` (`src/replay.rs:407`),
which binds `policy_version: "policy-1"`, and then calls `replay_session(..., None)`
with no policy registry. `try_replay_from_checkpoint` therefore bails at
`src/replay.rs:61-69` — a checkpoint carrying a bound `policy_version` with
`policy_definition: None` cannot be trusted — and falls back to a **full replay from
the start of the log**. The test's three `seen_message_ids` assertions are satisfied
by that full replay, so they pass whether or not the checkpoint code works at all.
The name asserts coverage the test has never had.

Proof (both run against `15318d0`):
- replacing the bail-out at `src/replay.rs:69` with `panic!` makes this test **panic**,
  while `replay_from_checkpoint_restores_suspension_intervals` passes untouched;
- short-circuiting `try_replay_from_checkpoint` to `Ok(None)` (fast path disabled
  entirely) reds exactly two tests in the whole workspace, and this is not one of them.

**How much was actually covered.** `file_backend_full_lifecycle`
(`tests/file_backend_integration.rs:155`) does reach the fast path, but only in the
**terminal-compaction** shape — the log compacted down to a single checkpoint with
nothing after it — and it asserts only `state == Resolved`. The **mid-session** shape,
restore-from-checkpoint-then-replay-a-tail, had **zero** coverage before Phase 11b.
That is the shape `MACP_CHECKPOINT_INTERVAL` produces, i.e. the entire reason the
setting exists.

Phase 11b's `replay_from_checkpoint_restores_suspension_intervals` is the first test
that genuinely exercises it (unbound `policy_version` plus a snapshot tripwire value
no full replay can reproduce). **That is one test covering one field**, which is not
the same as the path being covered.

Worth doing, in rough priority order:
1. ~~Repair `replay_from_checkpoint_restores_state` so its name is true — or delete it,
   since a test that silently tests something else is worse than no test.~~ **Done
   2026-09-11** in the Phase 11b gap-closure pass: the fixture now binds an empty
   `policy_version` and asserts the same snapshot tripwire
   (`intent == "restored-from-checkpoint"`), so the fallback can no longer satisfy it.
   Mutation-proven: restoring the bound `policy_version` reds it. No defect was found
   in the fast path itself — the dedup assertions hold on the genuine path too.
2. Give the mid-session fast path a real matrix: entries after the checkpoint,
   dedup-slot restoration, TTL/expiry interaction, `mode_state` survival, and the
   bail-out conditions at `src/replay.rs:61-69` each asserted to bail *for the reason
   claimed* rather than incidentally.
3. Audit every other test whose name claims checkpoint coverage for the same trap —
   a bound `policy_version` with a `None` registry is silent, not an error.

## 17. `process_message` instantiates the mode three times per envelope

Found by the Phase 11c verifier (NIT-5). `authorize_sender`, `validate_client_envelope`
and `on_message_at` each go through `ModeRef`, and each call does its own
`factory()?.create()` (`crates/macp-modes/src/mode_registry.rs:682`, `:692`, `:706`) —
so an accepted message now builds three mode instances where it previously built two.

This is a pre-existing pattern, not something 11c introduced; 11c only made the count
one higher. Mode construction is cheap (no I/O, no allocation beyond the struct), so
this is not a live performance problem, and no benchmark shows it. But it is free to
fix: hoist one instance in `Runtime::process_message` and pass it to all three, or give
`ModeRef` an internal cached instance.

Deliberately **not** done inside G4 — it touches the hot path for a non-functional
reason, and G4's diff is already carrying a trait change plus a one-way door. Do it as
its own commit with its own before/after measurement, or not at all.

## 18. Unpinned structural guarantees around synthesis (Phase 11e NITs)

The Phase 11e verifier found four things that are true by construction but asserted
nowhere. None is a defect; each is a tripwire we do not have.

1. **No test pins that a duplicate, TTL-expired, or unauthorized trigger synthesizes
   nothing.** Guaranteed by where `synthesize_due_accept` sits in `process_message`
   (after every precheck and after the 11c client boundary), so a reordering — not a
   logic change — is what would break it. Criterion 6's squatter test does not cover
   this: its squat is pre-deadline, so nothing is due at that point anyway.
2. **The live/replay `participant_message_counts` divergence** that
   `synthesize_due_accept`'s rustdoc argues for (no `record_participant_activity`,
   because replay never records activity) is **not** caught by `assert_replay_matches`,
   which compares only state, resolution, `mode_state` and `seen_message_ids`. Its single
   guard is the one mutation test that adds `step::commit`.
3. **Observation time is now unrecoverable from the log** — `received_at_ms` carries the
   deadline D, and only a `tracing::info!` records when the runtime actually noticed.
   Not a one-way door: `LogEntry` is an internal serde struct and can gain a defaulted
   field later if we ever want both clocks.
4. **`handoff_id` is client-supplied, unbounded, and now embedded verbatim into a
   permanent `message_id`.** No new exposure — it was already in the payload and in
   `mode_state` — but the blast surface moved into the id namespace.

Worth doing if someone touches this area again; not worth a commit on its own.

## 19. `integration_tests/` is covered by neither the `fmt` nor the `clippy` CI gate

**Found:** 2026-09-13, during Phase 11f of `plans/backlog-closeout-2026-09.md`.

`ci.yml:131` runs `cargo fmt --all -- --check` and `ci.yml:158` runs
`cargo clippy --all-targets -- -D warnings`, both from the repo root. `--all` and
`--all-targets` mean *the root workspace*, and the root manifest **excludes**
`integration_tests/` (it is a separate cargo workspace with its own `Cargo.lock`). So an entire
test crate — 23 tier-1 files, plus tiers 2 and 3 and the `macp_integration_tests` helper lib — is
formatted and linted by nobody.

This is not hypothetical: `cargo fmt --check` inside `integration_tests/` already reports drift in
`tests/tier1_protocol/test_policy_registry.rs:1016` and `tests/tier1_protocol/test_session_lifecycle.rs:206`
on `feat/handoff-implicit-accept-rev2`. Neither has ever reddened CI.

Deliberately **not** fixed in Phase 11f: that phase is tests-only and scoped to one new file, and
sweeping formatting drift across two unrelated files would have polluted a diff whose whole value
is being auditable. The fix is its own small PR:

1. `cargo fmt --manifest-path integration_tests/Cargo.toml --all -- --check` as a step in the
   `fmt` job (or the `integration` job, which already has the toolchain warm).
2. Same for clippy — note this one needs the runtime binary built first, so the `integration` job
   is the cheaper home.
3. Fix the two existing diffs in the same PR, since step 1 reds without it.

**Watch for:** the `integration` job is also where `integration_tests/Cargo.lock` is guarded by
`cargo metadata --locked` (`ci.yml:533`). Adding lint steps there must not reorder or shadow that
guard — it is the only thing standing between a stale lock and the misleading compile errors
documented in follow-on 8's history.
