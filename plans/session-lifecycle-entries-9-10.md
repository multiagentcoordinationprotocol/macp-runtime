# Session lifecycle entries — follow-on items 9 & 10

**Plan written:** 2026-09-28. **Base corrected in Round 3:** the plan's stated
base `5e95c4a` is **not an ancestor of `main`** (it was a local commit squashed
into PR #203); the tree every citation in this file was verified against is
`8115e75` (`chore: release v0.8.4 (#202)`, workspace `0.8.4`,
`CURRENT_SEMANTICS_REV = 3`). Round 3 re-verified all citations at `8115e75` and
they hold — the line numbers did not shift — but `/implement` should diff
against `8115e75`, not `5e95c4a`, which it cannot check out.
**Scope:** `plans/defer/follow_ons.md` items 9 and 10.

---

## Context

The brief asked for an implementation plan covering two deferred items: item 9
(`SessionSuspend`/`SessionResume`/`SessionCancel` are emitted as
`EntryKind::Internal` rather than entering accepted history — framed as
"confirmed non-conformance", to be fixed behind a new `semantics_rev` gate) and
item 10 (`make_internal_entry`'s second clock read can drop a session at
startup).

**Reading the code and the normative spec inverted both premises.** Neither item
is in the state `follow_ons.md` describes. What follows is what is actually
true at HEAD, verified line by line.

### Item 10 already shipped

`make_internal_entry` already takes the clock as a parameter
(`src/runtime.rs:282-305`, `at_ms: i64` at `:287`, used for both
`received_at_ms` at `:291` and `timestamp_unix_ms` at `:299`), and its rustdoc
(`src/runtime.rs:266-281`) argues the fix in exactly the terms item 10 asks
for — "The clock is **injected, never read here**". All four call sites read
the clock once and pass it:

| Call site | Single clock read | Passed to entry | Passed to session mutation |
|---|---|---|---|
| `maybe_expire_session` | `src/runtime.rs:322` | `:329` | `:335` (no clock arg needed) |
| `cancel_session` | `src/runtime.rs:1027` | `:1038` | `:1046` (`cancel()` is clock-free) |
| `suspend_session` | `src/runtime.rs:1087` | `:1097` | `:1104` (`session.suspend(now_ms)`) |
| `resume_session` | `src/runtime.rs:1141` | `:1167` | `:1176` (`session.resume(now_ms)`) |

`git log -S "at_ms: i64," -- src/runtime.rs` dates this to `7c652b6`
(2026-09-13) — the same commit as item 1's handoff implicit-accept work, which
is precisely where item 10 said it "should land". It landed; the backlog entry
was never updated. The regression test exists too:
`tests/integration_mode_lifecycle.rs:556` (`suspend_resume_entries_share_the_session_mutation_clock`)
asserts `session.accumulated_suspended_ms == resumed_at - suspended_at` read
back off the two log entries, then replays and asserts agreement (`:634-641`).

**What that test is and is not** (Round 3, and the test says so itself at
`tests/integration_mode_lifecycle.rs:550-554`): it *pins the invariant*, it does
not differentially prove the old race — "before the fix it would only have
broken when a millisecond tick happened to land between the two `Utc::now()`
reads". Item 10's real guarantee is the **injected-clock signature**, which is
structural and checked by the compiler; the test guards against a future caller
re-reading the clock. Do not describe it as a differential regression test when
Phase 4 rewrites the backlog entry.

Item 10's `banked_ms` sub-item was addressed **on its own terms** — and a
separate defect it never named was left in place. Round 3 correction: item 10
(`plans/defer/follow_ons.md:219-224`) asked for one of two things — "either
consume it on replay or document it as informational" — and `7c652b6` took the
second, which is what item 10 authorized (the rustdoc at `src/runtime.rs:1142-1152`).
What is wrong is a question item 10 never asked: whether the *quantity* recorded
is the one the RFC defines for that field. It is not. See "The one real defect"
below. This distinction matters because Phase 4 rewrites item 10's entry, and
"its `banked_ms` half was done wrongly" would be an unfair reading of a backlog
item that got what it asked for.

### Item 9's premise is wrong, and implementing it would break conformance

Item 9 (dated 2026-09-11) rests on RFC-MACP-0001 §7.5, which says of
`SessionSuspend`/`SessionResume`: "they enter the accepted history so the
suspension timeline is part of the replayed record"
(`rfcs/RFC-MACP-0001-core.md:314` in the spec repo — read verbatim this
session). From that, item 9 infers they should be `EntryKind::Incoming`,
consume accepted ordinals, and reach `StreamSession` subscribers.

**RFC-MACP-0006 §3.2 forbids exactly that, by name.** Verbatim,
`rfcs/RFC-MACP-0006-transport-bindings.md`:

> `:117` — "Entries a runtime records for its own bookkeeping — the
> `SessionSuspend` / `SessionResume` annotations of RFC-MACP-0001 §7.5, TTL
> expiry, storage checkpoints, and any other internal log entry — **MUST NOT
> consume ordinals**. Client-visible ordinals are therefore contiguous."
>
> `:122` — "The envelopes delivered on a subscribe stream MUST be exactly those
> that consume ordinals. **A runtime MUST NOT deliver an internal annotation on
> this stream**: a client cannot distinguish it from an ordinal-consuming
> envelope, and would over-count."

**Round 3 precision on the scope of each clause** — cite them for what they
literally say, because Phase 1 asserts the invariant at two different stream
levels and only one of them is covered by `:122`'s literal words:

- **`:117` is unrestricted.** It governs *ordinals*, full stop, and names the
  two envelope types. It applies to live `StreamSession` and passive subscribe
  alike, and to the storage filter underneath both.
- **`:122` is textually scoped to "a subscribe stream"** — it sits inside the
  `#### Passive Session Subscription` subsection (`:98`-`:142`). The live
  `StreamSession` contract is `:85-90` plus `:94`, which require acceptance
  order and permit echoing accepted client envelopes, and do **not** contain an
  explicit prohibition on internal annotations. The live half of the invariant
  therefore follows *a fortiori* from `:117` plus `:120`'s counting argument
  ("a client can determine its position only by counting the **distinct
  accepted envelopes** it has been delivered"), not from `:122`'s literal text.
  This does not weaken the conclusion — the two paths share one `stream_bus`
  and one `get_incoming_after` in this runtime, and a live subscriber counts
  its resume position the same way — but a test doc comment that cites `:122`
  for the live-stream assertion is citing the analogous clause, not the
  governing one. Say so, or a reviewer who opens `:122` will find it scoped
  elsewhere and distrust the rest.

Three facts settle this and are not matters of taste:

1. **§3.2 names the two envelope types explicitly**, not by inference. It is
   not a general rule that happens to catch them.
2. **§3.2 is the newer and more specific text.**
   `git log -S "MUST NOT consume ordinals"` in the spec repo dates `:117` and
   `:122` to `f1489df` (2026-08-30, "rfcs: specify the subscribe sequence…",
   spec PR #79). RFC-0001 §7.5's "enter the accepted history" clause dates to
   `048739d` (2026-06-22). Item 9 was written 2026-09-11 against the older
   clause and did not consult §3.2.
3. **The two clauses are reconcilable, and the runtime already implements the
   reconciliation.** "Accepted history" (RFC-0001 §8.3:342 — the authoritative,
   durable, replayed record) and "the ordinal-consuming, client-deliverable
   accepted-envelope sequence" (RFC-0006 §3.2:116-118) are different sets.
   `EntryKind::Internal` is exactly "in the durable log, replayed, no ordinal,
   not published": written by `make_internal_entry` (`src/runtime.rs:295`),
   durably appended (`:1099-1103`, `:1169-1173`, `:1039-1043`), replayed by
   `replay_entry`'s `Internal` arm (`src/replay.rs:139-168`), excluded from
   ordinals by `get_incoming_after`'s `Incoming`-only filter
   (`crates/macp-storage/src/log_store.rs:128`), and never handed to
   `publish_accepted_envelope` (`src/runtime.rs:229-233`).

So **the runtime is conformant today, and it is conformant because of the design
item 9 proposes to undo.** Implementing item 9 would violate two verbatim
MUST NOTs, and would additionally break replay outright: routing these entries
into `replay_entry`'s `Incoming` arm calls
`mode.authorize_sender(session, &replay_env)` (`src/replay.rs:125`) with
`sender == "_runtime"` (`src/runtime.rs:292`), which every mode refuses —
the default participant check at `crates/macp-modes/src/mode/mod.rs:169-174`
and handoff's own at `crates/macp-modes/src/mode/handoff.rs:307` both return
`Forbidden`. `replay_session` would then `Err`, and `src/main.rs:386-392`
would log "failed to replay session; skipping" for every session that had ever
been suspended.

**And the need item 9 was implicitly serving is already met** (Round 3 — the
rejection owes the reader this, or item 9 returns). A client that must observe
suspension does not need these envelopes on `StreamSession`: `WatchSessions`
(RFC-MACP-0006 §3.8) is the lifecycle-observation RPC, and the runtime already
emits `Suspended` / `Resumed` / `Cancelled` on it from inside the same three
functions (`SessionLifecycleEvent::Resumed` at `src/runtime.rs:1183`, and the
sibling sends in `suspend_session` / `cancel_session`), alongside per-mode
counters (`src/metrics.rs:90`, `:96`, `:102`). So the suspension timeline is
observable on the plane the spec designed for it, *and* durable and replayed in
the log — without being counted as an accepted envelope by a client tracking
`after_sequence`. Item 9 would move an observation that already works onto the
one plane where `:117` and `:120` make it harmful.

**Item 9 is therefore rejected on the merits.** No `semantics_rev` gate is
needed; `CURRENT_SEMANTICS_REV` stays at `3`
(`crates/macp-core/src/session.rs:110` — note it is already 3, not the 2 the
brief assumed, bumped by `ab64d1b` for the multi_round `Contribute` tie-break).
See Open questions Q1 for the override path if this conclusion is rejected.

### What is actually worth doing

Four real gaps, all surfaced by the analysis above rather than by the backlog:

1. **The conformance invariant is pinned by nothing.** The only ordinal test is
   `crates/macp-storage/src/log_store.rs:189`, which uses a generic `Internal`
   entry, never a real suspend/resume/cancel cycle. **No test anywhere asserts
   that a `SessionSuspend`/`SessionResume`/`SessionCancel` envelope is absent
   from a `StreamSession` subscriber's feed or from passive-subscribe replay.**
   `src/server.rs:1945` is named `stream_session_emits_accepted_envelopes_only`
   but only reads one `SessionStart` off a fresh stream — the name overclaims.
   So a future agent doing what item 9 asks would turn the suite green while
   making the runtime non-conformant. That is the highest-value work in this
   plan.
2. **`SessionResumePayload.banked_ms` carries the wrong quantity.** Confirmed
   divergence — see below.
3. **`replay_entry`'s `_ => {}` arm (`src/replay.rs:167`) is silent.** Item 9 is
   right about this one sub-point, and it is independent of the entry-kind
   question: a runtime-internal `message_type` a binary does not recognise is
   dropped with no signal.
4. **The backlog record is wrong in two places**, and item 9 carries a
   "confirmed non-conformance" label that will send the next reader down the
   same path.

### The one real defect: `banked_ms`

Two RFCs define the quantity. **Round 3 attribution fix:** the block quote below
is verbatim from **RFC-MACP-0003 §2** only (`rfcs/RFC-MACP-0003-determinism.md:48`);
RFC-MACP-0001 §7.5 (`rfcs/RFC-MACP-0001-core.md:318`) states the *same* quantity
in different words, so "both verbatim" was wrong as written.

> RFC-MACP-0003 §2, `:48` (verbatim): "On suspend at time `t_s`, the runtime
> banks `banked = deadline − t_s`; on resume at time `t_r`, it sets
> `deadline = t_r + banked` (recorded in `SessionResumePayload.banked_ms`)."

> RFC-MACP-0001 §7.5, `:318` (verbatim): "On suspend, the runtime records the
> remaining time `banked = deadline − suspend_time`; on resume, it sets
> `deadline = resume_time + banked` (the resume's `banked_ms` records this value
> for replay)."

Both name the same quantity — `deadline − t_s` — and only RFC-0001 adds "for
replay" (which Q3 addresses).

`banked` is **the remaining TTL at suspend**. The runtime writes the
**suspended duration** instead:

```rust
// src/runtime.rs:1153-1156
let banked_before = session
    .suspended_at_ms
    .map(|at| (now_ms - at).max(0))       // t_r − t_s, not deadline − t_s
    .unwrap_or(0);
```

The *deadline arithmetic* is correct and unaffected —
`Session::resume` does `ttl_expiry += banked` with `banked = now_ms - suspended_at`
(`crates/macp-core/src/session.rs:287`, `:314`), and
`deadline_old + (t_r − t_s) ≡ t_r + (deadline_old − t_s)` algebraically. Only
the **recorded field** is wrong, and by a large margin: for a 60 s TTL suspended
5 s in for a 2 s pause, the spec wants `55_000` and the runtime writes `2_000`.

Exposure is near zero, which is what makes this cheap to fix correctly now:
`grep -rn banked_ms` over `src crates tests integration_tests benches` returns
**only** the write at `src/runtime.rs:1160` and its own comment — no reader
anywhere in the workspace — and zero hits across `docs/`, `README.md`,
`CLAUDE.md`. It is not deliverable to clients either, because the `SessionResume`
entry is `Internal` and so never published. The only path by which anyone sees
it is the documented offline audit of `log.jsonl` (`docs/deployment.md:140`).

The existing rustdoc at `src/runtime.rs:1142-1152` declares the field
"informational only" and argues against consuming it on replay. That argument
is sound and is retained (see Q3); it just does not license writing a
different quantity than the spec names.

### Constraints the plan must respect

- `EntryKind` (`crates/macp-storage/src/log_store.rs:4-9`) has **no serde
  attributes** — no `#[serde(other)]`. Adding a variant is an
  `enum_variant_added` semver major and makes every older binary skip the line
  (each backend catches the per-record error and continues:
  `crates/macp-storage/src/storage/file.rs:145-155`,
  `rocksdb.rs:211-221`, `redis_backend.rs:134-141`), silently shifting ordinals.
  **No phase here adds an `EntryKind` variant.**
- `LogEntry` is public, exported (`crates/macp-storage/src/lib.rs:10`,
  re-exported `src/lib.rs:47`), literal-constructed from outside the crate
  (`benches/replay_bench.rs:46`, `tests/replay_round_trip.rs:38`), and **not**
  `#[non_exhaustive]` — it was not in 0.8.0's sealing sweep. Adding a field to
  it is still a `constructible_struct_adds_field` major. **No phase here adds
  one.** `PersistedSession` *is* sealed (`crates/macp-storage/src/registry.rs:34-35`).
- `tests/conformance/` is vendored from the spec repo and byte-diffed by CI, so
  no fixture can be added there. Suspend/resume/cancel claims must live in
  `src/replay.rs`'s test module, `tests/`, or tier 1.
- `CHANGELOG.md` is generated by release-plz from conventional commits
  (`release-plz.toml`, `CLAUDE.md` "Releasing"). **No phase edits it**; the
  release note is the commit subject.
- `integration_tests/` is a separate cargo workspace with its own
  `Cargo.lock`, guarded by `cargo metadata --locked` in the `integration` CI
  job. No phase here changes a dependency, so no lock regeneration is needed.
- `integration_tests/` is covered by neither the `fmt` nor the `clippy` CI gate
  (follow-on 19). Phase 1 adds a tier-1 file; run
  `cargo fmt --manifest-path integration_tests/Cargo.toml --all` by hand.

---

## Phases

### Phase 1 — Pin the ordinal and stream-delivery conformance invariant

- **Status:** DONE (2026-09-29). Implemented as specified; independently verified
  PASS by a fresh Opus subagent (see `plans/session-lifecycle-entries-9-10-PROGRESS.md`
  for the full checkpoint). One correction to this phase's own criterion 6, found
  during verification — see **Round 4 correction** immediately below.

  **Round 4 correction (criterion 6's mutation is not uniform across the three
  levels — found during implementation, confirmed independently by the verifier).**
  Criterion 6 as written claims that changing `make_internal_entry`'s
  `entry_kind: EntryKind::Internal` (`src/runtime.rs:295`) to `EntryKind::Incoming`
  "reds at least one test at each of the three levels." That is true for level 1
  (ordinal accounting, `tests/integration_mode_lifecycle.rs`) and level 3 (passive
  subscribe, `integration_tests/tests/tier1_protocol/test_suspend_resume.rs`) — both
  independently reproduced red under exactly this one-line mutation. It is **not**
  true for level 2 (live `StreamSession`, `tests/stream_integration.rs`): that test
  did not red under this mutation, because live delivery is gated by a structurally
  separate mechanism — the three explicit `publish_accepted_envelope` call sites
  (`src/runtime.rs:622, 795, 943`, inside `process_session_start`,
  `synthesize_due_accept`, `process_message` only) — never consulted by, or wired
  to, `entry_kind` at all. `suspend_session`/`resume_session`/`cancel_session` never
  call `publish_accepted_envelope`, regardless of what `entry_kind` the entry they
  append carries, so flipping that one enum tag cannot exercise this code path.
  A second, level-2-specific mutation (temporarily adding an explicit
  `self.publish_accepted_envelope(...)` call inside `suspend_session`, mirroring
  what "implementing item 9" would actually require for live delivery) *does* red
  `stream_subscriber_never_sees_lifecycle_annotations` — confirmed
  (`["SessionStart","SessionSuspend","Proposal"]` vs the expected two-element
  sequence) and then reverted. So the invariant genuinely is tested and mutation-
  provable at all three levels; it just takes two different mutations, not one,
  because the runtime enforces the two ends of the invariant (no-ordinal,
  no-delivery) through two independent mechanisms. A future reader should not
  "fix" the level-2 test by trying to make the single `entry_kind` mutation red it
  — that would be chasing a false premise.
- **Risk:** complex — this phase establishes the foundation for every other
  phase and encodes a normative contract. Getting an assertion backwards would
  bless the non-conformance rather than forbid it, and the suite would look
  healthier than before.
- **Delivers:** executable assertions that a `SessionSuspend` / `SessionResume`
  / `SessionCancel` log entry (a) consumes no accepted ordinal, (b) never
  reaches a live `StreamSession` subscriber, and (c) never appears in
  passive-subscribe replay — each citing RFC-MACP-0006 §3.2:117 and :122. After
  this phase, implementing item 9 as written reds the suite.
- **Depends on:** nothing.
- **Repo:** `macp-runtime` (this repo).
- **Files:**
  - `tests/stream_integration.rs` — new tests (in-process subscriber feed).
  - `tests/integration_mode_lifecycle.rs` — new test (ordinal accounting across
    a suspend/resume cycle).
  - `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` — new test
    (the contract over the real gRPC boundary).
- **Approach.**

  Three assertions at three levels, because the invariant is enforced in three
  different places and a single-level test would leave two unguarded:

  1. **Ordinal accounting (`tests/integration_mode_lifecycle.rs`).** Drive a
     real session through `rt.process(SessionStart)` → accepted message →
     `rt.suspend_session` → `rt.resume_session` → accepted message, then call
     `rt.log_store.get_incoming_after(&sid, 0)` and assert the returned
     `(ordinal, entry)` pairs are exactly the client envelopes with contiguous
     1-based ordinals — i.e. the suspend/resume pair inserted *no* gap and
     *no* entry. Assert the raw log length exceeds the ordinal count by exactly
     2, so the test proves the entries were written and excluded, not merely
     absent. Build on the existing `make_runtime()`
     (`tests/integration_mode_lifecycle.rs:15`) and the `stamp_of`-style log
     inspection already in
     `suspend_resume_entries_share_the_session_mutation_clock` (`:589-603`).

     **`cancel_session` needs its own session and a different assertion shape** —
     do not append it to the suspend/resume session. `cancel_session` is terminal
     and runs `maybe_compact_log` (`src/runtime.rs:1048`), whose behavior forks
     on the backend:
     - On `MemoryBackend`, `replace_log` resolves to the trait's **default**
       impl, which returns `Err(Unsupported)`
       (`crates/macp-storage/src/storage/mod.rs:42-47`; `memory.rs` does not
       override it), so compaction fails and the `else` branch
       `force_insert_checkpoint` (`src/runtime.rs:1049`) **appends** a
       `Checkpoint` entry with `compacted_incoming_ordinals: 0` (`:1299`). A
       cancel therefore adds **two** non-ordinal entries, not one.
     - On `FileBackend`, compaction **succeeds** and
       `replace_session_log(sid, vec![checkpoint])` (`src/runtime.rs:1257-1259`)
       reduces the in-memory log to a single checkpoint whose
       `compacted_incoming_ordinals` is `N`. `get_incoming_after(&sid, 0)` then
       returns **`Err(N)`**, because `after_sequence (0) < base (N)`
       (`crates/macp-storage/src/log_store.rs:123-125`).

     So: run the cancel leg on `MemoryBackend`, capture
     `get_incoming_after(&sid, 0)` immediately *before* the cancel and again
     after, and assert the two ordinal lists are identical while the raw log grew
     by exactly 2. Comparing before against after is stronger than an absolute
     count here, and is immune to the force-inserted checkpoint.

     **Round 3 — mandatory mechanical correction: `LogEntry` does not implement
     `PartialEq`.** `crates/macp-storage/src/log_store.rs:11` derives only
     `Clone, Debug, serde::Serialize, serde::Deserialize`; `PartialEq` is on
     `EntryKind` (`:4`), not on `LogEntry`. So `assert_eq!` on the
     `Vec<(u64, LogEntry)>` that `get_incoming_after` returns — "the two ordinal
     lists are identical", "byte-identical" — **does not compile**. Every
     comparison in this phase must be on a projection. Use the shape the
     existing ordinal test already uses
     (`crates/macp-storage/src/log_store.rs:203-205`, e.g.
     `assert_eq!((all[0].0, all[0].1.message_id.as_str()), (1, "m0"));`): collect
     `Vec<(u64, String)>` of `(ordinal, entry.message_id)` and `assert_eq!`
     that. Do not "fix" this by deriving `PartialEq` on `LogEntry` — that is an
     API addition to an unsealed public struct in the middle of a tests-only
     phase, and the projection is the clearer assertion anyway (it names the
     envelopes by id rather than comparing opaque payload bytes).
  2. **Live publication (`tests/stream_integration.rs`).** Subscribe via
     `rt.subscribe_session_stream(&sid)` (`src/runtime.rs:176`) *before* the
     session starts, then run the same sequence, then drain the receiver with
     `try_recv()` until `Empty` and assert the collected
     `message_type`s contain no `SessionSuspend` / `SessionResume` /
     `SessionCancel` **and** equal the expected client-envelope sequence
     exactly. The positive half matters as much as the negative one: a test
     that only asserts absence also passes if publication broke entirely.
     Mirror the shape of `stream_subscribers_see_the_synthetic_envelope_in_order`
     (`tests/stream_integration.rs:213`), which is the closest existing
     analogue and the test that pins the *opposite* answer for the handoff
     synthetic accept — the contrast is the point and should be noted in the
     new test's doc comment.
  3. **Passive subscribe over gRPC
     (`integration_tests/tests/tier1_protocol/test_suspend_resume.rs`).** Open a
     session, send an accepted message, `SuspendSession`, `ResumeSession`, send
     another accepted message, then attach a passive-subscribe frame with
     `after_sequence: 0` and assert the replayed envelope sequence contains
     only the client envelopes, with the ordinal-implied count matching. Reuse
     `subscribe_frame` / `open_stream` / `next_envelope` — the canonical copies
     live at `integration_tests/tests/tier1_protocol/test_passive_subscribe.rs:24`,
     `:42`, `:63`; the handoff file keeps local copies at
     `test_handoff_implicit_accept.rs:199`, `:207`, `:227`. This is the level
     that would catch a regression in `server.rs`'s
     `process_subscribe_frame` → `get_session_envelopes_after` path
     (`src/server.rs:512-521` → `src/runtime.rs:198-227`) rather than only in
     the storage filter.

  **Why tests-only and why first.** The invariant is normative, currently
  unasserted, and the thing most likely to be broken by a well-intentioned
  reading of the backlog. Landing the tripwire before touching any behavior
  means Phase 2's change lands on a suite that already asserts what Phase 2
  must not disturb.

  **Rejected:** asserting via `EntryKind` equality alone. `entry_kind ==
  Internal` is the *mechanism*; ordinals and delivery are the *contract*. A test
  keyed on the mechanism would pass under a refactor that preserved the kind but
  broke the filter, and would red under a legitimate internal rename. Assert the
  observable contract.

  **Rejected:** a `tests/conformance/` fixture. The directory is vendored and
  byte-diffed by CI, and its transcript format carries only client messages —
  there is no way to express a runtime-originated entry in it.
- **Edge cases & failure modes.**
  - *Vacuous-pass risk.* Every negative assertion is paired with a positive one
    (exact expected sequence, exact ordinal count, and the raw-log-minus-ordinals
    delta), so a test cannot pass by the feature having disappeared.
  - *Broadcast lag.* `SessionStreamBus` uses a 256-capacity
    `tokio::sync::broadcast` (`src/stream_bus.rs:6`). These tests publish
    single-digit envelopes, so `Lagged` is unreachable; still, drain with
    `try_recv()` and treat `Lagged` as a test failure rather than `Empty`, so a
    future capacity change surfaces as a failure and not a false pass.
  - *Subscribe-before-publish ordering.* `SessionStreamBus::publish`
    (`src/stream_bus.rs:38-46`) is a no-op when no channel exists, so the
    subscriber must be created before the first `process` call. The existing
    `stream_receives_accepted_envelopes` (`tests/stream_integration.rs:57`)
    already establishes that ordering; follow it.
  - *Compaction interference on the cancel leg.* Fully specified in the Approach
    above — `MemoryBackend` appends a force-inserted `Checkpoint` (so the log
    grows by 2), while `FileBackend` collapses the log and makes
    `get_incoming_after(.., 0)` return `Err(N)`. This backend fork is exactly why
    `tests/handoff_implicit_accept_live.rs` keeps two harnesses (`:56` vs `:62`)
    and the `assert_log_is_uncompacted` guard (`:95`). Take the same precaution
    and say so in the test's comment, so the next reader does not "simplify" the
    test onto a durable backend.
  - *The force-inserted checkpoint is itself non-ordinal.* `force_insert_checkpoint`
    writes `compacted_incoming_ordinals: 0` (`src/runtime.rs:1299`) and
    `get_incoming_after`'s base is a `.max()` over checkpoints
    (`crates/macp-storage/src/log_store.rs:117-122`), so it cannot reset a base
    established by an earlier real compaction. The cancel-leg assertion must
    therefore hold, not merely happen to.
  - *Tier-1 parallelism.* `integration_tests` tier 1 runs with
    `--test-threads=1` and one shared `ServerManager` per binary, constructed at
    `integration_tests/tests/common/mod.rs:36`. **Round 3 citation fix:**
    `MACP_MEMORY_ONLY=1` is **not** set in `common/mod.rs` (that file sets no env
    vars at all; `:49` is `grpc_client`) — it is the `ServerManager` default, at
    `integration_tests/src/server_manager.rs:69`, inside `start_with_env`
    (`:60`), which `start()` (`:55-57`) delegates to with an empty `extra_env`.
    `test_persistence_replay.rs:22` and `test_backends.rs:28` are the tests that
    override it to `0`. Use a
    fresh session id (`new_session_id()`,
    `integration_tests/src/helpers.rs:22`) and do not assume an empty registry.
  - *No shared suspend/resume helper exists* in `integration_tests/src/helpers.rs`
    (there is `cancel_session_as` at `:154` but no suspend/resume equivalent);
    the existing tier-1 tests build the requests inline
    (`test_suspend_resume.rs:9`). Follow the local pattern rather than adding a
    shared helper in a tests-only phase.
- **Acceptance criteria.**
  1. A new test in `tests/integration_mode_lifecycle.rs` drives
     SessionStart → accepted message → suspend → resume → accepted message and
     asserts `get_incoming_after(&sid, 0)`, **projected to
     `Vec<(u64, String)>` of `(ordinal, message_id)`** (see the Round 3
     correction in Approach — `LogEntry` has no `PartialEq`), equals exactly the
     client envelopes' ids with ordinals `1..=N` contiguous, **and** that the raw
     log is exactly 2 entries longer than `N`.
  2. A sibling test on `MemoryBackend` asserts the equivalent for
     `cancel_session` on its **own** session: the `(ordinal, message_id)`
     projection of `get_incoming_after(&sid, 0)` is identical before and after
     the cancel, while the raw log grows by exactly 2 (the `SessionCancel` entry
     plus the force-inserted `Checkpoint`). The test's comment must state why the
     backend choice is load-bearing.
  2a. **(Round 3, added.)** The suspend/resume test re-derives the projection
     across a **simulated restart**: build a fresh `LogStore`, replay the
     collected entries into it the way startup does
     (`create_session_log` then `append` per entry — `src/main.rs:371-374`), and
     assert `get_incoming_after(&sid, 0)`'s `(ordinal, message_id)` projection is
     identical to the pre-restart one. Note what this must *not* be: asserting
     the projection is unchanged after calling `replay_session` on the same
     `LogStore` is **vacuous**, because `replay_session` rebuilds a `Session` and
     never touches the log. The fresh-store round trip is the real assertion, and
     it pins RFC-MACP-0006 §3.2:123 ("An ordinal MUST be stable for the life of
     the session — across runtime restarts…") for exactly the entry kinds this
     plan is about, in about five lines. It is the assertion that would catch a
     future change giving `Internal` entries ordinal-bearing position on the
     *read* side rather than the write side.
  3. A new test in `tests/stream_integration.rs` asserts a subscriber attached
     before `SessionStart` receives exactly the client envelopes, in order, and
     that none of the three lifecycle `message_type`s appears.
  4. A new test in
     `integration_tests/tests/tier1_protocol/test_suspend_resume.rs` asserts the
     same over passive subscribe with `after_sequence: 0`.
  5. Each new test's doc comment cites `RFC-MACP-0006 §3.2` as the source of the
     invariant, **with the per-clause scoping Round 3 added to Context**: the
     ordinal assertions cite `:117` (unrestricted); the passive-subscribe
     assertion cites `:122` (whose MUST NOT is textually scoped to "a subscribe
     stream"); the live-`StreamSession` assertion cites `:117` plus `:120`'s
     counting argument and names `:122` as the analogous clause rather than the
     governing one. A reviewer who opens `:122` must not find it scoped to a
     different subsection than the test claims.
  6. **Mutation-proven:** changing `make_internal_entry`
     (`src/runtime.rs:295`) to `EntryKind::Incoming` reds at least one test at
     each of the three levels. Record the observed failures in the phase's
     commit body. This criterion is the phase's whole point and must be
     demonstrated, not asserted.
  7. `cargo test --workspace` green; tier 1 green; `cargo clippy --workspace
     --all-targets -- -D warnings` clean; `cargo fmt --all -- --check` and
     `cargo fmt --manifest-path integration_tests/Cargo.toml --all -- --check`
     clean.
- **Tests.** The four above are the deliverable. Failure paths covered: the
  mutation in criterion 6 is the failure path, run in both directions
  (`Incoming` → red, restored → green).
- **Docs.** `docs/testing.md:68` enumerates what tier 1 covers ("session
  suspend/resume … `StreamSession` including RFC-MACP-0006-A1 passive
  subscribe"); extend that sentence to name the ordinal/delivery contract.
  Nothing else in `docs/` becomes stale — this phase changes no behavior.

---

### Phase 2 — Correct `SessionResumePayload.banked_ms` to the normative quantity

- **Status:** DONE (2026-09-29)
- **Risk:** complex — it changes the value of a field written permanently into
  append-only history. Logs already on disk keep the old quantity forever, so
  the field becomes generation-dependent. That is a one-way door for
  already-written logs (already wrong) and for logs written after this lands.
- **Delivers:** `SessionResumePayload.banked_ms` carries `deadline − t_s` (the
  remaining TTL banked at suspend), as RFC-MACP-0001 §7.5:318 and
  RFC-MACP-0003 §2:48 require, instead of `t_r − t_s`.
- **Depends on:** nothing technically. **Round 3 honesty correction:** the draft
  said "Phase 1", but Phase 2 touches one arithmetic expression in
  `resume_session` and no Phase 1 test reads `banked_ms`, so Phase 2 compiles,
  tests and ships on its own. What Phase 1 buys is *review confidence* — landing
  the tripwire first is what lets a reviewer conclude the change is confined to
  the payload field rather than take the author's word for it. That is a
  sequencing preference and a PR-packaging choice, stated as such, exactly as
  Phase 3's `Depends on` already is. If Phase 1 slips, Phase 2 is not blocked.
- **Repo:** `macp-runtime`.
- **Files:**
  - `src/runtime.rs` — `resume_session`, the `banked_before` expression
    (`:1153-1156`) and the rustdoc above it (`:1142-1152`).
  - `docs/API.md` — the `ResumeSession` section (`:204-212`).
- **Approach.**

  Replace the expression with the spec's quantity, read from state the function
  already holds and **before** `session.resume(now_ms)` mutates `ttl_expiry`:

  ```rust
  // RFC-MACP-0001 §7.5 / RFC-MACP-0003 §2: `banked_ms` records the remaining
  // TTL at suspend (`deadline − t_s`), not the pause's duration.
  let banked_ms = session
      .suspended_at_ms
      .map(|t_s| session.ttl_expiry.saturating_sub(t_s).max(0))
      .unwrap_or(0);
  ```

  `Session::suspend` does not touch `ttl_expiry`
  (`crates/macp-core/src/session.rs:250-257` sets only `state` and
  `suspended_at_ms`), so `session.ttl_expiry` at this point is still the
  pre-suspension deadline — exactly the spec's `deadline`. `now_ms` is no longer
  an input to the field, which is why this must stay ordered before the
  `session.resume(now_ms)` call at `:1176`.

  **The deadline arithmetic is deliberately not touched.** `Session::resume`
  keeps `ttl_expiry += (now_ms − suspended_at)`
  (`crates/macp-core/src/session.rs:287`, `:314`), which is algebraically
  identical to the spec's `deadline = t_r + banked` and is what replay
  re-derives from the two entry timestamps. Rewriting it to the spec's literal
  form would change nothing observable while touching the single most
  replay-sensitive method in the codebase. Rejected.

  **Replay still ignores the field** — see Q3. The spec's own determinism
  argument rests on the event timestamps, not on `banked_ms`
  (RFC-MACP-0003 §2:48: "every input to this computation (the recorded cap and
  the suspend/resume event timestamps) is on the replayed timeline"), so
  re-deriving is conformant, and consuming the field would make legacy logs —
  which recorded the wrong quantity — replay to a different deadline. The
  existing rustdoc's conclusion is kept; only its premise about *which* value is
  written changes.

  **No `semantics_rev` gate.** A gate exists to keep replay byte-identical for
  sessions accepted under older semantics. Nothing reads this field on any
  path, at any revision, so no replay outcome depends on it and there is
  nothing for a gate to protect. Gating it would add a permanent branch and a
  revision bump for zero behavioral difference. (Contrast rev 2 and rev 3, both
  of which change an accept/reject or decode outcome —
  `crates/macp-core/src/session.rs:77-109`.)

  **Commit subject:** `fix(runtime): record banked_ms as the remaining TTL at
  suspend`. Not `!`-marked: the field has no reader in this workspace, is
  absent from `docs/`, `README.md` and `CLAUDE.md`, and is not deliverable to
  clients (the `SessionResume` entry is `Internal`, so it reaches neither
  `StreamSession` nor passive subscribe). A `!` would advertise a break to
  dependents where none is reachable through any documented surface. The commit
  body must still name `log.jsonl` (`docs/deployment.md:140`) as the one path by
  which an operator could have observed the old value.
- **Edge cases & failure modes.**
  - *`suspended_at_ms` is `None`.* Unreachable: `resume_session` returns early
    unless `state == Suspended` (`src/runtime.rs:1137-1139`), and only
    `Session::suspend` sets that state, always alongside
    `suspended_at_ms = Some(now_ms)` (`crates/macp-core/src/session.rs:254-255`).
    Keep the `.unwrap_or(0)` rather than panicking — the existing code does, and
    `SessionBuilder` lets a library consumer construct an inconsistent session.
  - *`t_s > ttl_expiry` → negative.* Guarded by `.max(0)`. Also nearly
    unreachable: `suspend_session` runs `maybe_expire_session` first
    (`src/runtime.rs:1082`), which expires any Open session past its deadline
    (`:325`), so `t_s <= ttl_expiry` holds at suspend. The clamp costs nothing
    and covers the builder-constructed case.
  - *Overflow.* `saturating_sub` — `ttl_expiry` defaults to `i64::MAX` for
    builder-constructed sessions (`crates/macp-core/src/session.rs:210`), so a
    naive subtraction is a real overflow risk, not a theoretical one.
  - *Multiple suspend/resume cycles.* Each resume records the remaining TTL as
    of *its own* suspend, matching the spec's per-event formula. The value is
    therefore non-monotonic across cycles by design (it shrinks as the session
    ages) — assert this explicitly so a later reader does not "fix" it.

    **Round 3 — the shrink is `<=`, not `<`, at millisecond resolution, so
    criterion 5's test must advance the clock.** Deriving it:
    `banked_n = deadline_n − t_sn` and
    `deadline_n = deadline_{n-1} + (t_r(n-1) − t_s(n-1))`, so
    `banked_n = banked_{n-1} + (t_r(n-1) − t_sn)`. Because a second suspend
    cannot precede the first resume, `t_sn >= t_r(n-1)`, giving
    `banked_n <= banked_{n-1}` — strict only when `t_sn > t_r(n-1)`. A test that
    resumes and immediately re-suspends within the same millisecond records two
    **equal** values and a strict `<` assertion flakes. Sleep past a millisecond
    boundary between the first resume and the second suspend (the existing
    fixture already does exactly this at
    `tests/integration_mode_lifecycle.rs:583`), and say in the test comment that
    the clock advance is load-bearing rather than cosmetic.
  - *A resume that force-expires still records `banked_ms`.* The payload is built
    and the entry appended (`src/runtime.rs:1157-1173`) **before**
    `session.resume(now_ms)` is called (`:1176`), and the cap-exceeded arm
    (`:1191-1219`) appends nothing further. So a `SessionResume` entry exists
    carrying a "remaining TTL" that is never applied, because the session went
    `Expired`. This is pre-existing shape, not something Phase 2 introduces — the
    old formula recorded on that path too — and the ordering must not be
    changed (PROGRESS: replay re-derives the force-expiry by re-running
    `session.resume(at)` off the recorded entry). Note it in the rustdoc so the
    value is not read as "the TTL this session went on to have".
  - *Zero.* A session suspended exactly at its deadline records `0`. Legal and
    distinguishable from "field absent" only by context; note it in the doc.
  - *Log-generation ambiguity.* Logs written before this phase carry `t_r − t_s`
    under the same field name, with no discriminator. This is accepted, not
    mitigated: adding a discriminator would mean a new `LogEntry` field, which
    is a semver major (see Constraints), for a field nothing reads. The
    rustdoc must state that entries written before this change carry the pause
    duration, so anyone who later wants to consume the field knows it is not safe
    to.

    **Round 3 — do not hardcode `0.8.5` in the rustdoc.** The draft said
    "pre-0.8.5 entries". `0.8.4` is released (`8115e75`, tag
    `macp-runtime-v0.8.4`), so `0.8.5` is the *likely* next version — but
    release-plz computes it from the conventional-commit history at release
    time, and CLAUDE.md's own opening warns against trusting a version written
    into prose. Anything landing alongside this with a `!` makes it `0.9.0` and
    the rustdoc ships a lie about its own release. Write the boundary as the
    change, not the number — "entries written before the commit that introduced
    this expression (`git log -S banked_before -- src/runtime.rs`)" — or leave a
    `<!-- fill at release -->` marker and fill it from the root `Cargo.toml`
    once the release PR exists. The same applies to the "0.8.5 boundary" phrase
    in Long-term posture.
- **Acceptance criteria.**
  1. `src/runtime.rs`'s `resume_session` computes `banked_ms` from
     `session.ttl_expiry` and `session.suspended_at_ms`, and **not** from
     `now_ms`. A reviewer can confirm by checking that `now_ms` does not appear
     in the expression.
  2. The expression is ordered before `session.resume(now_ms)`.
  3. A test decodes `SessionResumePayload` out of the `SessionResume` log entry
     and asserts `banked_ms == ttl_expiry_before_resume − suspend_entry.received_at_ms`,
     with a fixture where that value is distinguishable from the pause duration
     by more than an order of magnitude.
  4. A test asserts the deadline is unchanged by this phase: after
     suspend/resume, `session.ttl_expiry == original_ttl_expiry + (resume_at − suspend_at)`.
  5. A test asserts `banked_ms` shrinks across two cycles on the same session
     (pinning the per-event semantics), **with a clock advance between the first
     resume and the second suspend** so the comparison is strict rather than a
     tie — see the Round 3 derivation in edge cases (`banked_n <= banked_{n-1}`,
     strict only when `t_sn > t_r(n-1)`).
  6. `tests/integration_mode_lifecycle.rs:556` and every Phase 1 test still
     pass unmodified — the change is confined to the payload field.
  7. The rustdoc at `src/runtime.rs:1142-1152` is updated to state the new
     quantity, cite RFC-MACP-0001 §7.5 and RFC-MACP-0003 §2, retain the
     do-not-consume-on-replay argument, and warn that entries written before this
     change carry the pause duration — expressed **without a hardcoded version
     number** (Round 3; see edge cases). It must also note that a resume which
     force-expires the session still records the field.
  8. `docs/API.md`'s `ResumeSession` section documents the field and its
     quantity. (It currently mentions banking in prose at `:206` and names no
     field; `banked_ms` appears nowhere in `docs/`.)
- **Tests.** Criteria 3-5 are new tests in `tests/integration_mode_lifecycle.rs`
  (alongside the existing clock test, which already builds the exact fixture
  needed). Failure paths: a fixture with `suspend_at == ttl_expiry` asserting
  `banked_ms == 0`; and a `Session::builder`-constructed session with
  `suspended_at_ms` set past `ttl_expiry` asserting `0` rather than a negative
  value or a panic.
- **Docs.** `docs/API.md:204-212` (`ResumeSession`) — add the field and its
  definition. `docs/deployment.md:140` describes the `log.jsonl` audit path; add
  a one-line note that `banked_ms` semantics changed in this release, since that
  is the only surface where the old value was observable.

---

### Phase 3 — Make the replay reader loud about unrecognized runtime entries

- **Status:** DONE (2026-09-29). Implemented as specified; independently
  verified PASS (fresh Opus subagent, round 1, no gaps) — see
  `plans/session-lifecycle-entries-9-10-PROGRESS.md` for the checkpoint.
- **Risk:** simple — one `tracing::warn!` in place of a silent arm, plus a test.
  Reversible in a commit; changes no state, no acceptance decision, no wire
  behavior.
- **Delivers:** `replay_entry`'s catch-all `Internal` arm emits a warning naming
  the unrecognized `message_type` instead of dropping it silently. This is the
  one sub-point of item 9 that survives its premise being wrong.
- **Depends on:** nothing. (Sequenced after Phase 1 only for PR packaging; it
  is independently shippable and verifiable.)
- **Repo:** `macp-runtime`.
- **Files:**
  - `src/replay.rs` — the `_ => {}` arm at `:167`, and a new test in `mod tests`.
- **Approach.**

  Replace `_ => {}` with a `tracing::warn!` carrying `session_id`,
  `message_type` and `received_at_ms`. Deliberately **warn, not error**: the log
  is authoritative and a replay failure drops the session from the registry
  entirely (`src/main.rs:386-392`) or aborts startup under
  `MACP_STRICT_RECOVERY=1` (`:379-385`). Turning an unrecognized bookkeeping
  annotation into a hard failure would convert a forward-compatibility gap into
  an outage, which is the same reasoning `validate_replay_consistency` is
  warn-only for (`src/replay.rs:188-190`).

  Note honestly what this does and does not buy. It cannot help a binary that
  has already shipped — the warning lives in the *reader*, so the benefit
  accrues to the next time a new runtime-internal `message_type` is introduced,
  at which point the then-current binaries are loud. That is worth two lines.

  This is *not* in tension with the "loud stale reader" posture documented at
  `crates/macp-modes/src/mode/mod.rs:80-82`, which argues a stale reader should
  fail visibly rather than skip silently. That argument is about *accepted*
  entries that carry mode state; this arm handles runtime bookkeeping
  annotations, where the state effect is the runtime's own and a missing arm
  means a state transition was not applied. A warning makes that visible
  without making it fatal. Say so in the code comment so the two are not read
  as contradictory.

  **Rejected:** returning `Err` for unrecognized internal types. See above —
  the blast radius is a silently vanished session.

  **Rejected:** a metrics counter. `RuntimeMetrics` has no recovery-path
  counters other than `record_replay_mismatch` (`src/main.rs:418`), and this
  condition is a startup-time, once-per-entry event that the structured log
  already surfaces with full context. Adding a counter for it would be the only
  metric in the replay path with no aggregation story.
- **Edge cases & failure modes.**
  - *Log volume.* A log with many unrecognized entries would emit one warning
    each at startup. Bounded by the log length, which is already fully iterated,
    and only reachable on a version downgrade. Acceptable; do not add rate
    limiting for a once-per-process path.
  - *`Checkpoint` entries are a separate arm* (`src/replay.rs:169-171`) and stay
    a silent no-op — skipping an intermediate checkpoint is correct and
    expected, not an anomaly.
  - *The four known types must not warn.* `TtlExpired`, `SessionCancel`,
    `SessionSuspend`, `SessionResume` (`src/replay.rs:140`, `:145`, `:151`,
    `:159`) keep their arms; the test must assert they produce no warning, or at
    minimum that they still apply their state transition (which the existing
    `replay_handles_ttl_expired` at `:538` and `replay_handles_session_cancel`
    at `:556` already do).
- **Acceptance criteria.**
  1. `src/replay.rs`'s `Internal` catch-all logs at `warn` level with
     `session_id`, `message_type` and `received_at_ms` as structured fields.
  2. A test builds a log containing an `Internal` entry with an invented
     `message_type` and asserts `replay_session` still returns `Ok` and the
     session is otherwise correctly rebuilt — i.e. the change is observability
     only, not a behavior change.
  3. `replay_handles_ttl_expired` (`src/replay.rs:538`) and
     `replay_handles_session_cancel` (`:556`) pass unmodified.
  4. The code comment explains why this is `warn` and not `Err`, and reconciles
     it with the loud-stale-reader note at
     `crates/macp-modes/src/mode/mod.rs:80-82`.
  5. `cargo clippy --workspace --all-targets -- -D warnings` clean.
- **Tests.** Criterion 2's test, built on the existing `internal_entry(message_type,
  received_at_ms)` fixture (`src/replay.rs:448`) — it takes the message type as
  a parameter, so an unrecognized one needs no new fixture.
- **Docs.** None. This surfaces an existing condition; it documents no new
  contract. (`docs/deployment.md`'s crash-recovery section describes
  replay failure modes but not per-entry warnings, and adding one warning to
  that list would overstate its significance.)

---

### Phase 4 — Settle the record: rustdoc, docs, backlog, upstream issues

- **Status:** DONE (2026-09-29). Upstream issues filed as **two**, folding ask
  #3 into ask #1 as the plan's own approach permitted:
  [multiagentcoordinationprotocol/multiagentcoordinationprotocol#159](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/159)
  (asks #1 and #3 — `SessionCancel`'s classification plus the missing
  §7.5/§7.3 → §3.2 cross-reference) and
  [multiagentcoordinationprotocol/multiagentcoordinationprotocol#160](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/160)
  (ask #2 — RFC-MACP-0010 §5.1(2)'s construction analogy and its wrong §7.5
  anchor for `SessionCancel`).
- **Risk:** simple — documentation, backlog, and upstream GitHub issues (three
  after Round 3, or two if #3 is folded into #1). No
  code path changes.
- **Delivers:** the conformance argument is recorded where the next reader will
  hit it (at the emission sites, in `docs/API.md`, and in the backlog), items 9
  and 10 are closed with evidence, and the genuine spec gaps this analysis found
  are filed upstream (three asks — Round 3 added the §7.5:314 cross-reference).
- **Depends on:** Phase 1 (the tests it cites must exist) and Phase 2 (the
  `banked_ms` quantity it documents must be the corrected one).
- **Repo:** `macp-runtime`, plus issues filed in the spec repo
  (`multiagentcoordinationprotocol/multiagentcoordinationprotocol`). Filing an
  issue in another repo is not a cross-repo write and needs no gate; **no file
  in the spec repo is edited.**
- **Files:**
  - `src/runtime.rs` — rustdoc on `make_internal_entry` (`:266-281`) and a
    short note at each of the three emission sites (`:1032`, `:1092`, `:1162`).
  - `src/replay.rs` — a note on the `Internal` arm (`:139`).
  - `crates/macp-storage/src/log_store.rs` — the ordinal contract doc
    (`:90-102`) already cites RFC-MACP-0006 §3.2 correctly; add the two
    envelope-type names so the link between the code and §3.2:117 is explicit.
  - `docs/API.md` — `CancelSession` (`:180-190`), `SuspendSession` (`:192-202`),
    `ResumeSession` (`:204-212`), and the `StreamSession` contract (`:82-92`).
  - `plans/defer/follow_ons.md` — rewrite items 9 and 10.
- **Approach.**

  **The rustdoc is the load-bearing deliverable.** Item 9 was written by a
  careful reader who had RFC-MACP-0001 §7.5 in hand and no pointer to
  RFC-MACP-0006 §3.2. The fix for that class of error is a citation at the
  point of confusion — `make_internal_entry` — stating: these entries are in
  the durable, replayed log; they consume no accepted ordinal and are not
  published, *because* RFC-MACP-0006 §3.2:117 names them as bookkeeping and
  :122 forbids delivering them; and this is deliberately the opposite treatment
  from the handoff synthetic accept (`synthesize_due_accept`,
  `src/runtime.rs:721`), which is a mode message with a spec-pinned `sender`
  (RFC-MACP-0010 §5.1(3)) and therefore does consume an ordinal and does
  publish. Cross-link the two so a reader landing on either sees the contrast.

  **`docs/API.md`.** Four edits. The three RPC sections currently say almost
  nothing about the envelopes: `CancelSession` (`:190`) says the runtime "writes
  a `SessionCancelPayload` to the log", and `SuspendSession` / `ResumeSession`
  mention no envelope at all. State for each: the runtime is the sole emitter,
  clients must not submit it via `Send`, it enters the durable replayed history,
  and it consumes no accepted ordinal and is not delivered on a subscribe
  stream. The `StreamSession` section (`:82-92`) says subscribers receive "all
  accepted envelopes"; sharpen it to the ordinal-consuming set, which is what
  RFC-MACP-0006 §3.2:122 actually requires and what the code does.

  **`plans/defer/follow_ons.md`.** Item 10 → **DONE**, citing `7c652b6`, the
  four call sites, the existing test at
  `tests/integration_mode_lifecycle.rs:556`, and Phase 2 for the `banked_ms`
  half. Item 9 → **NOT A DEFECT — closed**, following the file's own convention
  for reversed items (item 2's "The original clause was **stale** and was
  dropped", item 12's "The audit … found it was not a sibling at all"). The
  rewrite must carry: the two verbatim §3.2 citations; the dates showing §3.2
  postdates the §7.5 clause item 9 relied on; the `authorize_sender` /
  `_runtime` mechanical consequence; and a pointer to Phase 1's tests as the
  thing that now stops the change from being made silently. Keep the surviving
  sub-point (the silent `_ => {}` arm) as closed by Phase 3.

  **Upstream issues (three after Round 3, filed with `gh issue create -R
  multiagentcoordinationprotocol/multiagentcoordinationprotocol` — Round 3
  confirmed that is the spec repo's actual `origin`).**
  1. *`SessionCancel` is not classified for ordinals or stream delivery.*
     RFC-MACP-0006 §3.2:117 enumerates `SessionSuspend` / `SessionResume` / TTL
     expiry / checkpoints but **not** `SessionCancel`, which is reachable only
     through the trailing "and any other internal log entry".
     RFC-MACP-0001 §7.3:269 does call it a "terminal annotation", which matches
     §3.2:122's "internal annotation", so this runtime reads it as
     non-ordinal-consuming and undeliverable — but the reading is inferential
     where the other two are explicit. Ask for `SessionCancel` to be named in
     §3.2:117. Include this runtime's reading and its reasoning so the issue
     carries a recommendation, not a bare question.
  2. *RFC-MACP-0010 §5.1(2)'s analogy invites the exact misreading that produced
     item 9, and its anchor is wrong.* The clause
     (`rfcs/RFC-MACP-0010-handoff-mode.md:95-99`) describes the synthetic accept
     as "the same construction as runtime-emitted `SessionSuspend`/
     `SessionResume`/`SessionCancel` envelopes (RFC-MACP-0001 §7.5)". Read in
     context the analogy is scoped to one property — the timer is outside the
     replay boundary, its recorded product is inside — but "the same
     construction" reads as entry-classification equivalence, which §3.2:117 and
     :122 contradict for two of the three named types. Propose narrowing the
     wording to the replay-boundary property. Also note the anchor: §7.5 covers
     suspend/resume only; `SessionCancel` is §7.3:269. The spec repo's own
     `plans/spec-followups-100-107.md:733-735` already flagged this anchor for
     verification and it was not acted on. (Round 3 verified `:733-735` verbatim:
     "Note it says **`§7.5`** for `SessionCancel` — `RFC-MACP-0001-core.md:254`
     is the runtime-sole-emitter rule. Verify that anchor at execution time…")
  3. **(Round 3, added.) RFC-MACP-0001 §7.5:314's bare "they enter the accepted
     history" is the clause that actually produced item 9, and it carries no
     cross-reference to §3.2.** This is the highest-value upstream fix of the
     three and the draft omitted it. Phase 4's whole stated method is "the fix
     for that class of error is a citation at the point of confusion" — the plan
     applies that to this runtime's rustdoc and to RFC-0010's analogy, but not to
     the sentence a careful reader lands on first. §7.5:314 says suspend/resume
     "enter the accepted history so the suspension timeline is part of the
     replayed record" and stops there; a reader with no pointer to §3.2:117
     reasonably concludes they are ordinal-consuming accepted envelopes, which is
     exactly the inference item 9 made on 2026-09-11. Propose adding a
     parenthetical — "(as internal annotations: they consume no ordinal and are
     not delivered on a subscribe stream, RFC-MACP-0006 §3.2)" — and the same at
     §7.3:269 for `SessionCancel`. Note in the issue that the two clauses are
     already reconcilable and that this runtime implements the reconciliation, so
     the ask is clarifying prose, not a semantic change. This issue may be filed
     as its own or folded into issue 1, whose `SessionCancel` ask it overlaps;
     do **not** drop it.

  **Rejected:** editing `docs/change-review-phases-a-e.md:220-222`. It says
  "internal (suspend/resume/TtlExpired) and checkpoint entries never consume
  ordinals", which is **correct** and now has a normative citation
  (RFC-MACP-0006 §3.2:117). It needs no change; noting that here prevents a
  later agent from "fixing" it into agreement with item 9.

  **Rejected:** editing `CLAUDE.md`. Its freeze-profile line (`:74`) describes
  the rejected-message carve-out and names the handoff synthetic accept as
  `EntryKind::Incoming`. Since no entry changes classification, the line stays
  true. Item 9's premise, had it been implemented, would have required editing
  it — its remaining accurate is a check on this plan's conclusion.
- **Edge cases & failure modes.**
  - *Issue links.* Each issue must link the blob URL for this plan
    (`https://github.com/multiagentcoordinationprotocol/macp-runtime/blob/main/plans/session-lifecycle-entries-9-10.md`),
    which resolves only after the PR merges. Note that in the issue body.
  - *Drift between the rustdoc and the tests.* The rustdoc asserts a contract
    Phase 1 tests. Name the specific test functions in the rustdoc so a reader
    can run them, and so deleting them leaves a dangling reference a reviewer
    will notice.
  - *Backlog rewrite scope.* Items 9 and 10 only. Items 11 and 13 also concern
    `validate_replay_consistency` and are explicitly out of scope (see
    Enterprise concerns).
- **Acceptance criteria.**
  1. `make_internal_entry`'s rustdoc cites RFC-MACP-0006 §3.2:117 and :122,
     states the no-ordinal / no-publication consequence, and cross-links
     `synthesize_due_accept` as the deliberate opposite.
  2. `synthesize_due_accept`'s rustdoc gains the reciprocal cross-link.
  3. `docs/API.md`'s three lifecycle RPC sections each state: runtime is the
     sole emitter, not submittable via `Send`, enters durable replayed history,
     consumes no accepted ordinal, not delivered on a subscribe stream.
  4. `docs/API.md`'s `StreamSession` section describes the delivered set as the
     ordinal-consuming envelopes rather than "all accepted envelopes".
  5. `plans/defer/follow_ons.md` item 10 is marked DONE with the `7c652b6`
     citation; item 9 is marked closed-not-a-defect with both verbatim §3.2
     citations, the relative dating, and a pointer to Phase 1's tests.
  6. The upstream issues exist in the spec repo with the content above (three,
     or two if ask #3 is folded into #1 — but ask #3's content must appear
     somewhere); their URLs are
     recorded in this plan's `## Plan review` section or in `PROGRESS.md`.
  7. No file outside this repo is modified (`git -C ../multiagentcoordinationprotocol
     status --porcelain` is empty apart from anything pre-existing).
- **Tests.** None — documentation and backlog only. The claims it records are
  tested by Phase 1; that is the point of criterion 1's test-name references.
- **Docs.** This phase *is* the doc work: `docs/API.md` (four sections), plus
  rustdoc in `src/runtime.rs`, `src/replay.rs`,
  `crates/macp-storage/src/log_store.rs`, and `plans/defer/follow_ons.md`.
  `docs/architecture.md`, `docs/sdk-guide.md`, `docs/examples.md`, `README.md`
  and `CLAUDE.md` are deliberately **not** touched: their "accepted envelopes"
  phrasing stays accurate because no entry changes classification. (Had item 9
  been implemented, all five would have needed rewriting — a fair measure of
  the one-way door avoided.)

---

## Long-term posture

**The one-way door in this plan is the one we are declining to walk through.**

Item 9 as briefed was correctly identified as a one-way door. Its cost, priced
concretely from what was read this session:

- *Log-format divergence with no discriminator.* Suspend/resume/cancel entries
  written after the change would be `EntryKind::Incoming`. An older binary
  deserializes them fine (no new variant, so no per-record skip) and then routes
  them into `replay_entry`'s `Incoming` arm, where
  `mode.authorize_sender` refuses `sender: "_runtime"`
  (`crates/macp-modes/src/mode/mod.rs:169-174`,
  `crates/macp-modes/src/mode/handoff.rs:307`) → `replay_session` errors →
  `src/main.rs:386-392` skips the session, or `:379-385` aborts startup under
  `MACP_STRICT_RECOVERY=1`. **Any session that had ever been suspended would
  vanish from the registry on downgrade.** Cancelled sessions would mostly
  survive by accident, because terminal compaction (`src/runtime.rs:1048`)
  replaces the log with a single checkpoint.
- *Ordinal renumbering.* `get_incoming_after` assigns ordinals positionally over
  the `Incoming`-filtered iterator (`crates/macp-storage/src/log_store.rs:126-133`).
  Reclassifying adds ordinals mid-session, which
  RFC-MACP-0006 §3.2:123 forbids: "An ordinal MUST be stable for the life of the
  session … A runtime that renumbers accepted envelopes breaks every resuming
  client silently."
- *Documentation blast radius.* Six files plus `CLAUDE.md` assert the current
  contract in prose (`docs/API.md:84`, `:92`; `docs/architecture.md:75`, `:135`,
  `:209`; `docs/examples.md:66`; `docs/sdk-guide.md:94`, `:101`, `:116`;
  `README.md:53`, `:425`; `CLAUDE.md:76`; and the flat contradiction at
  `docs/change-review-phases-a-e.md:221`).
- *And it would be non-conformant anyway*, against two verbatim MUST NOTs.

**On the reading side of a semantics gate — the question the brief asked, answered
for the record.** Revision gating in this runtime is *structural*, not
duplicated: replay rebuilds the `Session` carrying the rev recorded on the
`SessionStart` entry (`src/replay.rs:346-349`, `LogEntry.semantics_rev` with
`#[serde(default)]` at `crates/macp-storage/src/log_store.rs:40-41`, so legacy
entries load as 0), then re-runs ordinary mode dispatch, so every gate fires
again against the stored rev. A gate therefore covers reading and emission at
once, with no separate reader branch — which is why rev 2's and rev 3's gates
live in the modes (`crates/macp-modes/src/mode/handoff.rs:169`, `:277`, `:404`,
`:637`, `:712`; `multi_round.rs:86`) and `src/replay.rs` branches on the rev
nowhere. **This mechanism is sound and is not the reason item 9 is rejected.**
Had item 9 been conformant, `semantics_rev >= 4` would have been the right
gate, with `CURRENT_SEMANTICS_REV` (`crates/macp-core/src/session.rs:110`) going
`3 → 4` and no proto change (the field is runtime-internal — it appears in no
`.proto` and no client can set or observe it). The residual hazard would have
been the *classification* half, which is carried by `entry_kind` in the entry
rather than by the rev, so a downgraded reader has no rev to consult before it
has already mis-dispatched — the reason the door is one-way even with a correct
gate.

**Debt this plan deliberately does not take on.** Phase 2 leaves logs written
before it carrying the old `banked_ms` quantity under the same field name, with
no discriminator. The alternative — a new `LogEntry` field — is a
`constructible_struct_adds_field` semver major (`LogEntry` is public,
literal-constructed across crates, and was not sealed in 0.8.0), which
`release-plz.toml`'s `semver_check = true` would surface as a blocked release PR
for all seven crates. Paying a major to disambiguate a field with zero readers
is the wrong trade. The mitigation is the rustdoc warning in Phase 2's
criterion 7, which is what a future consumer needs in order to know the field
is not safe to consume across the boundary at which the quantity changed (stated
as the change, not as a version number — Round 3, Phase 2 edge cases).

**Where a faster approach would have created debt.** Folding Phase 1 into Phase
2 would have been one smaller PR. It would also have meant the only test of the
ordinal/delivery contract arriving in the same diff as a change to the
`SessionResume` payload — so a reviewer could not tell whether the new tests
pass because the contract holds or because they were written against the diff.
Landing the tripwire first, on unchanged behavior, is what makes it a tripwire.

---

## Enterprise concerns

**Reliability / failure domains.** The dominant failure mode in this area is
*silent session loss at startup*: `replay_session` returning `Err` causes
`src/main.rs:386-392` to skip the session, leaving it absent from the registry
with only a `warn` line, or aborts startup under `MACP_STRICT_RECOVERY=1`. Every
phase here is chosen to widen, not narrow, the space of logs that replay
successfully. Phase 3 makes the one remaining silent drop (an unrecognized
runtime-internal `message_type`) visible.

**Observability.** What is visible when this area breaks in production:
`validate_replay_consistency` (`src/replay.rs:191-276`) compares eight fields
warn-only, aggregated into `recovery_replay_mismatches` and exported via
`record_replay_mismatch` (`src/main.rs:411-418`) — that counter is the
production signal for live/replay divergence and needs no change here.
Suspend/resume/cancel each already increment a per-mode counter
(`src/metrics.rs:90`, `:96`, `:102`). Phase 3 adds a structured `warn`. No new
metric is introduced; see Phase 3's rejected alternatives for why.

**Scale.** No phase changes any hot path. Phase 2 replaces one arithmetic
expression in `resume_session`, which is a control-plane RPC, not a
per-message path. Phase 1 and 3 add tests and one log line. `seen_message_ids`
growth (follow-on 6) is untouched — deliberately: had item 9 been implemented
with dedup slots for the lifecycle entries, it would have added up to
`2 × MAX_SUSPENSION_CYCLES` (2048) ids per session to a set that re-serializes
on every snapshot.

**Migration / rollback.** Phases 1, 3 and 4 are fully reversible and produce no
persisted-format change. Phase 2 changes a recorded value going forward; rolling
the binary back resumes writing the old quantity, and since nothing reads the
field, mixed-generation logs have no functional consequence. There is no data
migration and none is needed.

**Security.** Unchanged. No phase touches authentication, sender derivation,
rate limiting, or payload limits. Worth recording explicitly: item 9 would have
begun publishing envelopes with `sender: "_runtime"` to `StreamSession`
subscribers — a synthetic identity that **no RFC defines**
(`grep -rn _runtime rfcs/ registries/` in the spec repo returns **nothing** —
verified in Round 3). **Round 3 correction to this paragraph's citation:** the
draft said "the spec pins only the payload attribution fields `cancelled_by` /
`suspended_by` / `resumed_by`, RFC-MACP-0001 §7.3:189". Two errors. (a) Line 189
is in **§6 Envelope Model** (`:163-196`), not §7.3 (`:248-272`). (b) Only
`cancelled_by` is pinned in RFC prose, at `:189` ("The `cancelled_by` field in
`SessionCancelPayload` is set by the runtime and MUST match the authenticated
`sender` of the originating `CancelSession` RPC"). `suspended_by` and
`resumed_by` appear **only** as proto fields
(`schemas/proto/macp/v1/core.proto:144`, `:152`) with no prose rule attached —
which, if anything, slightly strengthens the point: the spec has said even less
about attribution on the suspend/resume envelopes than the draft claimed, so
inventing a client-visible `_runtime` sender for them would be further out on
its own. Declining item 9 keeps that
runtime-local invention out of client-visible surface, which is a security
posture benefit and not only a conformance one.

**Adjacent findings, deliberately out of scope.** Each is recorded here rather
than folded into a phase, because none is item 9 or 10 and folding them in
would make the phases dishonest about what they deliver:

- `validate_replay_consistency` does not compare `semantics_rev`
  (`src/replay.rs:191-276`), so a log/snapshot rev disagreement is silent.
  Harmless for emission today, because `src/main.rs:376` inserts the *replayed*
  session and the log therefore wins. Belongs with follow-ons 11/13.
- `crates/macp-storage/src/storage/recovery.rs:6-24` (`recover_session`) folds
  every entry's non-empty `message_id` into `seen_message_ids`, kind-blind. It
  is public API with **no production caller** (only the re-export at
  `storage/mod.rs:21` and its own tests), and is currently harmless because
  runtime-authored entries carry an empty `message_id`
  (`src/runtime.rs:290`, `:1286`, `compaction.rs:25`). Item 9 would have made it
  a live divergence source. Worth a note if anyone gives runtime entries ids.
- `get_incoming_after` reads only the in-memory `LogStore`
  (`crates/macp-storage/src/log_store.rs:108-111`), so an evicted session's
  durable history is invisible to passive subscribe. Not reachable today:
  `evict_stale_sessions` removes the registry entry (`src/runtime.rs:1601`,
  inside the write-guard block `:1598-1603`) *before* the log cache
  (`remove_session_log` at **`:1605`** — Round 3 fix; the draft said `:1607`,
  which is the `stream_bus.remove_if_unused` call at `:1608`), so
  `process_subscribe_frame`'s
  `get_session_checked` returns `None` → `NotFound` (`src/server.rs:476-480`).
  The ordering is load-bearing and asserted nowhere.
- `src/server.rs:1945`'s `stream_session_emits_accepted_envelopes_only`
  overclaims (it reads one envelope off one stream). Phase 1 supplies the
  coverage the name implies; renaming the old test is left alone to keep Phase
  1's diff auditable.
- `suspend_session` appends the log entry before `session.suspend(now_ms)?`
  (`src/runtime.rs:1099-1104`). If `suspend` could fail the entry would already
  be durable and replay would diverge (Open → Suspended). Unreachable: the
  `state == Open` guard at `:1083` runs under the session mutex. Latent, worth
  a `debug_assert` if anyone touches the function.
- `integration_tests/` is linted by neither CI gate (follow-on 19); Phase 1 adds
  a file there and must be formatted by hand.

---

## Open questions

**Q1 — Item 9 is rejected rather than implemented. Decided by Opus; flagged for
your review because it contradicts the brief.**

The brief asked for an implementation plan for item 9, correctly anticipating a
one-way door and a new `semantics_rev` gate. The analysis concluded item 9 must
not be implemented: RFC-MACP-0006 §3.2:117 names `SessionSuspend` /
`SessionResume` explicitly as bookkeeping entries that **MUST NOT consume
ordinals**, and :122 says a runtime **MUST NOT deliver an internal annotation**
on a subscribe stream. Both clauses postdate (2026-08-30) the RFC-MACP-0001
§7.5 clause item 9 relied on (2026-06-22), and item 9 (2026-09-11) does not cite
them.

**Not routed to Fable**, and the reasoning for that is itself worth stating:
Fable is for a genuine fork on a critical call. There is no fork here — the
question is settled by two verbatim MUST NOTs that name the exact envelope
types, which Fable would read identically. Routing it there would buy a second
reading of two sentences at twice the cost. What *is* a judgment call is whether
to override the spec deliberately, and that is yours, not Fable's.

If you want item 9 anyway — a considered local departure from RFC-MACP-0006
§3.2, which is a legitimate choice this repo has made before (follow-on 14's
`count` alias) — the plan changes to: (1) `CURRENT_SEMANTICS_REV` `3 → 4` with a
rev-4 bullet in the doc block at `crates/macp-core/src/session.rs:67-109`;
(2) build the `Envelope` first in each of the three functions and derive the
entry via the existing `make_incoming_entry` (`src/runtime.rs:247`) rather than
adding a builder, mirroring `synthesize_due_accept:757` exactly; (3) hoist
lifecycle handling in `replay_entry` into a `message_type` match that runs
*before* the `entry_kind` match, so both log generations replay through one
path and `_runtime` never reaches `authorize_sender` — this is mandatory, not
optional; (4) do **not** add `maybe_insert_checkpoint` to the suspend/resume
paths, because more mid-session checkpoints widen the
`suspension_intervals`-loss source documented at
`crates/macp-core/src/session.rs:377-381`; (5) keep `message_id` empty and skip
dedup insertion, so `recover_session` (`storage/recovery.rs:6-24`) stays inert
and `seen_message_ids` does not grow by 2 per cycle; (6) accept the
downgrade-drops-suspended-sessions door in Long-term posture; (7) rewrite the
six doc files plus `CLAUDE.md:74` and `:76`; (8) file a spec issue proposing the
§3.2:117/:122 change, because shipping against a MUST NOT without an upstream
attempt is how a local departure becomes a permanent fork. Phases 1-4 here
remain valid as prerequisites in that world too — Phase 1's tests would simply
be inverted after the gate, and Phase 1 is what would make that inversion
visible and deliberate.

**Q2 — `SessionCancel`'s classification is a genuine spec gap. Decided:
treat it as an internal annotation (no ordinal, no delivery), i.e. no change.**

RFC-MACP-0006 §3.2:117 enumerates `SessionSuspend` / `SessionResume` / TTL
expiry / checkpoints but not `SessionCancel`; it is covered only by the trailing
"and any other internal log entry". The supporting evidence for the decided
reading: RFC-MACP-0001 §7.3:269 calls it a "terminal annotation", and
§3.2:122's prohibition is on delivering "an internal annotation" — the same
word. Also: `SessionCancel` is a Core control-plane side effect with an
unspecified Envelope `sender`, exactly like the other two, and unlike the
handoff synthetic accept whose `sender` the spec pins. So the current behavior
is the defensible default and no code changes. Phase 4 files the upstream issue
asking for it to be named explicitly. Pending confirmation only in the sense
that a spec answer could later contradict it; `/implement` should log this to
`ASSUMPTIONS.md` as `UNCONFIRMED`.

**Q3 — Should replay consume `SessionResumePayload.banked_ms` once Phase 2 makes
it correct? Decided: no.**

RFC-MACP-0001 §7.5:318 and RFC-MACP-0003 §2:48 both say the field is "recorded
… for replay", which reads as an instruction to consume it. Three reasons not
to, in order of weight: (1) legacy logs record the *wrong* quantity (the pause
duration), so consuming the field would change how every log written before
Phase 2 replays
— the exact class of break `semantics_rev` exists to prevent, and there is no
discriminator to gate on without a `LogEntry` field that costs a semver major;
(2) the spec's own determinism argument does not rest on the field —
RFC-MACP-0003 §2:48 concludes "every input to this computation (the recorded cap
and the suspend/resume event timestamps) is on the replayed timeline", naming
the timestamps, not `banked_ms`; (3) re-deriving from two recorded timestamps is
strictly more robust than trusting a third recorded value that must agree with
them. Phase 2 keeps the existing rustdoc's conclusion and corrects only its
premise. Log to `ASSUMPTIONS.md` as `UNCONFIRMED`.

**Q4 — Correcting `banked_ms` changes a permanently recorded value. Decided:
correct it, unbreaking, ungated.**

Decided as tier 2 (consequential but decidable from the code) rather than
escalated, on measured exposure: `grep -rn banked_ms` over `src crates tests
integration_tests benches` returns only the write at `src/runtime.rs:1160` and
its own comment — zero readers; zero hits in `docs/`, `README.md`, `CLAUDE.md`;
and the field is not deliverable to clients because the `SessionResume` entry is
`Internal`. The single surface where an operator could have observed the old
value is the documented `log.jsonl` audit path (`docs/deployment.md:140`), which
Phase 2 updates. Hence `fix(runtime):` without `!`. If you know of an external
consumer parsing `log.jsonl` for `banked_ms`, say so and Phase 2 becomes
`fix(runtime)!:` with a deployment note — that is the one input that would
change the call.

**Q5 — Should Phase 1 also rename `src/server.rs:1945`?** Decided: no. The name
`stream_session_emits_accepted_envelopes_only` overclaims relative to what it
asserts, but renaming an existing test inside a phase whose value is a clean,
auditable set of new tripwires dilutes the diff. Recorded under Enterprise
concerns as an adjacent finding.

---

## Repo map

Saved to `plans/session-lifecycle-entries-9-10-PROGRESS.md` alongside the PR
strategy and per-phase risk tags, per `/implement`'s rules.

---

## Plan review

**Round 1 — self-review by the drafting agent (fresh Opus, no prior context on
this repo), 2026-09-28.** Performed as a skeptical re-read against the code,
before any implementation. Verdict: **SOUND**, after the corrections below.

*Sources opened and read this session (not recalled):* `src/runtime.rs`
(1-360, 700-1260, 1572-1616 plus a full symbol map), `src/replay.rs` (1-395 plus
a full symbol map), `src/main.rs` (300-410), `src/stream_bus.rs` (whole file),
`src/server.rs` (108-180, 330-540, 1945-1980),
`crates/macp-storage/src/log_store.rs` (whole file),
`crates/macp-storage/src/storage/recovery.rs` (1-40),
`crates/macp-core/src/session.rs` (20-220, 244-424),
`crates/macp-modes/src/mode/mod.rs` (1-205),
`crates/macp-modes/src/mode/handoff.rs` (250-310),
`tests/integration_mode_lifecycle.rs` (545-643),
`macp-proto-0.1.10/proto/macp/v1/core.proto` (132-160), `CHANGELOG.md` (1-40),
and in the spec repo `rfcs/RFC-MACP-0006-transport-bindings.md` (96-136),
`rfcs/RFC-MACP-0001-core.md` (262-282, 305-320),
`rfcs/RFC-MACP-0003-determinism.md` (§2). Four Opus subagents covered the
storage layer, `semantics_rev` threading, the test inventory, and the doc
surface, each returning `file:line` findings.

*Claims verified against the code, and what changed as a result:*

1. **The brief's line numbers are all stale** (it cited `runtime.rs:816, 875,
   933, 272, 247, 540, 669, 870, 923-931`). At HEAD the functions are at
   `:1032`, `:1092`, `:1162` (emission), `:282` (`make_internal_entry`), `:247`
   (`make_incoming_entry`), `:1068`/`:1123` (suspend/resume). Every citation in
   this plan was re-derived from a read, not carried over.
2. **Item 10 is already done** — `make_internal_entry` takes `at_ms`
   (`src/runtime.rs:287`); confirmed by `git log -S "at_ms: i64,"` → `7c652b6`.
   The plan was restructured around this rather than planning a completed fix.
3. **`CURRENT_SEMANTICS_REV` is 3, not 2** as the brief assumed
   (`crates/macp-core/src/session.rs:110`, bumped by `ab64d1b`, released in
   0.8.3 per `CHANGELOG.md:23`). Any new gate would be rev 4. Corrected
   throughout.
4. **Item 9's premise fails against RFC-MACP-0006 §3.2.** I did not take the
   subagent's quotation on this: I opened
   `rfcs/RFC-MACP-0006-transport-bindings.md:96-136` and read :117 and :122
   verbatim, then `rfcs/RFC-MACP-0001-core.md:314` and `:269`, then dated all
   three with `git log -S` (§3.2 = `f1489df`, 2026-08-30; §7.5's clause =
   `048739d`, 2026-06-22). This inverted the plan from "implement item 9" to
   "reject item 9 and pin the invariant".
5. **The mechanical consequence of item 9 was verified, not assumed.**
   `replay_entry`'s `Incoming` arm calls `authorize_sender`
   (`src/replay.rs:125`); the default implementation
   (`crates/macp-modes/src/mode/mod.rs:169-174`) and handoff's
   (`handoff.rs:307`) both reject a non-participant, and
   `make_internal_entry` sets `sender: "_runtime"` (`src/runtime.rs:292`). So
   item 9 breaks replay, not merely conformance.
6. **`banked_ms` is wrong, and the deadline is not.** Read
   `RFC-MACP-0003-determinism.md:48` and `RFC-MACP-0001-core.md:318` for the
   required quantity, then `src/runtime.rs:1153-1156` for what is written, then
   `crates/macp-core/src/session.rs:287`/`:314` to confirm the deadline
   arithmetic is algebraically equivalent and unaffected. Phase 2 was narrowed
   to the payload field alone as a result.
7. **Zero readers for `banked_ms` confirmed by grep** over `src crates tests
   integration_tests benches` and over `docs/ README.md CLAUDE.md`. This is what
   downgraded Q4 from a potential Fable escalation to a tier-2 decision.
8. **Checkpointing the suspend/resume paths was considered and rejected on
   evidence**, after reading `synthesize_due_accept:778-794`'s argument for the
   opposite and `crates/macp-core/src/session.rs:377-381`'s under-count source
   #2. Recorded in Q1's override sketch rather than silently omitted.
9. **The eviction concern was chased to ground rather than left as a worry.**
   Read `evict_stale_sessions` (`src/runtime.rs:1572-1616`) and confirmed the
   registry removal at `:1600-1604` precedes the log-cache removal at `:1607`,
   so the empty-replay path is unreachable via subscribe. Demoted from a phase
   to an Enterprise-concerns note with the ordering named as load-bearing.
10. **`LogEntry` is not `#[non_exhaustive]`** (`log_store.rs:11-12`) even though
    `PersistedSession` is (`registry.rs:34-35`). This is what makes a
    discriminator field for `banked_ms` a semver major, and it is why Phase 2
    takes the rustdoc-warning route. The brief's §8a pointer was about
    `PersistedSession`; the binding constraint here is the unsealed `LogEntry`.
11. **`EntryKind` has no `#[serde(other)]`** (`log_store.rs:4-9`), so adding a
    variant would make older binaries skip whole log lines and shift ordinals
    silently. Added to Constraints as an explicit prohibition, since a
    `Lifecycle` variant is the obvious-looking alternative to item 9.
12. **Test-coverage gap confirmed at the right level.** Read
    `src/server.rs:1945` and found `stream_session_emits_accepted_envelopes_only`
    reads exactly one envelope — the name overclaims. Read
    `crates/macp-storage/src/log_store.rs:189` and found the only ordinal test
    uses a generic `Internal` entry. So the invariant really is unpinned, and
    Phase 1's three levels are each necessary rather than redundant.
13. **`tests/conformance/` cannot host these tests** (vendored, byte-diffed by
    CI, and its transcript format carries only client messages). Moved into
    Constraints so `/implement` does not attempt it.
14. **`CHANGELOG.md` is release-plz-generated** (`release-plz.toml`, and every
    released section is machine-shaped). No phase edits it; the release note is
    the commit subject. Added to Constraints.

*Phase boundaries re-checked.* Phase 1 is tests-only and verifiable by
`cargo test` plus the criterion-6 mutation. Phase 3 depends on nothing and was
explicitly marked as such rather than inheriting a false dependency from PR
packaging. Phases 2 and 4 have real dependencies (Phase 2 needs Phase 1's
tripwire to prove containment; Phase 4 documents Phase 2's corrected quantity),
and both are stated. No phase's `Files` list names a path outside this repo;
the two spec-repo artifacts in Phase 4 are issues, not edits.

*Acceptance criteria re-checked for falsifiability.* Each is a statement a
reviewer holding only the phase section, the diff, and the test output can
confirm. Criterion 1.6 (the mutation proof) and criterion 2.1 ("`now_ms` does
not appear in the expression") were sharpened from earlier softer phrasings
during this pass.

*Corrections applied to the plan during this round:* the four stale-line-number
fixes above; removal of an earlier draft's `EntryKind::Lifecycle` option once
the missing `#[serde(other)]` was confirmed; narrowing Phase 2 from "align
suspension banking with §7.5" to the payload field only, once the deadline
arithmetic was shown equivalent; demoting the eviction and
`validate_replay_consistency` findings out of phases and into Enterprise
concerns; and adding the Q1 override sketch, since a plan that reverses its
brief owes the reader a costed alternative rather than only a refusal.

*Not resolved and deliberately left open:* Q2 (`SessionCancel`'s spec
classification) and Q3 (`banked_ms` on replay) are decided with reasoning and
flagged for `ASSUMPTIONS.md`; Q4 has one external input (`log.jsonl` consumers)
that only the user can supply.

**Round 2 — skeptical re-read of the written plan against the code,
2026-09-28.** Verdict: **REVISE**, then **SOUND** after the five corrections
below were applied to this file. This round deliberately targeted the claims
Round 1 had taken from subagent reports rather than opened itself.

*Verified and correct as written* (opened this round): `docs/API.md` section
anchors — `### StreamSession` `:82`, `### CancelSession` `:180`,
`### SuspendSession` `:192`, `### ResumeSession` `:204`; `docs/testing.md:68` is
indeed the tier-1 coverage sentence; `docs/deployment.md:140` is indeed the
`log.jsonl` offline-audit bullet; `src/runtime.rs:501` and `:573` are the
`semantics_rev` read and stamp; `force_insert_checkpoint`'s
`message_id: String::new()` at `:1286`;
`integration_tests/tests/tier1_protocol/test_passive_subscribe.rs` helpers at
`:24`, `:32`, `:42`, `:63`, `:75`; `crates/macp-storage/src/storage/mod.rs:21`'s
`recover_session` re-export.

*Corrections applied:*

1. **Phase 1's cancel leg was wrong, and would have failed on first run.** The
   draft said "repeat the tail with `rt.cancel_session`" on the same session and
   asserted `log.len() - ordinals == 2`. Reading
   `crates/macp-storage/src/storage/mod.rs:42-47` showed `replace_log` has a
   **default** impl returning `Err(Unsupported)` which `MemoryBackend` does not
   override — so `maybe_compact_log` fails and `force_insert_checkpoint`
   (`src/runtime.rs:1049`) **appends** a `Checkpoint`, making the delta 2 for the
   cancel alone and 4 for a combined session. Worse, on `FileBackend` compaction
   *succeeds*, `replace_session_log` (`:1257-1259`) collapses the log, and
   `get_incoming_after(.., 0)` returns `Err(N)`
   (`crates/macp-storage/src/log_store.rs:123-125`). Phase 1's Approach, edge
   cases and criterion 2 were rewritten to give the cancel leg its own session,
   its own before/after ordinal comparison, and an explicit `MemoryBackend`
   requirement with the reason stated.
2. **Added the `.max()` base guarantee** (`log_store.rs:117-122`) as an edge case,
   so the cancel-leg assertion is shown to hold structurally rather than
   incidentally.
3. **Corrected `docs/change-review-phases-a-e.md:221` → `:220-222`** (the claim
   spans three lines) and added the RFC-MACP-0006 §3.2:117 citation to the
   "do not fix this" note.
4. **Fixed a malformed path** in Phase 3's criterion 4
   (`crates/macp-modes/src/mode/mode/mod.rs` → `crates/macp-modes/src/mode/mod.rs:80-82`).
5. **Confirmed `publish_accepted_envelope` has exactly three call sites** —
   `src/runtime.rs:622`, `:795`, `:943`, none in the three lifecycle functions —
   which is the load-bearing support for the Context's claim that `banked_ms` is
   not client-visible and therefore for Q4's no-`!` decision. Recorded in
   `PROGRESS.md` so it is not re-derived.

*Re-checked after the corrections:* no other acceptance criterion depends on a
raw-log-length delta; Phase 2's criteria were re-read against
`crates/macp-core/src/session.rs:250-257` to confirm `Session::suspend` leaves
`ttl_expiry` untouched (it sets only `state` and `suspended_at_ms`), which is
what makes Phase 2's expression valid where it is placed; Phase 3's and Phase 4's
criteria are unaffected.

A separate, independent reviewer round is still expected after this one.

**Round 3 — independent re-verification pass (fresh Opus reviewer, not the
drafting agent), 2026-09-28.** Verdict: **REVISE**, then **SOUND** after the
twelve corrections below were applied to this file and to `PROGRESS.md`. The
three central claims all **hold**. Nothing was taken from the drafting agent's
Round 1/2 notes; every claim below was re-derived by opening the file.

*The three central claims, re-derived independently.*

1. **The item-9 reversal is correct.** I opened
   `rfcs/RFC-MACP-0006-transport-bindings.md` myself and read `:117` and `:122`
   verbatim — both quotations in Context are **exact, character for character**,
   including "MUST NOT consume ordinals" and "A runtime MUST NOT deliver an
   internal annotation on this stream". `:117` does name `SessionSuspend` /
   `SessionResume` explicitly (and, as Q2 says, **not** `SessionCancel`). Section
   placement confirmed by heading scan: `### 3.2 StreamSession` at `:81`, next
   heading `### 3.3` at `:144`, so both lines are inside §3.2. Dating confirmed
   two ways: `git log -S "MUST NOT consume ordinals"` and
   `git log -S "MUST NOT deliver an internal annotation"` both → `f1489df`
   **2026-08-30**; `git blame -L 116,125` attributes `:116-118` and `:121-125`
   to `f1489df` (with `:120` newer still, `110add2` 2026-08-31 — reinforcing, not
   reversing). `git log -S "enter the accepted history so the suspension
   timeline"` → `048739d` **2026-06-22**, and `git blame -L 314,318` confirms
   `:314` is `048739d` 2026-06-22. Nothing later touching RFC-0006 reverses §3.2
   (last three commits: `aedfcad` 2026-09-11 CI, `cd5ac2b`, `110add2`). The
   mechanical consequence is real, not rhetorical: `src/replay.rs:125` is exactly
   `mode.authorize_sender(session, &replay_env)?;` in the `Incoming` arm;
   `crates/macp-modes/src/mode/mod.rs:169-174` is the default `authorize_sender`
   returning `Forbidden` for a non-participant; `handoff.rs:307` is
   `_ => Err(MacpError::Forbidden)`; `src/runtime.rs:292` is
   `sender: "_runtime".into()`. `CURRENT_SEMANTICS_REV: u32 = 3` is at
   `crates/macp-core/src/session.rs:110` — confirmed, and confirmed as the
   *current* value by `git log -S "CURRENT_SEMANTICS_REV: u32 = 3"` → `ab64d1b`
   (2026-09-25). One precision note rather than a correction: the default
   `authorize_sender` refuses only when `participants` is non-empty, which is
   guaranteed for standards-track modes by strict `SessionStart` but not for an
   extension mode with an empty participant list — so "every mode refuses" is
   true for the modes that matter and slightly strong in general. The conclusion
   is unaffected.
2. **Item 10 is already shipped.** `src/runtime.rs:282-305` is
   `make_internal_entry`, and `at_ms: i64` is at `:287` — read, not recalled —
   feeding `received_at_ms` (`:291`) and `timestamp_unix_ms` (`:299`). All four
   call sites pass a single read: `:322`→`:329`, `:1027`→`:1032`,
   `:1087`→`:1092`/`:1104`, `:1141`→`:1167`/`:1176`. `git log -S "at_ms: i64," --
   src/runtime.rs` → `7c652b6` alone; `git show 7c652b6 --stat` is PR #171
   (2026-09-13, 31 files) and `git branch --contains 7c652b6` lists **`main`** —
   merged, not sitting on a branch. The test at
   `tests/integration_mode_lifecycle.rs:556` exists with exactly the claimed name
   and asserts `accumulated_suspended_ms == resumed_at - suspended_at` (`:607-612`)
   plus replay agreement (`:634-641`). **Caveat I added to the plan:** it pins the
   invariant rather than differentially proving the race, and its own doc comment
   (`:550-554`) says so. Item 10 is done; the *guarantee* is the signature.
3. **The `banked_ms` defect is real.** `src/runtime.rs:1153-1156` is verbatim
   `session.suspended_at_ms.map(|at| (now_ms - at).max(0)).unwrap_or(0)` — the
   pause duration `t_r − t_s`. `rfcs/RFC-MACP-0003-determinism.md:48` requires
   `banked = deadline − t_s`, and `rfcs/RFC-MACP-0001-core.md:318` says the same
   in different words. The deadline arithmetic is indeed unaffected and
   algebraically equivalent: `Session::suspend`
   (`crates/macp-core/src/session.rs:250-257`) sets only `state` and
   `suspended_at_ms` — it does **not** touch `ttl_expiry`, so Phase 2's expression
   is valid where it is placed — and `resume` banks at `:287`, applies at `:314`.
   Zero readers re-verified by grepping the **whole repo**, not a path subset:
   outside `plans/`, the only hits are `src/runtime.rs:1142`, `:1150`, `:1152`
   (comments) and `:1160` (the write); nothing in `docs/`, `README.md`,
   `CLAUDE.md`. `publish_accepted_envelope` really has exactly three call sites
   (`:229` def; `:622`, `:795`, `:943`), none in the three lifecycle functions, so
   the "not client-visible" support for Q4 holds.

*Citations spot-checked beyond the central claims — all accurate:* every line in
`log_store.rs` (`EntryKind` `:4-9` no serde attrs, `LogEntry` `:11-56` not
`#[non_exhaustive]`, ordinal doc `:90-102`, `get_incoming_after` `:103-134`,
`Incoming` filter `:128`, base `.max()` `:117-122`, `Err(base)` `:123-125`, the
lone ordinal test `:189`); `storage/mod.rs:42-47`'s default `replace_log` →
`Err(Unsupported)` with **no `MemoryBackend` override** (grep across all four
backends); `registry.rs:34-35`'s `#[non_exhaustive]`; `force_insert_checkpoint`
`:1275`/`:1286`/`:1299` and that it really appends to `log_store`;
`maybe_compact_log` `:1225`/`:1229-1244`/`:1257-1259`; the cap-exceeded arm
`:1191-1219` appending no `TtlExpired`; `recover_session` `:6-24` kind-blind with
**no production caller**; `validate_replay_consistency` `:191-276` comparing 8
fields and never `semantics_rev`; all of `src/server.rs` (`:114-152`, `:349`,
`:458`, `:476-480`, `:483-490`, `:512-521`, `:925`, `:987`, `:1049`, `:1154`,
`:2518`) and that `:1945` reads one envelope; `src/main.rs` `:261-262`,
`:313-402`, `:340`, `:349-356`, `:371-374`, `:376`, `:379-385`, `:386-392`,
`:411-418`; `crates/macp-core/src/session.rs` `:65`, `:139-192`, `:210`, `:232`,
`:332`, `:368-404` with source #2 at `:377-381`, `:405`;
`crates/macp-modes/src/mode/mod.rs` `:80-82`, `:84-99`, `:100`, `:145-150`;
`handoff.rs` `:34`, `:276`, `:277`, `:403`; `mode_registry.rs` `:700`, `:717`;
`metrics.rs` `:90`/`:96`/`:102`; `stream_bus.rs` `:6`/`:27`/`:38-46`/`:54`; all
four backends' skip-with-warning arms; every `docs/` anchor (`API.md:82`,
`:180`, `:192`, `:204-212` with GetManifest at `:245`; `testing.md:68`;
`deployment.md:140`; `change-review-phases-a-e.md:220-222`); the doc
blast-radius list (`API.md:84`/`:92`, `architecture.md:75`/`:135`/`:209`,
`examples.md:66`, `sdk-guide.md:94`/`:101`/`:116`, `README.md:53`/`:425`,
`CLAUDE.md:74`/`:76`); `follow_ons.md` item 9 = `:148-178` and item 10 =
`:180-224`; all `integration_tests` helper lines; all
`handoff_implicit_accept_live.rs` lines; `replay.rs` `:18`, `:36`, `:139-168`,
`:167`, `:188-190`, `:279`, `:288`, `:346-349`, `:395`, `:403`, `:423`, `:448`,
`:538`, `:556`; in the spec repo `RFC-MACP-0010:95-99` and `:101-111`, and
`plans/spec-followups-100-107.md:733-735`. Also confirmed the spec repo's
`origin` is `multiagentcoordinationprotocol/multiagentcoordinationprotocol`, so
Phase 4's `gh issue create -R` target is right, and that this repo's working tree
is clean apart from the two plan files. **No `==`/`!=` comparison on
`semantics_rev` exists in production code** (only inequalities, plus `assert_eq!`
in tests) — PROGRESS's claim holds.

*Corrections applied in Round 3 (twelve).*

1. **Phase 1 cannot compile as written: `LogEntry` has no `PartialEq`**
   (`log_store.rs:11` derives only `Clone, Debug, Serialize, Deserialize`;
   `PartialEq` is on `EntryKind` at `:4`). Criterion 2's "byte-identical" and the
   Approach's "the two ordinal lists are identical" would fail at `assert_eq!`.
   Rewrote the Approach, criterion 1 and criterion 2 around a
   `Vec<(u64, String)>` `(ordinal, message_id)` projection — the shape
   `log_store.rs:203-205` already uses — and forbade deriving `PartialEq` as a
   workaround. This was the only defect that would have stopped the first
   `cargo test`.
2. **Added criterion 2a**: a fresh-`LogStore` restart round trip pinning
   RFC-0006 §3.2:123 (ordinal stability across restart) for these entry kinds,
   with an explicit warning that re-deriving after `replay_session` on the *same*
   store is vacuous — `replay_session` rebuilds a `Session` and never touches the
   log.
3. **Clause-scope precision on §3.2.** `:117` is unrestricted; `:122` sits inside
   `#### Passive Session Subscription` (`:98-142`) and is textually scoped to "a
   subscribe stream". The live-`StreamSession` contract is `:85-90`/`:94`, which
   contains no equivalent prohibition. Added the scoping to Context and rewrote
   criterion 5 so each test cites the clause that actually governs it. The
   conclusion is unchanged — the live half follows from `:117` plus `:120`'s
   counting argument — but a reviewer opening `:122` must not find it scoped
   elsewhere than the test claims.
4. **The "both verbatim" block quote was verbatim from RFC-0003 only.** RFC-0001
   §7.5:318 states the same quantity in different words
   (`deadline − suspend_time` / `resume_time + banked`). Split into two
   separately-attributed verbatim quotes.
5. **"Item 10's `banked_ms` sub-item was addressed — but wrongly" mischaracterized
   item 10.** `follow_ons.md:219-224` asked for "either consume it on replay **or**
   document it as informational"; `7c652b6` took the second, which item 10
   authorized. The quantity defect is a *new* finding item 10 never raised.
   Rewritten — this matters because Phase 4 rewrites that backlog entry.
6. **Added the item-10 test-honesty note** (`:550-554`: pins the invariant, does
   not differentially prove the race).
7. **Added the observability answer the rejection owed.** The plan rejected item 9
   without saying what serves the legitimate need to observe a suspension. It is
   `WatchSessions` (RFC-0006 §3.8), already emitted from inside all three
   functions (`SessionLifecycleEvent::Resumed` at `src/runtime.rs:1183`) plus the
   `metrics.rs` counters. Without this, item 9 comes back.
8. **Phase 2's `Depends on: Phase 1` was not honest.** Phase 2 touches one
   expression no Phase 1 test reads; it is independently shippable. Restated as a
   review/packaging preference, matching the honesty Phase 3's entry already has.
9. **Phase 2 criterion 5 could flake.** Derived the bound:
   `banked_n = banked_{n-1} + (t_r(n-1) − t_sn)`, so the shrink is `<=` and strict
   only when the second suspend lands in a later millisecond than the first
   resume. Criterion 5 and the edge case now require a clock advance
   (`tests/integration_mode_lifecycle.rs:583` already has the pattern).
10. **Added the force-expire edge case**: the `SessionResume` entry (payload built
    `:1157`, appended `:1169-1173`) is written *before* `session.resume`
    (`:1176`), and the cap-exceeded arm (`:1191-1219`) appends nothing, so a
    `banked_ms` can be recorded for a resume that expired the session. Pre-existing
    shape; must be documented, and the ordering must not be "fixed".
11. **Removed the hardcoded `0.8.5`** from Phase 2 criterion 7, its edge case, and
    Long-term posture. `0.8.4` is released (`8115e75`, tag `macp-runtime-v0.8.4`),
    so `0.8.5` is only the *likely* next version — release-plz computes it, and
    CLAUDE.md's opening line warns against trusting a version in prose.
12. **Three citation fixes.** (a) `MACP_MEMORY_ONLY=1` is **not** at
    `integration_tests/tests/common/mod.rs:49` (that is `grpc_client`; the file
    sets no env vars) — it is the `ServerManager` default at
    `integration_tests/src/server_manager.rs:69`; the shared manager is
    constructed at `common/mod.rs:36`. Fixed here and in `PROGRESS.md` (two
    places). (b) `RFC-MACP-0001:189` is in **§6 Envelope Model** (`:163-196`), not
    §7.3 (`:248-272`), and only `cancelled_by` is pinned in prose there —
    `suspended_by` / `resumed_by` exist only as proto fields
    (`schemas/proto/macp/v1/core.proto:144`, `:152`). (c)
    `evict_stale_sessions`'s log-cache removal is `:1605`, not `:1607` (which is
    the `stream_bus.remove_if_unused` call at `:1608`); registry removal is
    `:1601` in the guard block `:1598-1603`. Fixed here and in `PROGRESS.md`.
    Also corrected the plan's **base commit**: `5e95c4a` is not an ancestor of
    `main` (squashed into PR #203); the verified tree is `8115e75`.

*Added upstream ask #3 to Phase 4.* The plan filed issues about `SessionCancel`'s
classification and RFC-0010's analogy but not about **RFC-MACP-0001 §7.5:314
itself** — the bare "they enter the accepted history" with no pointer to §3.2,
which is the sentence that produced item 9. Phase 4's own stated method is
"a citation at the point of confusion"; it applied that to the rustdoc and to
RFC-0010 but not to the clause a reader hits first. Added as ask #3 (foldable
into #1, not droppable), with the issue counts updated throughout Phase 4.

*Re-checked and left alone.* The two-PR split is the right shape: PR 1 is
protective and additive (tests + one `tracing::warn!`), PR 2 isolates the single
behavior change for review — and burying a one-line arithmetic change under
~15 files of test/doc churn is genuinely how a wrong formula ships unreviewed.
Phase boundaries are real and independently verifiable; Phase 3's `Depends on:
nothing` was already honest (Phase 2's now is too). Phase 1 criterion 6's
mutation proof is the strongest criterion in the plan and is correctly marked as
must-be-demonstrated. The `MemoryBackend`-vs-`FileBackend` compaction fork is
real and correctly reasoned (verified: no `MemoryBackend` override of
`replace_log`; `force_insert_checkpoint` appends with
`compacted_incoming_ordinals: 0`; the base is a `.max()` so it cannot reset an
earlier real base). Q2, Q3 and Q5's decisions are sound as reasoned. Phase 2's
proposed expression is correct: `.max(0)` is load-bearing after `saturating_sub`
(which saturates at `i64::MIN`, not 0), and `ttl_expiry` defaults to `i64::MAX`
for builder-constructed sessions.

*On Q1/Q4's routing — my own independent view, agreeing with the plan.* **Q1 to
the user, not Fable, is right.** I read both MUST NOTs myself, confirmed they
name the exact envelope types, confirmed the dating two ways, and confirmed the
replay breakage mechanically. There is no interpretive fork left for a second
model to resolve — the residual question is whether to *deliberately depart* from
a normative MUST NOT, which is an owner's call about this project's relationship
to its own spec, not a reasoning problem. Fable would read the same two sentences
and reach the same place at twice the cost. What makes the routing *correct*
rather than merely defensible is that the plan hands the user a fully costed
override path (Q1's eight steps) instead of only a refusal — that is what a
reversal of the brief owes the person who wrote it. **Q4 to the user is also
right, and for a sharper reason:** the single input that would flip
`fix(runtime)` to `fix(runtime)!` is whether an external consumer parses
`log.jsonl` for `banked_ms`. That fact does not exist anywhere in this repo — I
grepped the whole tree — so no amount of additional model reasoning can produce
it. It is the user's knowledge or nobody's. Asking is the only correct move.

*Not resolved, and correctly left open:* Q2 (`SessionCancel`'s spec
classification — a genuine gap, since §3.2:117 really does omit it) and Q3
(`banked_ms` on replay). Both are decided with reasoning and flagged for
`ASSUMPTIONS.md` as `UNCONFIRMED`, which is the right disposition.

**Round 3 verdict: SOUND as now written.** No further reviewer round is required
before `/implement`; the two open questions are for the user, not for another
reviewer.
