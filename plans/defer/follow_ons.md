# Actionable Follow-ons (post-v0.5.0)

**Created:** 2026-07-05, at the close of the improvement-plan execution
(v0.5.0 released; all seven crates on crates.io). These items are **not
blocked** — they are scoped, ready work that deliberately did not gate the
release. Distinct from `README.md`'s hard-blocked table.

Ordered by value:

## 1. Handoff implicit-accept timer (runtime implementation)
Spec contract merged (RFC-0010 §5.1, spec PR #50); `macp-proto` 0.1.6 ships
`HandoffAcceptPayload.implicit`. Implement the synthetic accept: timing from
the offer's recorded acceptance time excluding suspended time, eager
sweep + lazy-before-commitment emission into accepted history,
runtime-emitted envelope (sender = target, `implicit: true`, deterministic
`message_id` `implicit-accept:<handoff_id>`), reject client-submitted
implicit accepts. Replaces the A6 interim in-commitment-handler check; gate
on a new `semantics_rev` (=2) per the established migration pattern so
rev≤1 histories replay under the interim semantics. Master plan §2.5.

## 2. `watch_sessions` initial-sync memory bound
Narrowed. The `list_sessions` half of this item **shipped**: `page_size`
capping + opaque `page_token` / `next_page_token` are implemented in
`server.rs` `list_sessions` (keyset cursor over session IDs, bounded by
`MACP_LIST_SESSIONS_{DEFAULT,MAX}_PAGE_SIZE`). The original "replace the
documented server-side cap" clause was **stale** and is dropped: `docs/API.md`
documented no cap and the handler applied none, so there was nothing to
replace.

Still open: the `watch_sessions` initial sync emits one `Created` event per
session in the registry with no bound, so a large registry produces an
unbounded burst. Note this is a **memory** bound, not a protocol change —
`WatchSessionsRequest` is empty in the proto, so there is nothing to paginate
there without an upstream proto field; any fix is a server-side emission
bound (chunking, or a documented cap with a reconcile-via-`ListSessions`
contract). Proto fields for `list_sessions` shipped in 0.1.6 (spec PR #51);
master plan §3.4.

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

## 12. Mode-state records are exhaustively constructible public API
Being resolved in G4 as part of the 0.8.0 release — see `DECISIONS.md` D7.
Recorded here so the general rule survives that one release: **any new field on
a `pub` mode-state record is a major semver break**, because these structs have
all-pub fields and (until D7) no `#[non_exhaustive]`. `release-plz.toml` sets
`semver_check = true`, so this blocks the release PR rather than failing
quietly, and the single `version_group` moves all seven crates together.

After D7 lands, the handoff and quorum records carry `#[non_exhaustive]` and
future fields are additive. `macp-storage`'s `PersistedSession` and the
remaining mode-state records were **not** audited as part of D7 — worth a sweep
with `cargo semver-checks check-release --workspace` before the next release
that adds persisted state anywhere.
