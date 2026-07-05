# RFC / Spec-Repo Changes

**Target repo:** `../multiagentcoordinationprotocol/` · **Source:** master plan §7,
§1.10, §2.3, §2.5, §3.4, §4.5, §5.7 · **File these first** — items 1–3 block Phase A
decisions.

Two categories: **normative changes** (need an RFC edit or proto change, i.e. a spec
PR and possibly a version bump) and **corrections** (doc/text fixes, low ceremony).
Suggested vehicle: one GitHub issue per item below, with items 1–3 additionally
proposed as spec PRs since the runtime needs their outcome.

## A. Blocking normative changes (runtime work waits on the decision)

### 1. Bind `MAX_SUSPEND_MS` at SessionStart (RFC-0001 §7.5, RFC-0003 §2)
**Problem:** the suspension cap is a deterministic replay input but is runtime
config, not session-bound. Two runtimes (or one runtime reconfigured) produce
different terminal states on identical history — violating RFC-0003 §1/§8
cross-implementation replay.
**Proposal:** either (a) add `max_suspend_ms` to `SessionStartPayload` (bound like
`ttl_ms`, default from runtime config when 0), or (b) explicitly declare the cap
outside the determinism boundary in RFC-0003 §2. Prefer (a) — (b) weakens the replay
guarantee.
**Runtime dependency:** replay determinism claims; conformance vectors for
suspend/expire. Decision needed before freeze; proto field addition is
backward-compatible (proto3 default 0 = "runtime default").

### 2. Handoff `implicit_accept_timeout_ms` timer contract (RFC-0012 §4.5, RFC-0010)
**Problem:** RFC-0012 §4.5 says the timeout is "consumed by a timer that emits a
synthetic accept into history", but specifies no timing source, no authority (who is
the synthetic accept's sender?), no determinism story, and no interaction with
suspension. No conformant implementation is currently possible; this runtime instead
auto-accepts inside the Commitment handler using the initiator-forgeable envelope
timestamp (master §2.5).
**Proposal:** specify: (a) the timer references the runtime's acceptance timeline
(log-recorded), not client timestamps; (b) the synthetic `HandoffAccept` is
runtime-emitted (like `SessionCancel`) with a defined sender convention; (c) it
enters history *before* commitment evaluation; (d) suspended time does not count
toward the timeout.
**Runtime dependency:** master §2.5's final fix. The interim mitigation (acceptance
timestamp) proceeds regardless; the timer implementation waits for this.

### 3. Commitment echo of resolved `policy_version` (RFC-0012 §6.1, RFC-0001 §7.1)
**Problem:** the spec says empty `policy_version` at SessionStart resolves to
`policy.default`, but is silent on (a) whether the *persisted* session metadata
records the rewritten value (replay-equality relevant per RFC-0012 §8) and (b) what
`CommitmentPayload.policy_version` must carry when the session started with empty.
This runtime rewrites to `policy.default` and then requires the Commitment to echo
it — a client that sent `""` gets rejected unless it echoes a value it never set.
**Proposal:** specify that empty `commitment.policy_version` matches the resolved
default (treat empty as "the session's bound policy"), and that persisted metadata
records the resolved id.
**Runtime dependency:** master §2.3 — the runtime-side fix direction follows this
decision. If upstream stalls, the runtime should unilaterally accept empty as
matching (permissive is forward-compatible with either outcome).

### 4. Quorum `threshold` schema defects (RFC-0012 §4.2, `schemas/json/policy/quorum-rules.schema.json`)
**Problem:** (a) the `percentage` threshold's `value` is typed `integer` with the
description "fraction (0-1)" — an integer fraction can only express 0% or 100%;
implementations (this runtime included, `quorum.rs:74`) treat it as 0–100. (b) There
is no separate *participation quorum* concept — this runtime's evaluator invented
one by reusing `threshold`, which RFC-0012 §4.2 defines as an approval-count
override (master §1.10).
**Proposal:** (a) retype `value` as `number` with 0–1 fraction semantics, or keep
integer with explicit 0–100 percentage semantics (pick one, state it); (b) if
participation quorums are wanted, add a distinct `participation` rule field in
schema_version 3; otherwise state that `threshold` is the only gate.
**Runtime dependency:** master §1.10 drops the evaluator's participation check
regardless; the percentage-unit fix aligns `rules.rs` types with whichever way the
schema goes.

### 5. `ListSessions` pagination (RFC-0006 §3.8, `core.proto`)
**Problem:** no page token/limit in `ListSessionsRequest`; a runtime with many
sessions must return everything (master §3.4).
**Proposal:** add `page_size` + `page_token` request fields and `next_page_token`
response field (proto3-compatible addition).
**Runtime dependency:** master §3.4's full fix; until then the runtime caps
server-side and documents it.

### 6. Multi-round mode proto (`macp/modes/multi_round/v1/multi_round.proto`, new)
**Problem:** `ext.multi_round.v1` is the only advertised mode without a canonical
proto payload; fixtures and SDKs carry ad-hoc JSON (master §4.5).
**Proposal:** add `message ContributePayload { string value = 1; }` to the spec
repo's proto package and publish a new `macp-proto` crate version. Decide `string`
vs `bytes`/structured before publishing — wire-frozen after.
**Runtime dependency:** master §4.5 Phase A work starts on the published crate.

## B. Non-blocking corrections (text/doc fixes)

7. **Mode RFCs 0007–0011 reference the removed `context` field** —
   `SessionStartPayload` replaced `bytes context` with `context_id` + `extensions`;
   Handoff's context-frozen determinism class (RFC-0003 §5) leans on the removed
   field. Update the mode RFCs' SessionStart sections to the new fields.
8. **RFC-0006 §3.6 `SessionLifecycleEvent` text stale** — lists
   CREATED/RESOLVED/EXPIRED; proto also defines SUSPENDED/RESUMED/CANCELLED.
9. **`policy.proto` comment stale** — says schema_version "currently 1"; RFC-0012
   defines {1,2}.
10. **Spec-repo CLAUDE.md/README lifecycle stale** — describe the pre-suspension
    state machine and a `SessionEnd` message that exists in no proto/RFC.
11. **`rfcs/RFC-MACP-0001.md` stub index omits RFC-0012.**
12. **`SessionStartPayload.intent` (field 1) undocumented** — absent from RFC-0001
    §7.1's binding requirements; state whether runtimes must preserve/validate it.
13. **Decision vote-cardinality wording** (RFC-0007 §5.3) — "or more permissive"
    permits multiple votes per participant with no tally/replacement semantics,
    conflicting with the mode's semantic-deterministic class. Define replacement
    semantics or drop the permissive branch.
14. **RFC-0006 §3.2 sequence definition** — the passive-subscribe `after_sequence`
    is defined behaviorally ("starting from after_sequence + 1") but never states
    what the sequence *is* (accepted-envelope ordinal, 1-based, per session) or how
    it survives log compaction. Propose the explicit definition the runtime is
    implementing (master §1.2), so other implementations converge.
15. **Conformance fixture ownership** — propose that `schemas/conformance/` in the
    spec repo is the single canonical fixture source and runtimes consume it
    (master §5.7 step 3); include the runtime's fixture-schema draft as a starting
    point.

## Filing checklist

- [ ] Items 1–6 filed as issues (1–3 flagged "decision needed for runtime freeze")
- [ ] Items 7–15 filed as issues (batch is fine; 7–11 could be one "doc drift" PR)
- [ ] Spec PRs drafted for 1, 3, 4 once maintainers ack direction
- [ ] `macp-proto` release requested for item 6
