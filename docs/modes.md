# Coordination Modes

This page documents the runtime's implementation of each coordination mode -- the internal state machines, phase progression rules, and implementation-specific behavior. For mode specifications, message types, authority matrices, and protocol-level semantics, see the [protocol modes documentation](https://www.multiagentcoordinationprotocol.io/docs/modes) and the individual mode RFCs.

## Mode-state records are sealed

Each mode keeps its working state in `session.mode_state`, a serialized blob whose Rust shape is a small set of `pub` records. Those records are **`#[non_exhaustive]` as of 0.8.0**, so a crate outside `macp-modes` can read and match their fields but cannot construct one by struct literal:

| Crate | Sealed records |
|---|---|
| `macp-modes` | `HandoffOfferRecord`, `HandoffContextRecord`, `HandoffState`, `ApprovalRequestRecord`, `BallotRecord`, `QuorumState` |
| `macp-storage` | `PersistedSession` |

This is a **breaking change for external callers** that built any of these by exhaustive struct literal, and it was taken deliberately. It is not, however, what makes 0.8.0 a major release: 0.8.0 was **already** forced to be one. These records grow a field whenever a mode learns something new, and with all-`pub` fields and no seal each addition is a `constructible_struct_adds_field` major break of its own. Against the published 0.7.6, `cargo semver-checks check-release --workspace` reports exactly two such breaks in this release -- `HandoffOfferRecord.suspended_ms_at_offer` and `PersistedSession.suspension_intervals` -- and neither is avoidable: they are the state the suspension-corrected implicit-accept deadline has to persist. Since `release-plz.toml` sets `semver_check = true` and all seven crates move in one `version_group`, either one alone blocks the release PR for the whole family.

Given a major was being spent regardless, it was spent once to end the class rather than twice on the same two fields: sealing the records makes every future mode-state field additive.

What still works unchanged: reading fields, mutating fields on a value you were handed, pattern-matching with `..`, `Default::default()` on `HandoffState` and `QuorumState`, and `PersistedSession::from(&Session)` followed by field assignment. What does not: `HandoffState { offers, contexts }` and its siblings, including the `{ ..Default::default() }` form.

One record has a supported replacement, because it is a parameter of a public API rather than pure serialization detail: build an `ApprovalRequestRecord` with `ApprovalRequestRecord::new(..)`, which keeps `QuorumMode::effective_threshold(&Session, &ApprovalRequestRecord)` callable from another crate. The rest are produced by the runtime from accepted envelopes and have no supported construction path.

The enums in the same modules -- `HandoffDisposition`, `BallotChoice`, `ApprovalThreshold` -- are deliberately **not** sealed. A `#[non_exhaustive]` enum forces a `_` arm in every external `match`, which would silently reinterpret a future variant as one of today's; for a governance bar that is the exact defect class issue #145 was. Adding a variant is already the major lint `enum_variant_added`, so the release PR catches it either way.

## Decision Mode

**Source**: `crates/macp-modes/src/mode/decision.rs` | **Identifier**: `macp.mode.decision.v1`

The decision mode tracks proposals, evaluations, objections, and votes through an automatic phase progression. Its internal state consists of:

- A map of proposals keyed by `proposal_id`
- Lists of evaluations and objections
- A nested map of votes keyed by `proposal_id` then by sender
- A phase indicator that advances automatically as the session progresses

**Phase progression** is automatic: the first `Evaluation` message moves the phase from Proposal to Evaluation, and the first `Vote` moves it to Voting. Once in the Voting phase, new proposals are no longer accepted. This progression is enforced by the runtime, not by agents.

**Value normalization**: Recommendation values, vote values, and severity levels are stored in a canonical form (uppercase for recommendations and votes, lowercase for severity) to ensure deterministic comparison during policy evaluation.

**Commitment readiness**: The runtime requires at least one proposal to exist before accepting a commitment. If governance policies are bound to the session, they impose additional requirements -- vote quorum, confidence thresholds, and veto rules -- that must also be satisfied.

**An empty `participants` list is accepted -- for Decision alone.** Every other standards-track mode refuses `SessionStart` with an empty roster (and Task and Handoff refuse more than that: Task needs a participant other than the initiator, Handoff needs two parties). RFC-MACP-0001 §7.1 requires `participants` only "when required by the Mode", and RFC-MACP-0007 makes the initiator's authority role-based rather than membership-based, so for Decision the roster and the authority model are independent. The resulting session is well-defined and **inert**: `Proposal`, `Evaluation`, `Objection` and `Vote` are authorized only for declared participants, so with none declared **every** such message is `FORBIDDEN` -- the initiator's included -- no proposal can ever exist, and commitment readiness can never be met. Such a session can only expire or be cancelled. The carve-out is enforced in one place (`macp_core::session`), not by the Decision mode itself.

## Proposal Mode

**Source**: `crates/macp-modes/src/mode/proposal.rs` | **Identifier**: `macp.mode.proposal.v1`

The proposal mode handles offer-and-counteroffer negotiation. Its internal state tracks live proposals, per-participant acceptance records, and any terminal rejections.

**Convergence detection** happens automatically after each message. The `refresh_phase()` method checks the session's acceptance criterion (configurable via policy as `all_parties`, `counterparty`, or `initiator`) and transitions the phase to Converged when the criterion is met. Convergence does not auto-resolve the session -- an explicit commitment is still required.

**Counter-proposal semantics**: A `CounterProposal` creates a new entry with its own `proposal_id`. The `supersedes_proposal_id` field is informational only -- the original proposal stays live and participants can accept either. Round limits are enforced at counter-proposal submission time, not just at commitment.

**Terminal rejection**: A `Reject` message with `terminal: true` immediately transitions the session phase to TerminalRejected, making the session eligible for a negative-outcome commitment.

## Task Mode

**Source**: `crates/macp-modes/src/mode/task.rs` | **Identifier**: `macp.mode.task.v1`

The task mode manages bounded work delegation. Its internal state tracks the task request, the currently active assignee, any rejection records, progress updates, and the terminal report (complete or fail).

**Assignment lifecycle**: After the initiator sends a `TaskRequest`, an eligible participant can accept with `TaskAccept`, which sets them as the active assignee. Only the active assignee can send `TaskUpdate` messages -- this is validated against the authenticated sender, not a payload field.

**Reassignment**: When the `allow_reassignment_on_reject` policy rule is enabled and the active assignee sends a `TaskReject`, the assignee is cleared. Other eligible participants can then send `TaskAccept` to take over the task.

**Terminal reports**: Either `TaskComplete` or `TaskFail` records the outcome, but neither resolves the session. An explicit commitment from the initiator is required to bind the result.

## Handoff Mode

**Source**: `crates/macp-modes/src/mode/handoff.rs` | **Identifier**: `macp.mode.handoff.v1`

The handoff mode manages responsibility transfer through serial offers. Its internal state tracks offers and their associated context messages.

**Serial offer constraint**: Only one outstanding (unresolved) offer may exist at a time. Once an offer is accepted, no further offers can be issued in that session.

**Late context**: `HandoffContext` messages are accepted even after the offer they reference has been accepted or declined. The protocol allows this as supplementary documentation -- additional context that may be useful to the accepting agent after the transfer.

### Implicit accept (RFC-MACP-0010 §5.1)

When the session's bound policy sets `acceptance.implicit_accept_timeout_ms` to a positive value, an outstanding offer that goes unanswered for that long is accepted **by the runtime**, on the target's behalf. This is a **deliberate semantics change**, not a bugfix, and it is gated on the session's `semantics_rev` -- see [Revision gating](#revision-gating) below.

At `semantics_rev >= 2` the accept is a **recorded event**, not an inference. The runtime appends a synthetic `HandoffAccept` envelope to accepted history:

| Field | Value |
|---|---|
| `sender` | the offer's `target_participant` |
| `message_type` | `HandoffAccept` |
| `message_id` | `implicit-accept:<handoff_id>` -- deterministic, not random |
| `payload.implicit` | `true` |
| `payload.reason` | `implicit accept (timeout)` |
| `timestamp_unix_ms` | the **computed deadline**, never the time the runtime noticed it |

Four consequences a client must be ready for:

- **It is ordinary accepted history.** The entry consumes an accepted ordinal and is published to `StreamSession` subscribers, so a client can receive a message that no participant sent. It also replays: rebuilding the session from the log produces the same entry at the same ordinal.
- **Suspended time does not count.** Time the session spends `SUSPENDED` is excluded from the timeout (§5.1(1)). The offer record snapshots the session's accumulated suspended time when the offer is made, and the deadline walk subtracts the pauses that fall inside the window -- not the pauses that began after it, which is why a `SuspendSession` issued after the deadline cannot move a timestamp already fixed.
- **It lands before the message that revealed it.** The deadline is observed on the background maintenance pass (see [Background maintenance](API.md#background-maintenance)) *and* on demand, immediately before the next session-scoped message is evaluated. §5.1(2) requires the synthetic entry to be in history before any later message is judged against the offer's acceptance state -- so a late explicit `HandoffAccept` (or `HandoffDecline`) meets an offer already in `Accepted` disposition and is refused with `INVALID_ENVELOPE`, rather than quietly succeeding against an offer the runtime had not got around to settling. This holds **even when that later message is itself rejected**: the synthetic entry was due independently of it. The trigger's own `message_id` is still not consumed, so re-sending a corrected message under the same id is accepted normally.
- **`participant_activity` does not move.** `SessionMetadata.participant_activity` reports the target's `message_count` and `last_seen` unchanged, deliberately. The runtime does not credit a participant with activity they did not perform, and replay never records activity for any entry kind -- crediting it live would fork the live session from what the log rebuilds.

**Clients may not forge one.** At `semantics_rev >= 2` two rules apply at the client boundary, before the mode sees the message:

1. any client envelope in the handoff session whose `message_id` starts with `implicit-accept:` is rejected with `INVALID_ENVELOPE` -- **every** message type, including `SessionStart`, `Commitment` and `HandoffContext`, because squatting the id a future offer would use would consume the runtime's own dedup slot and strand the session short of commitment. The match is case-sensitive; `Implicit-Accept:h1` is an ordinary client id and can never collide with the lowercase id the runtime builds;
2. a `HandoffAccept` whose payload decodes with `implicit = true` is rejected (§5.1(3)).

Both carry the wire code `INVALID_ENVELOPE`. The runtime distinguishes the two internally (`MacpError::InvalidEnvelope` vs. `MacpError::InvalidPayload`) but there is no distinct `INVALID_PAYLOAD` code in the RFC vocabulary -- both map to `INVALID_ENVELOPE`, so a client cannot tell them apart by code alone.

### Revision gating

Every session records the `semantics_rev` it was started under, and the runtime honors that revision for the session's whole life so an already-persisted history replays to the outcome it was accepted with (RFC-MACP-0003 §1). The revision is bound at `SessionStart`; there is no way to move an existing session forward.

| `semantics_rev` | Implicit-accept behavior |
|---|---|
| `0` | Inferred inside `Commitment` handling, timed against the client-supplied `Envelope.timestamp_unix_ms`. Suspended time counts. |
| `1` | Same inference, timed against the runtime's acceptance clock instead of the client's timestamp. Suspended time counts. |
| `2` (current) | The synthetic history entry described above. Suspended time is excluded. Client-submitted implicit accepts and reserved ids are refused. |

Sessions started by this release are rev 2. Sessions restored from a log written by an earlier release keep their recorded revision and continue to resolve through the interim in-`Commitment` path, with no synthetic entry and no reserved-id restriction. **Rollback is not a revert**: once a rev-2 session has written a synthetic accept, an older binary replays that history differently, so the recovery path for a bad 0.8.0 deployment is to roll forward.

## Quorum Mode

**Source**: `crates/macp-modes/src/mode/quorum.rs` | **Identifier**: `macp.mode.quorum.v1`

The quorum mode tracks approval requests and ballots against a threshold. Its internal state records the approval request and a map of ballots (approve, reject, or abstain) keyed by sender.

**Threshold resolution**: A governance policy's `threshold` rule *replaces* the `required_approvals` value from the `ApprovalRequest` payload rather than supplementing it (RFC-MACP-0011 §6). The arithmetic is ceiling-rounded with a floor of one approval, and lives in `QuorumThreshold::effective` (`macp-core`) -- the same function the policy evaluator calls, so the mode and the evaluator cannot derive two different bars from one policy.

**Reading the threshold from outside the runtime**: two public accessors on `QuorumMode` report the bar the runtime itself enforces, so a caller never has to re-derive it from policy rules and participant counts:

| Accessor | Returns |
|----------|---------|
| `QuorumMode::effective_threshold_for_session(&Session)` | `Result<Option<ApprovalThreshold>, MacpError>` |
| `QuorumMode::effective_threshold(&Session, &ApprovalRequestRecord)` | `ApprovalThreshold` |

Prefer the session-level form. The request-level form exists for a caller that already holds a record; build one with `ApprovalRequestRecord::new(request_id, action, summary, details, required_approvals, requested_by)` -- as of 0.8.0 the record is `#[non_exhaustive]` (see [Mode-state records are sealed](#mode-state-records-are-sealed)) and the struct-literal form no longer compiles outside `macp-modes`.

The session-level form decodes the accepted request out of `session.mode_state` itself. Each layer of its return type answers exactly one question:

- `Ok(Some(ApprovalThreshold::Approvals(n)))` -- `n` `Approve` ballots seal a positive commitment. Never zero; for a session whose request the mode accepted, never above the participant count. A session with no `threshold` rule (the common case) reports the payload's own `required_approvals` here, already resolved.
- `Ok(Some(ApprovalThreshold::Unsatisfiable))` -- the bound policy admits no positive commitment at any approval count (`threshold.type: "weighted"`, an unrecognised type, or a `percentage` over an empty participant set). `RegisterPolicy` refuses the first two, so reaching them requires a directly constructed `PolicyDefinition`; the third cannot be caught at registration, which has no participant count, and is blocked by `QuorumMode::on_session_start` rejecting an empty participant set instead. Such a session seals **neither** outcome.
- `Ok(None)` -- no `ApprovalRequest` has been accepted yet, so there is nothing to resolve. Deliberately distinct from `Unsatisfiable`: "not yet" and "never" are different answers.
- `Err(MacpError::InvalidModeState)` -- `session.mode_state` is not decodable quorum state, so no answer would be honest.

**Commitment readiness**: the runtime accepts a commitment when the approval threshold is met, or when it has become mathematically unreachable and at least one ballot has been cast -- the latter being RFC-MACP-0011 §4a's trigger for a *negative* commitment:

```text
approvals >= required || (counted > 0 && approvals + remaining < required)
                                         // remaining = participants - counted
```

The `counted > 0` guard stops a coordinator sealing a binding `quorum.rejected` before anyone has voted, which an over-participant policy threshold could otherwise reach. Every decline the RFC describes has at least one ballot behind it.

Because readiness fires on *either* outcome, it is **non-monotonic in the approval count**, and it depends on the whole ballot box rather than the approval count alone. On three participants with `required = 3`: three rejections (zero approvals) are ready, one approval plus two rejections is ready, two approvals with one participant yet to vote is *not* ready, three approvals are ready. Probing readiness to discover the threshold -- by binary search especially -- returns a confident wrong answer; call the accessors above instead.

**Abstention handling**: `abstention.counts_toward_quorum` is **currently inert**. It is parsed into `AbstentionRules` and checked at registration, but no production path reads it: `QuorumThreshold::effective` divides a `percentage` threshold by the raw declared participant count, and Decision mode's `voting.quorum` percentage uses the same unadjusted denominator. An abstention therefore never shrinks a percentage denominator. The one abstention field that is read is `interpretation` -- and `evaluate_quorum_commitment` only *reports* it in the decision reasons rather than gating on it (see [Policy](policy.md#how-evaluation-works)). Separately, and not driven by these rules, Decision mode's *voting ratio* does exclude abstain ballots from its denominator, per RFC-MACP-0004.

## Built-in Extension: Multi-Round Mode

**Source**: `crates/macp-modes/src/mode/multi_round.rs` | **Identifier**: `ext.multi_round.v1`

The multi-round mode is a built-in extension for iterative convergence. It is discoverable via `ListExtModes` (not `ListModes`, which returns only standards-track modes).

Participants send `Contribute` messages with a `value` string. Each contribution overwrites the sender's previous value. When all declared participants have contributed the same value, the runtime marks the session as converged. Convergence does not auto-resolve the session -- an explicit commitment is required.

Unlike the standards-track modes, multi-round uses JSON-encoded payloads rather than protobuf.

## Dynamic Extension Modes

Extensions can be registered at runtime via `RegisterExtMode`. Each registered extension is backed by the passthrough handler (`crates/macp-modes/src/mode/passthrough.rs`), which accepts any message type listed in the extension's descriptor and requires an explicit commitment from the initiator to resolve the session.

Extension mode names must not use the reserved `macp.mode.*` namespace. Built-in modes cannot be unregistered. Extensions can be promoted to standards-track status via `PromoteMode`, and all registry changes are broadcast to `WatchModeRegistry` subscribers.
