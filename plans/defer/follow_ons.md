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
