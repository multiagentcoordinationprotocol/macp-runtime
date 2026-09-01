# PLAN — passive-subscribe resume, canonical fixture vendoring, and cardinality triage (`macp-sdk-typescript`)

**Verified against:** `macp-sdk-typescript` at `605b583` on `main`, **clean tree**, read 2026-08-31. Every `file:line` below was read in that checkout, not recalled.
**Spec checkout verified against:** `/Users/ajitkoti/code/multiagentcoordinationprotocol/multiagentcoordinationprotocol` at `110add2` (2026-08-31) — the commit that landed the RFC-MACP-0006 §3.2 client-side redelivery contract (spec PR #80). All §3.2 quotes below are from that text.
**Runtime cross-checks** were read in `macp-runtime` at `b723a95` (`feat/list-sessions-pagination`), read-only.

**Write scope:** this plan is authored in `macp-runtime`; every file it proposes to change lives in `macp-sdk-typescript`. Nothing in this document was executed. No sibling repo was modified.

---

## Context

### What the three issues are about, and what the code actually says

Issues #58, #59, and #60 are all well-argued and all three are worth acting on. Each of them, checked against the code, is also **wrong or imprecise about something load-bearing**. Those corrections are the reason the phases below are shaped the way they are.

#### 1. There is no reconnect path in this SDK at all

Issue #58 says the missing `afterSequence` means "every subscribe — **including on reconnect** — replays the full session history from sequence 0", and that this "is what makes the initiator-echo double-apply reachable on every reconnect, not only once."

The replay-from-0 claim is exactly right: `src/agent/transports.ts:57` is `await this.stream.sendSubscribe(this.sessionId);` and `MacpStream.sendSubscribe` defaults `afterSequence = 0` (`src/client.ts:169`). But **nothing in this repository reconnects.** `GrpcTransportAdapter.start()` (`src/agent/transports.ts:50-68`) opens exactly one stream, sends exactly one subscribe frame, and iterates `this.stream.responses()` until the stream ends or throws. `Participant.run()` (`src/agent/participant.ts:279-284`) drives that generator once inside a `try { for await (…) } finally { this.running = false; }` — no `catch`, no retry, no backoff. `grep -rn "reconnect" src/` returns only doc comments in `errors.ts`, `retry.ts`, and `watchers.ts`, none of which are wired to the transport adapter.

So the reachable reconnect today is *a caller invoking `start()` a second time* (directly, or after `stop()`). That works, and correctly so: `seq` (`:29`) and `delivered` (`:30`) are instance fields that survive a stream teardown, and `Participant.run()`'s `this.running` guard is reset in its `finally`, so `run()` may legitimately be called again. **That second `start()` is the call site the fix must target.** Framing the phase as "add a reconnect loop" would be scope creep and would land an untested retry policy this SDK has never had.

#### 2. `lastSequence` is not merely unused — as a resume cursor it is currently *wrong*

`src/agent/transports.ts:61` is `this.delivered++`, executed once per envelope yielded for this session. That counts **raw delivery events**. RFC-MACP-0006 §3.2 names this precisely, at `rfcs/RFC-MACP-0006-transport-bindings.md:134`:

> 1. A redelivery MUST NOT advance the client's sequence position; only a distinct accepted envelope does. A client that counts raw delivery events rather than distinct envelopes arrives at a position ahead of the true one, and its next resume silently skips history.

Under today's always-from-0 behaviour, a second `start()` re-delivers the entire history, so `delivered` ends at roughly twice the true ordinal. Passing that value as `afterSequence` — the "suggested fix" in the issue, verbatim — would skip history rather than resume it. **The fix is therefore two-part**, and the counter half is the part the issue does not name.

#### 3. The dedup guard is six copies, not one, and it does not cover what the issue implies it covers

The brief and issue #58 both describe `BaseProjection.applyEnvelope`'s `message_id` guard as the mitigation. That guard is real (`src/projections/base.ts:130` for the set, `:228-238` for the check), but **none of the five built-in mode projections extends `BaseProjection`.** Each is a standalone class carrying its own private copy:

| Projection | class | `seenMessageIds` | guard |
|---|---|---|---|
| Decision | `src/projections/decision.ts:36` | `:64` | `:83-93` |
| Proposal | `src/projections/proposal.ts:30` | `:56` | `:75-85` |
| Task | `src/projections/task.ts:43` | `:70` | `:89-99` |
| Handoff | `src/projections/handoff.ts:25` | `:49` | `:68-78` |
| Quorum | `src/projections/quorum.ts:22` | `:48` | `:67-77` |

`BaseProjection`'s own copy covers ext-mode subclasses only — its docblock says as much at `src/projections/base.ts:136-141`. Any statement of the form "the guard in `BaseProjection` is what protects us" is off by five files.

More consequentially: **the guard protects projection state and nothing else.** `Participant.processMessage` applies the envelope to the projection (`src/agent/participant.ts:333-342`) and then unconditionally calls `await this.dispatcher.dispatch(message, ctx);` (`:344`) with **no `message_id` filter anywhere on that path**. On a replay-from-0, every historical envelope re-enters the agent's own handlers — an agent that votes in `onProposal` votes again. The phase-change and terminal dispatch below it (`:346-359`) *are* incidentally protected, because they are gated on `currentPhase !== this.lastPhase` and the deduped projection will not move its phase. So the blast radius is: projection state safe, handler dispatch unprotected. Issue #58 understates this; the plan must not repeat the understatement.

#### 4. Two docblocks in this repo contradict each other about what `seq` is

`src/client.ts:163-165` says of `afterSequence`:

> Clients derive the ordinal by counting delivered envelopes (the Nth accepted envelope has ordinal N) — see `IncomingMessage.seq`, **which is exactly this ordinal under the new contract.**

`src/agent/transports.ts:43-44` says the opposite:

> **Distinct from** `IncomingMessage.seq`, which is a client-local 0-based delivery index.

`transports.ts` is correct. `seq` is `this.seq++` from an initial `0` (`src/agent/transports.ts:29`, assigned at `:65`), so it is 0-based *and* counts raw deliveries; the ordinal is 1-based and counts distinct accepted envelopes. `client.ts`'s claim is false on both axes. `docs/guides/streaming.md:74` and `docs/api/client.md:405-406` inherit the same "counting delivered envelopes" imprecision.

#### 5. The runtime side, verified rather than assumed

- `after_sequence` is the 1-based ordinal of accepted session-scoped envelopes, **exclusive**, `0` = from the first (`RFC-MACP-0006-transport-bindings.md:116`, `:118`). Ordinals are stable across restart, migration, and compaction (`:121-124`).
- A resume below the compacted base is `FAILED_PRECONDITION` (`:125`). The runtime implements exactly that: `macp-runtime/src/server.rs:512-518` maps a `get_session_envelopes_after` miss to `Status::failed_precondition("session history before ordinal {base} was compacted; resume with after_sequence >= {base} …")`.
- `docs/api/client.md:408` claims "envelopes accepted during the subscribe window are never delivered twice." **This is true**, and I checked rather than trusting it: the runtime subscribes the broadcast receiver *before* snapshotting history (`macp-runtime/src/server.rs:498-500`, then `:509-512`) and deliberately dedups the overlap — `replay_dedup` at `src/server.rs:559-566`, applied at `:627-629` and `:639-641`. So within a single subscribe, the SDK's counter is not exposed to that particular duplicate source. It remains exposed to reconnect replay, which §3.2 `:130-131` calls out as its own mechanism.

#### 6. Fixture vendoring: the CI break is real and unbuffered

`.github/workflows/conformance-fixtures.yml:29-35` checks out `multiagentcoordinationprotocol/multiagentcoordinationprotocol` **with no `ref:`** — i.e. the default branch at whatever it is when the job runs — into `_spec`, then runs `make verify-fixtures SPEC_CONFORMANCE_DIR=…/_spec/schemas/conformance`. `verify-fixtures` (`Makefile:46-89`) diffs bidirectionally: every canonical `*.json` must exist byte-identically under `tests/conformance/` (`:56-62`), and every local `*.json` must have a canonical source (`:63-69`). There is no pin and no allowlist. **The first push or PR after spec issues #84 / #81 merge fails this job**, on a change unrelated to the PR that trips it, with a `DRIFT:` line naming a file the author has never seen.

#### 7. Issues #59 and #60 both assert that *none* of the ten sites has a normative rule. Five of them do.

Issue #59: "These seven sites are list-keyed accumulators with **no cardinality rule attached in the RFCs at all**." Issue #60: "none of these **currently has a confirmed normative 'first/last stands' rule** behind it."

Reading the four mode RFCs plus RFC-MACP-0001 in full contradicts that for **five of the ten sites** — Proposal `Accept`, Task `TaskAccept`, both Handoff status writes, and the post-`Commitment` `Vote` phase flip all have explicit MUSTs, three of which the SDK currently violates in shipped code. Two more sit on a genuine ambiguity in RFC-MACP-0009, and only three are cleanly silent (and in a way that reads as deliberate). The full table with quotes is Phase 3's deliverable. This is the single most important correction in this document: the issues' shared premise — "we must not invent semantics, so nothing can be decided yet" — is right in spirit but wrong in fact for half the list, and treating all ten as undecided would leave three spec violations shipped.

---

## Phases

### Phase 1 — pass a resume cursor from `GrpcTransportAdapter` and make the cursor correct (issue #58)

**Status: TODO**

**Delivers.** `GrpcTransportAdapter` resumes a passive subscribe from its own position instead of replaying the whole session, and `lastSequence` becomes a value that is actually the RFC-MACP-0006 §3.2 ordinal rather than a raw delivery count. The dead-API condition described in issue #58 is closed from both ends.

**Depends on.** Nothing. Implement now.

**Files** (all paths inside `macp-sdk-typescript`):

- `src/agent/transports.ts` — `GrpcTransportAdapter` (`:27-76`): the subscribe call (`:57`), the counter (`:30`, `:61`), the `lastSequence` docblock (`:38-48`).
- `src/client.ts:163-165` — correct the false `IncomingMessage.seq` equivalence in `sendSubscribe`'s docblock. No behaviour change.
- `tests/unit/agent/transports.test.ts` — `:161-165` and `:186-187` currently assert `sendSubscribe` is called with **one** argument and carry a comment ("If we ever start passing a cursor, this assertion needs updating") that this phase makes true.
- `docs/guides/streaming.md:72`, `:74`; `docs/api/client.md:405-406`, `:413-414`; `README.md:370`.
- `CHANGELOG.md` — behaviour change under the release-please convention already used at `CHANGELOG.md:118`.

**Approach.**

*A single code path, not a first-subscribe/reconnect branch.* `sendSubscribe(this.sessionId, this.delivered)`. The adapter's counter is `0` before anything is delivered **by construction** (`src/agent/transports.ts:30`), and `afterSequence = 0` is normatively "replay from the session's first accepted envelope" (`RFC-MACP-0006-transport-bindings.md:118`). So the first subscribe and every subsequent one are the same expression, and the first-subscribe case is not a special case that could drift. This also matches the getter's own docblock, which already states "`0` before anything is delivered" (`src/agent/transports.ts:41`).

**Rejected:** an explicit `private firstSubscribe = true` (or `if (this.delivered === 0) … else …`). It adds a second piece of state that must agree with the counter, buys nothing the counter's zero-value does not already give, and creates a way for the two to disagree.

**Rejected:** passing `this.seq`. It is 0-based and counts raw deliveries (`:29`, `:65`) — wrong by one *and* wrong in kind.

*Count distinct envelopes, not deliveries.* Add a private `seenMessageIds: Set<string>` to the adapter and increment `delivered` only when `envelope.messageId` is non-empty and previously unseen; an envelope with an empty/absent `messageId` increments unconditionally (there is no identity to dedup on, and collapsing a feed of id-less envelopes to one would be strictly worse). This mirrors, deliberately, the empty-id carve-out the projections already make and document at `src/projections/base.ts:224-227`, so the two dedup sites read the same way. Without this half, the resume cursor is the exact anti-pattern RFC-MACP-0006 §3.2 `:134` names, and the phase would ship a regression dressed as a fix.

Unbounded set growth is acceptable here for the same reason the projections give at `src/projections/base.ts:120-129`: sessions are TTL-bounded by protocol, and this adapter's own `IncomingMessage` objects already retain full envelope payloads downstream. A set of id strings is strictly dominated.

*The envelope is still yielded.* Deduping the **counter** is not the same as suppressing the **delivery**. The adapter continues to yield every envelope it receives; the projections' guards decide what reaches derived state. This keeps the phase additive, as the brief requires, and keeps `GrpcTransportAdapter` behaviourally consistent with `HttpTransportAdapter`, whose comment at `src/agent/transports.ts:120-127` explicitly delegates redelivery protection to the projection guard. Suppressing at the adapter would be the natural home for a fix to the *dispatcher* gap (Context §3) but is a behaviour change to what handlers observe and belongs to its own decision — see Open questions.

*Do not weaken the dedup guard.* This phase touches no file under `src/projections/`. Reviewers should be able to confirm that from the diff alone.

*`FAILED_PRECONDITION` is recognised, not recovered from, in this phase.* A resume below a compacted base arrives as a stream error: `MacpStream`'s `call.on('error')` handler (`src/client.ts:130-132`) wraps it as `MacpTransportError(details, 'FAILED_PRECONDITION')` and `responses()` rethrows it (`:186`), so it propagates out of `start()`'s `for await`, out of `Participant.run()`'s uncaught `try/finally` (`src/agent/participant.ts:279-285`), to the caller. `MacpTransportError.code` already carries the status name and `src/errors.ts:15-17` already documents this exact case. **The adapter must not auto-recover by re-subscribing from 0**, because a full re-replay would re-fire every agent handler through the unguarded `dispatcher.dispatch` at `src/agent/participant.ts:344`. Auto-recovery is only safe once dispatch is `message_id`-idempotent; that is a separate phase with a separate risk profile, listed under Long-term posture. What this phase owes the user is that the failure is legible and documented, which it is.

**Edge cases & failure modes.**

1. **Re-entrant `start()` without `stop()`.** `start()` currently assigns `this.stream = this.client.openStream(…)` unconditionally (`:51`), orphaning any prior stream while the earlier generator keeps iterating it. Two live generators would both feed one counter from two differently-positioned replays. The fix makes this more consequential, so `start()` should close any previously-held stream before opening a new one. Cheap, and it makes `stop()`-then-`start()` and bare re-`start()` behave identically.
2. **Empty or absent `messageId`.** Covered above: increments unconditionally, never dedups. A test must pin this, or a future "tidy-up" will collapse an id-less feed to one envelope.
3. **Cross-session envelopes.** The `envelope.sessionId !== this.sessionId` filter (`:60`) already runs before the counter. Envelopes for another session must not enter `seenMessageIds` either — the set is per-session state.
4. **Resume against a runtime older than 0.5.0.** `docs/api/client.md:408-410` records that older runtimes compared `after_sequence` inclusively against a raw log index. Resuming with a non-zero cursor against such a runtime skips or repeats one envelope. This SDK's `package.json` version is `0.8.0` and the docs already scope the ordinal contract to "runtime ≥ 0.5.0"; the phase should not add a version negotiation, but the CHANGELOG entry must name the minimum.
5. **A cursor ahead of the accepted tail.** `tests/integration/runtime.test.ts:772-805` already pins that this yields nothing from history and then live traffic — no error. So an over-counting cursor fails *silently*, which is precisely why the counter fix cannot be deferred to a follow-up.
6. **Compaction mid-session.** Ordinals are stable across compaction (`RFC-MACP-0006-transport-bindings.md:121-124`), so a valid cursor stays valid; only a cursor below the discarded base fails, and it fails loudly (`:125`). No client-side arithmetic is needed.

**Acceptance criteria.**

A reviewer holding only this section, the diff, and the test output can check every one of these:

1. `src/agent/transports.ts` contains exactly one `sendSubscribe` call, and it passes two arguments, the second being the adapter's distinct-envelope counter.
2. There is no `firstSubscribe`/`isReconnect` boolean, and no `if`/`else` selecting between `0` and the cursor.
3. `git diff --stat` shows **no file under `src/projections/`**.
4. A unit test drives `start()` → consume N envelopes with distinct `messageId`s → `stop()` → `start()` again, and asserts the second `sendSubscribe` was called with `(sessionId, N)`.
5. A unit test delivers the same `messageId` twice within one `start()` and asserts `adapter.lastSequence === 1`, not `2`.
6. A unit test delivers two envelopes with `messageId: ''` and asserts `adapter.lastSequence === 2` — the empty-id carve-out is load-bearing and must be pinned.
7. `src/client.ts:163-165` no longer claims `IncomingMessage.seq` is the ordinal.
8. `npm run test:coverage` passes, including the `vitest.config.ts:40-45` thresholds (lines 93 / branches 83 / functions 90 / statements 92). New branches added here (the empty-id carve-out, the seen-check) are branch-coverage-relevant; criteria 5 and 6 exist partly to keep the branch floor met.
9. `npm run check && npm run lint && npm run format:check && npm test` all pass.
10. The CHANGELOG entry describes it as a behaviour change ("a reconnecting `GrpcTransportAdapter` no longer replays the whole session"), not a fix — someone relying on full re-replay to rebuild a *fresh* projection on a *reused* adapter would see less history.

**Tests.**

Extend `tests/unit/agent/transports.test.ts`. Its `makeMockStream` helper (`:22-29`) already returns a `vi.fn()` `sendSubscribe`, so cursor assertions need no new scaffolding; `makeEnvelope` (`:8-20`) already takes a `messageId` override.

- *Happy resume*: as acceptance criterion 4. Must assert on `sendSubscribe.mock.calls[1]`, not just call count.
- *First subscribe is still a full replay*: `sendSubscribe.mock.calls[0]` equals `(sessionId, 0)`. This replaces the existing single-argument assertions at `:165` and `:187` and their stale comment at `:162-164`.
- *Redelivery does not advance the cursor* (criterion 5) — the direct §3.2 `:134` obligation. Failure path: assert `lastSequence` is 1 after two deliveries of `msg-1`, and that a subsequent `start()` subscribes at 1, not 2.
- *Empty `messageId` never dedups* (criterion 6). Failure path.
- *Cross-session envelope affects neither counter nor set*: reuse the existing three-envelope fixture at `:32-60` (two for `session-1`, one for `session-2`) and assert `lastSequence === 2`.
- *Re-entrant `start()` closes the prior stream*: two `openStream` calls, `close()` called once on the first mock before the second subscribe. Failure path for edge case 1.
- *`FAILED_PRECONDITION` propagates unchanged*: a mock whose `responses()` throws `new MacpTransportError('…compacted…', 'FAILED_PRECONDITION')`; assert `start()` rejects with that error and `code === 'FAILED_PRECONDITION'`, and that the adapter did **not** issue a second `sendSubscribe`. This pins "recognised, not recovered from" so a later well-meaning auto-retry has to delete a test to land.
- *Integration*: `tests/integration/runtime.test.ts` already exercises passive subscribe against a real runtime at `:717-806`. Add one case driving `GrpcTransportAdapter` itself through start → stop → start against a live session and asserting the second pass yields only envelopes accepted after the first pass ended. This is the only test that proves the ordinal the SDK computes agrees with the ordinal the runtime assigns; the unit tests all use a mock and cannot.

**Docs.**

- `src/agent/transports.ts:38-48` — the getter is no longer describing a hypothetical call site. State that the adapter passes it itself, and that it counts **distinct** envelopes.
- `src/client.ts:163-165` — delete the `IncomingMessage.seq` equivalence.
- `docs/guides/streaming.md:74` and `docs/api/client.md:405-406` — "counting delivered envelopes" → "counting **distinct** delivered envelopes (keyed on `message_id`)", citing §3.2 `:134`.
- `docs/guides/streaming.md:72`, `docs/api/client.md:413-414`, `README.md:370` — "calls this automatically" is still true but incomplete; say it now resumes from its own cursor on a second `start()`.
- `docs/guides/agent-framework.md:249-254` (`GrpcTransportAdapter (default)`) — add the resume behaviour and the explicit statement that the adapter does not reconnect on its own.
- `CHANGELOG.md` — as acceptance criterion 10.

---

### Phase 2 — vendor the new canonical fixtures for spec issues #84 and #81

**Status: TODO — BLOCKED on upstream. Sequence only; do not start.**

**Delivers.** `tests/conformance/` back in sync with the canonical corpus once the new fixtures land upstream, `make verify-fixtures` green, and every new fixture replaying green through the harness. CI stops being red.

**Depends on.** Spec repo issues #84 (duplicate-`Vote` / duplicate-ballot reject paths) and #81 (`Objection` / `Withdraw` / `TaskUpdate`, currently zero fixtures) merging into `multiagentcoordinationprotocol`'s default branch. **This phase cannot be started early and cannot be pre-empted:** `Makefile:63-69` fails any local `tests/conformance/*.json` that has no canonical source, so hand-writing the expected fixtures would fail this repo's own CI as `EXTRA:` rather than pass it.

**Trigger.** `.github/workflows/conformance-fixtures.yml:29-35` pulls the spec repo at its default branch with no `ref` pin. The break lands on the first push/PR after the upstream merge, in a job the triggering PR did not touch.

**Files** (all inside `macp-sdk-typescript`):

- `tests/conformance/*.json` — new and updated fixtures, written **only** by `make sync-fixtures`.
- `tests/conformance/conformance.test.ts` — only if one of the harness gaps below actually bites.
- `src/proto-registry.ts` — only if a new fixture uses a payload the registry does not map.
- `CHANGELOG.md`.

**Approach.**

Run `make sync-fixtures` (`Makefile:20-39`), review `git diff tests/conformance/`, run `npm test`, commit. That is the whole intended mechanism, and it is the *only* permitted one — `Makefile:44-45` says so and the bidirectional gate enforces it. Everything below is about what to check before assuming that is enough.

**Verified in advance — four questions the brief asked, answered from the code.**

**(a) Will `duplicateAcceptedBallots` tolerate the new #84 reject-path fixtures? Yes, unchanged.** `tests/conformance/duplicate-ballots.ts:117` is `if (msg.expect !== 'accept') continue;` — the first statement of the loop body, before any bucketing. A fixture holding an accepted first `Vote`/ballot followed by a **rejected** duplicate contributes exactly one entry to `seen` and zero to `duplicates`. The guard needs **no change**. The module docblock at `:100-106` says it was scoped this way precisely so the missing fixture could land, and the code matches the comment — which is not something to take on trust, so: `:117` is the line.

The cross-type Quorum case #84 asks for (a `Reject` after an accepted `Approve`) is also safe, and for a non-obvious reason: the ballot arm is gated on `payload_type.startsWith('macp.modes.quorum.v1.')` (`:124`, prefix at `:89`), not on the bare message-type name, so a Quorum `Reject` cannot be confused with the Proposal `Reject` that already lives in `proposal_negative_outcome.json`. The long comment at `:46-87` explains the collision; the guard genuinely implements what it describes.

**(b) Can the harness replay a fixture message with `"expect": "reject"` and an `expected_error_code` at all? It can *carry* one; it cannot *exercise* one.** `tests/conformance/conformance.test.ts:244` filters replay to `m.expect === 'accept'`, so a reject message never reaches a projection. What the harness does assert about rejects is a fixture-side contract, at `:379-401`: every reject message must have a truthy `expected_error_code`, that code must be in `CANONICAL_ERROR_CODES` (`:118-135`), and its `payload_type` must still resolve through `resolvePayloadType` (`:398`). The runtime-behaviour half is a deliberate, *visible* `it.skip` at `:402-407` pointing at `macp-runtime`'s conformance oracle. So #84's fixtures will be **gated but not executed** here — which is correct for an in-process projection harness, and worth stating plainly rather than letting a reader assume the SDK now tests duplicate-vote rejection.

`INVALID_ENVELOPE`, the code #84 proposes, is already in the canonical set (`:120`, imported from `src/constants.ts`). No change needed.

**(c) Are `Objection` / `Withdraw` / `TaskUpdate` payloads decodable by this SDK today? Yes — all three, end to end.** Registry mappings at `src/proto-registry.ts:20` (`macp.modes.decision.v1.ObjectionPayload`), `:28` (`…proposal.v1.WithdrawPayload`), `:34` (`…task.v1.TaskUpdatePayload`). Projection handlers at `src/projections/decision.ts:113-117`, `src/projections/proposal.ts:139-144`, `src/projections/task.ts:140-149`. Send paths at `src/decision.ts:142-151`, `src/proposal.ts:158-166`, `src/task.ts:142-150`.

The one encoding hazard I checked and cleared: `TaskUpdatePayload.partial_output` is a proto `bytes` field, and fixture JSON supplies bytes as plain strings — but `partialOutput` is already in the harness's `BYTES_FIELDS` set (`tests/conformance/conformance.test.ts:179`), so `normalizePayload` (`:183-193`) coerces it. `ObjectionPayload` (`proposal_id`, `reason`, `severity`) and `WithdrawPayload` (`proposal_id`, `reason`) are all-string. No harness change needed for any of the three.

**(d) Two real harness gaps the new fixtures could hit.** Neither is hypothetical; both are stated so the phase does not discover them as a red CI run.

- **`expected_mode_state` is only partially asserted.** The harness checks `phase` (`:300-302`) and `votes` (`:305-313`) and **nothing else**. A #81 fixture carrying `expected_mode_state.objections` or `.updates` would be silently unasserted — the fixture replays, the test passes, and the cross-implementation oracle issue #81 exists to create does not actually exist here. If the landed fixtures carry such keys, extending the harness is **in scope for this phase**, not a follow-up; a green run that asserts nothing is worse than a red one.
- **`CORE_MAP` is narrow.** `src/proto-registry.ts:5-10` maps only `SessionStart`, `Commitment`, `Signal`, `Progress`. The canonical `payload_type` pattern admits `SessionCancel` / `SessionSuspend` / `SessionResume` as `macp.v1.*` core payloads (spelled out at `tests/conformance/duplicate-ballots.ts:76-79`), and `encodeKnownPayload` throws `unknown payload mapping for …` on an unmapped type (`src/proto-registry.ts:123`). No current fixture uses one — I enumerated every `payload_type` across all 17 fixtures and the only core payload present is `macp.v1.CommitmentPayload` — but a new lifecycle-flavoured fixture would fail as a thrown error inside a `it()`, not as a helpful diff.

**Edge cases & failure modes.**

1. **A fixture for a mode not in `MODE_PROJECTIONS`.** Handled well already: `conformance.test.ts:232-240` registers a deliberately failing `it()` rather than silently contributing zero assertions. All six known modes are mapped (`:106-113`).
2. **`schema.json` changes upstream alongside the fixtures.** `sync-fixtures` copies it (`Makefile:29-33` globs `*.json`) and `verify-fixtures` diffs it, while the harness excludes it from the fixture list (`conformance.test.ts:200-202`). If the schema gains a field the harness's `Fixture`/`FixtureMessage` interfaces (`:39-70`) do not model, TypeScript will not complain — extra JSON keys are simply ignored. Read the schema diff, do not skim it.
3. **`sync-fixtures` copies but never deletes.** `Makefile:86` says so explicitly. If upstream *renames* a fixture, the old file survives locally and `verify-fixtures` reports it as `EXTRA:`, needing a manual `git rm`.
4. **The upstream fixture linter is a second gate.** `.github/workflows/conformance-fixtures.yml:41` runs `_spec/schemas/conformance/lint_fixtures.py` against the canonical tree. A canonical-side inconsistency fails this repo's CI with an error that is not this repo's to fix; the response is an upstream issue, never a local edit.
5. **The `Reject` name collision, again.** `#84`'s Quorum arm adds another accepted Quorum `Reject` to the corpus. Nothing in the guard breaks (see (a)), but any *new* cross-fixture guard written in this phase must key on `payload_type`, not `message_type`, for the reason documented at `duplicate-ballots.ts:46-87`.
6. **Both upstream issues may land in separate PRs.** Sync twice rather than waiting; each sync is independently green-able and a smaller diff to review.

**Acceptance criteria.**

1. `make verify-fixtures` exits 0 against a fresh checkout of the spec repo's default branch, printing "All conformance fixtures and cmt-hash vectors match the canonical source."
2. `git diff` for `tests/conformance/*.json` contains **only** content byte-identical to `schemas/conformance/` — verifiable by re-running `make sync-fixtures` and getting an empty diff.
3. `npm test` passes with every new fixture visibly named in the vitest output under all four `describe` blocks that apply to it (`projection replay`, `no duplicate accepted vote or ballot`, `fixture format guard`, and — for the #84 fixtures — `reject-path fixtures`).
4. `tests/conformance/duplicate-ballots.ts` is **unmodified**, unless the diff includes a written justification for why the verified answer in (a) turned out wrong.
5. For each of `Objection`, `Withdraw`, `TaskUpdate`: at least one assertion in the run demonstrably depends on that message type — not merely a longer transcript. If the landed fixtures do not carry assertable `expected_mode_state`, the harness gains the assertion in this phase and criterion 3 covers it.
6. `npm run test:coverage` still meets `vitest.config.ts:40-45`.

**Tests.** The fixtures *are* the tests. Beyond running them:

- If harness gap (d)-1 bites: assert the new `expected_mode_state` keys against the projections' own accessors — `DecisionProjection.objections` (`src/projections/decision.ts:39`) and `hasBlockingObjection` (`:216-223`), `TaskProjection.updates` (`src/projections/task.ts:45`), `ProposalProjection.liveProposals()` (`src/projections/proposal.ts:192-199`) for a withdrawal. `hasBlockingObjection` is the specific predicate issue #81 argues has no cross-implementation oracle; wiring it to a canonical fixture is the whole point of vendoring them.
- If harness gap (d)-2 bites: extend `CORE_MAP` (`src/proto-registry.ts:5-10`) and add a `tests/unit/proto-registry.test.ts` case per added type.
- Failure path to keep: the `tests/unit/fixture-drift-gate.test.ts` suite already drives the real Makefile recipes against synthetic trees and must stay green — it is what proves the gate itself still gates.

**Docs.**

- `CHANGELOG.md` — one line noting the corpus grew and which message types gained coverage. Not a feature.
- `docs/guides/testing.md` — if it enumerates fixture coverage, update it; otherwise no change.
- Nothing else. Vendored fixtures are not an API surface.

---

### Phase 3 — triage the ten cardinality / ordering sites (issues #59 and #60). **No code.**

**Status: TODO**

**Delivers.** For each of the ten sites: the governing RFC section, a verdict, and what the SDK should do — with the "rule absent" cases turning into proposed upstream spec issues rather than invented SDK semantics. The deliverable is the table below plus the follow-up list; **this phase changes no `src/` file.**

**Depends on.** Nothing. It is pure reading and is done — the table is the output, not a promise to produce one.

**Files.** None in `src/`. Output is issue text (comments on #59 / #60, and new spec-repo issues).

**Approach.** Read the four mode RFCs and RFC-MACP-0001's session state machine in full; quote before concluding; where the RFC is silent, say silent and propose a spec question rather than choosing a rule. Three sites turned out to be silent *coherently* (the silence is explained by adjacent text), and those need no spec issue at all — distinguishing "silent" from "silent and probably a gap" is the part that keeps this from generating noise upstream.

**Verified against, in binding order:** `RFC-MACP-0001-core.md` (session state machine), `RFC-MACP-0007-decision-mode.md`, `RFC-MACP-0008-proposal-mode.md`, `RFC-MACP-0009-task-mode.md`, `RFC-MACP-0010-handoff-mode.md`, with `RFC-MACP-0006-transport-bindings.md` §3.2 as the cross-cutting client obligation and `RFC-MACP-0012-policy.md` for the policy knobs. All line numbers are in the spec checkout at `110add2`.

#### The cross-cutting rule that applies to all seven of #59's sites

`RFC-MACP-0006-transport-bindings.md:136`:

> 3. A consumer that accumulates state per envelope — appending to a list, incrementing a counter — MUST be idempotent with respect to `message_id`. Re-applying a redelivered envelope MUST NOT change derived state.

This is normative **on the client** (`:138`: "The requirements above are the client-side counterpart"), and it is already satisfied at all seven sites by the per-projection `seenMessageIds` guards listed in Context §3. It settles the *redelivery* half. It says nothing about two **distinct** envelopes (distinct `message_id`s) carrying the same logical content — by §3.2's own framing at `:132` ("A redelivered envelope is **the same message, not a new one**"), those are two records, and the question moves to the mode RFCs. That is what #59 is actually asking, and the framing is correct.

#### The table

| # | Site (`file:line`) | Governing RFC § | Verdict | What the SDK should do |
|---|---|---|---|---|
| 1 | Decision `Evaluation` push — `src/projections/decision.ts:110` | RFC-0007 §4 matrix `:37`; §5 rules `:78-79` | **RULE ABSENT — coherently** | Nothing. Keep appending. |
| 2 | Decision `Objection` push — `src/projections/decision.ts:115` | RFC-0007 §4 matrix `:38`; §5 `:78-79`; RFC-0012 `:81` | **RULE ABSENT — coherently** | Nothing. Keep appending. |
| 3 | Proposal `Accept` push — `src/projections/proposal.ts:120` | RFC-0008 §5 rule 5 `:70`; §7 `:89` | **RULE EXISTS** | **Change.** A later `Accept` from the same sender supersedes their earlier one. |
| 4 | Proposal `Reject` push — `src/projections/proposal.ts:126` | RFC-0008 §5 rule 3 `:68`; §4 `:57` | **RULE ABSENT** | Keep appending. Propose a spec question (low priority). |
| 5 | Task `TaskUpdate` push — `src/projections/task.ts:142` | RFC-0009 §5 rule 4 `:73`; §4 `:58` | **RULE ABSENT — multiples are the expected case** | Nothing. Keep appending. |
| 6 | Task `TaskComplete` push — `src/projections/task.ts:152` | RFC-0009 §5 rule 5 `:74`; §8 `:106` | **AMBIGUOUS** | No SDK change yet. **Propose a spec issue.** |
| 7 | Task `TaskFail` push — `src/projections/task.ts:169` | RFC-0009 §5 rule 5 `:74`; §8 `:106` | **AMBIGUOUS** | Same issue as #6. |
| 8 | `TaskAccept` overwrites `assignee` — `src/projections/task.ts:125-129` | RFC-0009 §5 rules 3/3a/3b/3c `:69-72`; RFC-0012 `:135` | **RULE EXISTS** | **Change.** First accept wins; only the §5 rule 3c path may reassign. |
| 9 | Handoff accept/decline overwrite `status` — `src/projections/handoff.ts:113`, `:127` | RFC-0010 §5 rule 4 `:68`, rule 3a `:67`; §5.1(4) `:113-116` | **RULE EXISTS** | **Change.** Accept/decline settles a `handoff_id`; ignore a later contradictory one. |
| 10 | `Vote` from a fresh sender flips `phase` back to `'Voting'` — `src/projections/decision.ts:142` | RFC-0001 §7.2 `:216`, §7.3 step 5 `:238`, `:247`; RFC-0007 §6 `:85` | **RULE EXISTS** | **Change.** Never regress `phase` out of `'Committed'`. |

#### Supporting quotes and reasoning

**Sites 1 & 2 — Decision `Evaluation` / `Objection`. Rule absent, and the absence is explained.**
RFC-0007's authority matrix attaches a cardinality parenthetical to exactly one row — `:39`: "`Vote` | Any declared participant (**at most one per proposal per participant in base v1**)" — against the unqualified `:37` "`Evaluation` | Any declared participant" and `:38` "`Objection` | Any declared participant". The §5 validation rules constrain only reference integrity for these two (`:78`: "`Evaluation`, `Objection`, and `Vote` MUST reference an existing `proposal_id`") while spelling out the `Vote` cap in full at `:79`. And RFC-0012 `:81` gives `objection_handling` a **`veto_threshold`**, a count that only means something if multiple objections can exist. The silence is a design choice, not a gap. **No spec issue. No SDK change.** Issue #59 is right that there is no rule and wrong to imply that is a problem for these two.

**Site 3 — Proposal `Accept`. Rule exists, and the SDK currently contradicts it.**
`RFC-MACP-0008-proposal-mode.md:70`:

> 5. A participant MAY change its acceptance target by sending a later `Accept` for a different live proposal. **The latest accepted `Accept` from a participant supersedes earlier accepts from the same participant.**

`src/projections/proposal.ts:120` pushes unconditionally, so a superseded accept stays in the list, and two derived accessors read wrong as a direct consequence:
- `isAccepted(proposalId)` (`:185-187`) returns `true` for a proposal whose only accept was superseded.
- `acceptedProposal()` (`:201-207`) builds `new Set(this.accepts.map(a => a.proposalId))` and returns `undefined` whenever `size !== 1`. A participant who accepts `p1` and then re-accepts `p2` makes it return `undefined` where the acceptance set is unambiguously `{p2}`.

That is not a style preference; §7's determinism clause makes it a conformance failure — `:89`: "Given the same accepted history and the same version-bound rules, implementations MUST derive the same **live proposal set, the same acceptance set**, and the same commitment eligibility." **This contradicts issue #59's premise directly**, and it is the reason Phase 3 exists as triage rather than as a bulk "add dedup everywhere" change.

**Site 4 — Proposal `Reject`. Rule absent.**
Only `:68` (reference integrity, shared with `Accept`/`Withdraw`) and `:57` ("**Reject** - rejects a specific proposal and MAY mark the rejection as terminal"). §5 rule 5's supersession sentence is scoped to accepts by its own wording. Keep appending; the asymmetry (accepts supersede, rejects apparently accumulate) is worth an upstream question but is low-stakes — nothing in the SDK's derived state depends on reject cardinality except `hasTerminalRejection()` (`:208-210`), which is a `.some()` and is order- and count-insensitive.

**Site 5 — Task `TaskUpdate`. Rule absent; multiples are the point.**
`RFC-MACP-0009-task-mode.md:58` describes it as "non-terminal progress or status update" and the only constraint in §5 is authorship — `:73`: "`TaskUpdate`, `TaskComplete`, and `TaskFail` MUST come from the active assignee." A progress stream is the intended shape. **No change.**

**Sites 6 & 7 — `TaskComplete` / `TaskFail`. Genuinely ambiguous; this is the one worth escalating.**
`:74`: "5. `TaskComplete` and `TaskFail` do not resolve the Session on their own. They make the Session eligible for `Commitment` by the requester or policy authority." So the session stays `OPEN`, and further completion messages are *not* barred by the terminal rule. Meanwhile §8 `:106` defines the side-effect idempotency key as `task_id + assignee + **first_accepted_completion_message_id**` — the word "first" presupposes there can be a second, and designates the first as authoritative for side effects.

What is undefined: whether a second `TaskComplete`, or a `TaskFail` after a `TaskComplete`, is valid at all, and if it is, what derived state should read. The SDK currently lets whichever arrives last win the task `status` (`src/projections/task.ts:155-156` sets `'completed'`, `:171` sets `'failed'`) and the `phase` (`:157`, `:172`) — a coin flip on message order, with no rule to appeal to. **Proposed upstream spec issue:** *"RFC-MACP-0009 §5 rule 5 and §8 together imply a second completion message is possible but do not say whether it is valid or which one governs derived state; §8's `first_accepted_completion_message_id` suggests first-wins — should that be stated normatively in §5, as §5 rule 3a does for `TaskAccept`?"* No SDK change until it is answered — that is the whole discipline these issues are asking for, and it applies here and only here among the ten.

**Site 8 — `TaskAccept` reassignment. Rule exists, in four consecutive clauses.**
`RFC-MACP-0009-task-mode.md:69-72`:

> 3. Only one assignee may become active for the Session in base v1.
> 3a. The first accepted `TaskAccept` from any eligible participant designates that participant as the active assignee. **Subsequent `TaskAccept` messages for the same session MUST be rejected if an active assignee is already designated.**
> 3b. A participant who has sent `TaskAccept` MUST NOT later send `TaskReject` for the same task in base v1. `TaskAccept` is irrevocable unless policy explicitly permits reassignment.
> 3c. When policy sets `allow_reassignment_on_reject: true` and the active assignee sends `TaskReject`, the session returns to the pre-assignment state. Other eligible participants MAY then send `TaskAccept` for the same `task_id`.

The SDK's `task.assignee = record.assignee` at `src/projections/task.ts:128` is unconditional. Correct behaviour is **first-accept-wins**, with exactly one legal reassignment path (3c: policy `allow_reassignment_on_reject` — confirmed present at `RFC-MACP-0012-policy.md:135` — *and* a `TaskReject` from the **active assignee**, which must first clear the assignee). Note the SDK's `TaskReject` handler (`src/projections/task.ts:135-139`) sets `status = 'rejected'` but never clears `assignee`, so even the legal path is not modelled. This is a two-part change and it needs the policy value, which the projection does not currently receive — worth flagging before anyone estimates it as a one-liner. **Contradicts issue #60's premise.**

**Site 9 — Handoff accept/decline. Rule exists.**
`RFC-MACP-0010-handoff-mode.md:68`: "4. Once an offer has been accepted, no competing accept for that same `handoff_id` is valid." The decline-after-accept direction is settled in §5.1's synthetic-accept race analysis, `:113-116`:

> 4. **Races resolve by history order.** An explicit `HandoffAccept` or `HandoffDecline` accepted into history before the synthetic accept **settles the offer**, and the synthetic accept is then never emitted. Once the synthetic accept is in history, **a later decline is invalid (rule 4)**.

"Settles" is the operative word: a `handoff_id` transitions `offered → accepted | declined` **once**. The unconditional writes at `src/projections/handoff.ts:113` and `:127` are wrong in both directions. Note the projection already gets this right one case up — the `HandoffContext` handler at `:100-102` guards with `if (handoff.status === 'offered')` and comments "Only update status if not already accepted/declined." The correct shape is already in the file, three lines above the first violation. **Contradicts issue #60's premise.**

**Site 10 — `Vote` after `Commitment`. Rule exists, in Core, and it is stronger than "don't flip the phase".**
Decision Mode resolves on `Commitment` (`RFC-MACP-0007-decision-mode.md:85`: "Decision Mode resolves when an authorized `Commitment` is accepted"), which puts the session in `RESOLVED`. Then `RFC-MACP-0001-core.md:216`:

> `RESOLVED`, `EXPIRED`, and `CANCELLED` are terminal. Sessions MUST transition **monotonically** with respect to termination: once a session is terminal, no further transition is permitted (in particular, no transition back to `OPEN` or `SUSPENDED`).

and §7.3's ordered terminal procedure, `:238`: "5. Reject any subsequent session-scoped messages for this session with a non-OPEN session error", restated at `:247`: "Any session-scoped message referencing a non-OPEN session MUST be rejected."

So a `Vote` after a `Commitment` **cannot exist in accepted history at all** — this is not a "late arrival" to be ordered, it is a message a conforming runtime rejects. Given the projections' accepted-only input contract (`src/projections/base.ts:174-215`), the SDK reaching `src/projections/decision.ts:142` in that state means the caller violated the contract. Two defensible responses: refuse to regress `phase` out of `'Committed'` (minimal), or additionally record it as a `ProjectionAnomaly`.

The second is more informative but is **blocked on a cross-SDK decision**: `ProjectionAnomalyKind` is a frozen union of `'duplicate_vote' | 'duplicate_ballot'` (`src/projections/base.ts:11`) with a compile-time guard on the *field* set (`:92-95`) and an explicit "do not add a kind without cross-SDK agreement" (`:8-9`). A `'post_commitment_message'` kind requires agreement with `macp-sdk-python` first. See Open questions.

One caveat to carry into any fix: **"phase" is not a normative MACP concept.** It appears once across all RFCs, in passing, at `RFC-MACP-0012-policy.md:211`. The constraint on the SDK's `phase` field is *derived* from the session state machine, not read off a phase specification — so the change must be justified as "monotonic terminality, per RFC-0001 `:216`", never as "the phase spec says so."

#### Follow-up work this triage produces (each its own issue/phase, none in scope here)

| Output | Where it goes |
|---|---|
| Proposal `Accept` supersession (site 3) | SDK change, own phase. Touches `src/projections/proposal.ts:120`, `:185-187`, `:201-207`. |
| `TaskAccept` first-accept-wins + `TaskReject` clearing `assignee` under policy (site 8) | SDK change, own phase. Needs the policy value plumbed into `TaskProjection`. |
| Handoff settle-once (site 9) | SDK change, own phase. Smallest of the three; the guard shape already exists at `src/projections/handoff.ts:100-102`. |
| Post-`Commitment` phase monotonicity (site 10) | SDK change, own phase. The anomaly-kind half is cross-SDK-blocked. |
| `TaskComplete`/`TaskFail` cardinality (sites 6, 7) | **Upstream spec issue.** No SDK change until answered. |
| `Reject` supersession asymmetry (site 4) | **Upstream spec question**, low priority. |
| Sites 1, 2, 5 | **Nothing.** Silence is correct and explained. Record the reasoning on issue #59 so it is not re-litigated. |

**Edge cases & failure modes.** The failure mode of *this* phase is inventing semantics. Two specific traps:
- Treating RFC-0006 §3.2 `:136` (`message_id` idempotence) as if it answered #59. It does not — it covers redelivery only, and the guards already satisfy it. Concluding "#59 is fixed" from that quote would close a real bug.
- Reading `hasBlockingObjection` (`src/projections/decision.ts:216-223`) — which cites "RFC-MACP-0004" in its comment at `:215` — as evidence of an objection cardinality rule. RFC-0004 is the identity/authorization RFC; the severity semantics are RFC-0007 `:38` plus RFC-0012 `:81`. The comment's citation is stale and should not be leaned on.

**Acceptance criteria.**

1. All ten rows carry a governing RFC **section number and line**, and every "RULE EXISTS" row carries a verbatim quote.
2. Every "RULE ABSENT" row is classified as *coherent silence* (no upstream action) or *gap* (upstream issue) — with the distinguishing evidence named. A bare "absent" is not an acceptable verdict.
3. **Zero files under `src/` are modified by this phase.** `git diff --stat` is empty for `src/`.
4. Issues #59 and #60 each receive a comment stating plainly that their shared "no confirmed normative rule" premise does not hold for five of the ten sites, with the list.
5. At most **two** new upstream spec issues are opened (sites 6+7 together; site 4 optionally). Ten issues would be noise; zero would be under-reporting.
6. Each "RULE EXISTS" row names the *specific accessor or assignment* that misbehaves, not just the mode — so the follow-up phase can be scoped without re-reading the RFC.

**Tests.** None. This phase produces no code. The follow-up phases it spawns each carry their own tests, and each should begin with a **failing** test derived from the RFC quote in its row — e.g. for site 3, a `ProposalProjection` fed accept(`p1`) then accept(`p2`) from one sender, asserting `acceptedProposal() === 'p2'` and `isAccepted('p1') === false`.

**Docs.**

- `docs/api/projections.md` — once the follow-up phases land, the accessors whose semantics change (`isAccepted`, `acceptedProposal`, task `assignee`, handoff `status`) need their contracts restated. Nothing to write in this phase.
- No `CHANGELOG.md` entry: nothing shipped.

---

## Long-term posture

**The dispatcher is not `message_id`-idempotent, and Phase 1 makes that the last remaining hole.** `src/agent/participant.ts:344` dispatches every delivered envelope to user handlers with no dedup. After Phase 1 the common case (a resume) no longer re-delivers history, which makes the hole *less often reached* and therefore *less likely to be noticed* — the classic shape of a bug that resurfaces at the worst time. It is also the specific thing blocking automatic `FAILED_PRECONDITION` recovery, since recovery means re-subscribing from the compacted base and re-delivering everything above it. Whoever adds retry/reconnect to this SDK must land dispatch idempotence in the same change or knowingly ship at-least-once handler invocation.

**Six copies of the same dedup guard.** The five built-in projections plus `BaseProjection` each carry their own `seenMessageIds` and their own copy of the empty-id carve-out. Phase 1 adds a seventh in the transport adapter. They are all correct today and they all cite the same RFC line, but a future change to the rule — say, a bounded set, or a decision to treat empty ids differently — has to land in seven places or silently diverge. Not worth refactoring now (the projections deliberately do not share a base class, per `src/projections/base.ts:136-141`), but worth a note so the seventh copy is a known cost rather than an accident.

**The unpinned spec checkout in `conformance-fixtures.yml` is a deliberate one-way door, and it is the right one.** Pinning a `ref` would make this repo's CI stop noticing upstream fixture changes — trading a loud, well-timed failure for silent staleness, which is exactly the failure class `make verify-fixtures` exists to prevent. The cost (Phase 2's break arrives on an unrelated PR) is the price of the guarantee. Do not "fix" it by pinning.

**`ProjectionAnomalyKind` is frozen across SDKs.** `src/projections/base.ts:11` and the compile-time guard at `:92-95` mean any future cardinality work that wants to *report* rather than merely *correct* — site 10, and potentially sites 3, 8, 9 — needs a `macp-sdk-python` conversation before it needs a TypeScript diff. Sequence that conversation early; it has a long lead time relative to the code.

**Three shipped spec violations (sites 3, 8, 9) are now documented but not fixed.** That is the correct outcome of a triage phase, but it does create a window in which the repo knows it is non-conforming and has not acted. The follow-up phases should be filed as issues immediately on Phase 3's completion, not held in this plan.

---

## Open questions

1. **Should `GrpcTransportAdapter` also *suppress* redelivered envelopes, not just decline to count them?**
   *Proposed default (Phase 1's approach): no — count-only.* Suppressing would fix the unguarded `dispatcher.dispatch` at `src/agent/participant.ts:344`, but it changes what user handlers observe, diverges `GrpcTransportAdapter` from `HttpTransportAdapter` (whose comment at `src/agent/transports.ts:120-127` explicitly delegates redelivery protection to the projection guard), and sets an expectation that third-party `TransportAdapter` implementations — a public extension point (`:6-9`) — cannot be held to. It is also not additive, and the brief scopes Phase 1 as additive. **Pending confirmation**; if the answer is "yes, suppress", it belongs in a dispatcher-idempotence phase alongside a decision about whether `IncomingMessage.seq` should then become the ordinal.

2. **Should `IncomingMessage.seq` be redefined as the 1-based ordinal, or should `client.ts:163-165` simply be corrected?**
   *Proposed default: correct the docblock.* `seq` is `number | undefined` on a public interface (`src/agent/types.ts:10`) and `Participant.processEvent` hard-codes `seq: 0` for manually-injected envelopes (`src/agent/participant.ts:304`), so redefining it would be a semver-relevant change to a field whose only in-repo consumer sets it to a constant. Correcting the prose costs nothing and removes the contradiction. **Pending confirmation** — the alternative (make `seq` the ordinal and delete `lastSequence`) is defensible and slightly simpler, but it is a breaking API change and `lastSequence`'s docblock was written for a reason.

3. **Genuine fork, no defensible default: what should the SDK do when it observes a `Vote` after a `Commitment` (site 10)?** The RFC settles that such a message cannot be in accepted history (`RFC-MACP-0001-core.md:216`, `:238`, `:247`) but says nothing about what a client that receives one anyway should *do*, because the RFC does not model client-side projections. Silently ignoring it hides a caller violating the accepted-only contract; recording a `ProjectionAnomaly` surfaces it but requires a new `ProjectionAnomalyKind`, which `src/projections/base.ts:8-9` freezes pending cross-SDK agreement with `macp-sdk-python`. This needs a human decision *and* a cross-repo conversation, and it should not block the minimal "do not regress `phase`" fix.

4. **Genuine fork: does the `TaskProjection` get access to the session's policy?** Site 8's only legal reassignment path is gated on `allow_reassignment_on_reject` (`RFC-MACP-0009-task-mode.md:72`, `RFC-MACP-0012-policy.md:135`), and `TaskProjection` currently has no policy input at all. The options — plumb the resolved `PolicyDefinition` into the projection constructor, assume the restrictive default (no reassignment) and under-model the 3c case, or model reassignment structurally from the `TaskReject`-then-`TaskAccept` sequence without consulting policy — differ in API surface, cross-SDK parity, and correctness, and no one of them is obviously right. Decide before scoping the site-8 follow-up, not during it.

5. **Will spec issues #84 and #81 land together or separately, and is anyone tracking their merge?** Phase 2's trigger is an upstream merge that nobody in this repo controls or is notified of. The pragmatic mitigation is a watch on those two issues rather than a CI change (see Long-term posture on why pinning is wrong). **Pending confirmation** that someone owns that watch — otherwise the first signal is a red CI run on an unrelated PR.
