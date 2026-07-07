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

## 2. ListSessions pagination (runtime implementation)
Proto fields shipped in 0.1.6 (spec PR #51). Implement `page_size` capping +
opaque `page_token` / `next_page_token` in `server.rs` `list_sessions` and
the `watch_sessions` initial sync; replace the documented server-side cap.
Master plan §3.4.

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

## 7. Built-in recommended policies (gated on upstream reservation)
Master plan §4.2's companion: ship `policy.majority`, `policy.supermajority`,
and `policy.unanimous` as optional pre-registered built-ins once the spec
repo reserves the identifiers and pins their canonical rule definitions —
filed as spec issue #55. Without the reservation, runtime built-ins could
collide with user registrations of the same names.

## 8. Small/cosmetic
- Duplicate-SessionStart ack during a failed start's rollback window can
  report `duplicate=true` with a non-Open state (adversarial-review
  advisory; self-corrects on retry).
- Disk-GC sweep loads each stored session per cycle — O(stored sessions)
  I/O; optimize only if session counts grow very large (change-review D6).
- Tier-1 suite has no suspend/resume RPC coverage (noticed during the
  max_suspend_ms work; runtime/core level is covered).
- Propose the `rules.audit` block (E3's audit-verbosity vocabulary) upstream
  — currently runtime-specific, harmless to other implementations.
- Upstream: `abstention.counts_toward_quorum` wording references a
  participation-quorum concept that schema_version ≤2 no longer has
  (flagged in spec PR #48 for a future schema_version alongside any real
  participation-quorum field).
