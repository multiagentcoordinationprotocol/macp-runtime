# Coordination Modes

This page documents the runtime's implementation of each coordination mode -- the internal state machines, phase progression rules, and implementation-specific behavior. For mode specifications, message types, authority matrices, and protocol-level semantics, see the [protocol modes documentation](https://www.multiagentcoordinationprotocol.io/docs/modes) and the individual mode RFCs.

## Mode-state records are sealed

Each mode keeps its working state in `session.mode_state`, a serialized blob whose Rust shape is a small set of `pub` records. Those records are **`#[non_exhaustive]` as of 0.8.0**, so a crate outside `macp-modes` can read and match their fields but cannot construct one by struct literal:

| Crate | Sealed records |
|---|---|
| `macp-modes` (handoff) | `HandoffOfferRecord`, `HandoffContextRecord`, `HandoffState` |
| `macp-modes` (quorum) | `ApprovalRequestRecord`, `BallotRecord`, `QuorumState` |
| `macp-modes` (proposal) | `ProposalRecord`, `TerminalRejectRecord`, `RejectRecord`, `ProposalState` |
| `macp-modes` (task) | `TaskRecord`, `TaskRejectRecord`, `TaskUpdateRecord`, `TaskCompleteRecord`, `TaskFailRecord`, `TaskState` |
| `macp-modes` (multi_round) | `MultiRoundState` |
| `macp-storage` | `PersistedSession` |

This is a **breaking change for external callers** that built any of these by exhaustive struct literal, and it was taken deliberately. It is not, however, what makes 0.8.0 a major release: 0.8.0 was **already** forced to be one. These records grow a field whenever a mode learns something new, and with all-`pub` fields and no seal each addition is a `constructible_struct_adds_field` major break of its own. Against the published 0.7.6, `cargo semver-checks check-release --workspace` reports exactly two such breaks in this release -- `HandoffOfferRecord.suspended_ms_at_offer` and `PersistedSession.suspension_intervals` -- and neither is avoidable: they are the state the suspension-corrected implicit-accept deadline has to persist. Since `release-plz.toml` sets `semver_check = true` and all seven crates move in one `version_group`, either one alone blocks the release PR for the whole family.

Given a major was being spent regardless, it was spent once to **end the class** rather than twice on the same two fields: sealing the records makes every future mode-state field additive. That is why the table covers all seventeen `macp-modes` records and not only the two that were forced -- the rest demonstrably grow too (`ProposalState.rejections`, `ProposalState.phase`, `MultiRoundState.convergence_type` and `MultiRoundState.converged` all carry `#[serde(default)]`, i.e. each was added after the fact), and sealing them in a release that was already major costs nothing.

What still works unchanged: reading fields, mutating fields on a value you were handed, pattern-matching with `..`, `Default::default()` on `HandoffState`, `QuorumState`, `ProposalState` and `TaskState`, and `PersistedSession::from(&Session)` followed by field assignment. What does not: `HandoffState { offers, contexts }` and its siblings, including the `{ ..Default::default() }` form.

One record has a supported replacement, because it is a parameter of a public API rather than pure serialization detail: build an `ApprovalRequestRecord` with `ApprovalRequestRecord::new(request_id, required_approvals)` and assign whatever else you need on a `mut` binding, which keeps `QuorumMode::effective_threshold(&Session, &ApprovalRequestRecord)` callable from another crate. The constructor is deliberately narrow -- it takes only the field threshold resolution reads plus the record's identity -- and **its arity will not change**: a future field on the record is reached by assignment, and a future *mandatory* field would get its own constructor. A constructor taking every field would have re-opened the door the seal closes, since widening it is a `method_parameter_count_changed` major that blocks the release PR exactly as the struct-literal break did. The rest of the records are produced by the runtime from accepted envelopes and have no supported construction path.

The five decision domain types in `macp-core` -- `DecisionState`, `Proposal`, `Evaluation`, `Objection`, `Vote` -- are deliberately **not** sealed and stay constructible by struct literal. `macp-core` is the runtime's vocabulary crate, and those five are the argument types of the public `PolicyEvaluator` trait: a consumer driving `macp-core` + `macp-modes` with its own evaluator needs to build a `DecisionState` to test their implementation, and `DecisionState` derives no `Default`. Sealing them needs constructors designed first, so it was not folded into this sweep.

The enums in the same modules -- `HandoffDisposition`, `BallotChoice`, `ApprovalThreshold` -- are deliberately **not** sealed. A `#[non_exhaustive]` enum forces a `_` arm in every external `match`, which would silently reinterpret a future variant as one of today's; for a governance bar that is the exact defect class issue #145 was. Adding a variant is already the major lint `enum_variant_added`, so the release PR catches it either way.

## Decision Mode

**Source**: `crates/macp-modes/src/mode/decision.rs` | **Identifier**: `macp.mode.decision.v1`

The decision mode tracks proposals, evaluations, objections, and votes through an automatic phase progression. Its internal state consists of:

- A map of proposals keyed by `proposal_id`
- Lists of evaluations and objections
- A nested map of votes keyed by `proposal_id` then by sender
- A phase indicator that advances automatically as the session progresses

**Phase progression** is automatic: the first accepted `Proposal` moves the phase from Proposal to Evaluation, and the first accepted `Vote` moves it to Voting. Once in the Voting phase, **deliberation is closed to all three deliberation message types** -- a subsequent `Proposal`, `Evaluation` *or* `Objection` is rejected (RFC-MACP-0007 §5 rule 6: the first accepted `Vote` fixes the option set and the deliberation record from which the voting result and RFC-MACP-0012's objection-handling rules are computed, so admitting post-vote deliberation would let two conforming runtimes derive different commitment eligibility from identical accepted history). All three rejections surface as the wire code `INVALID_ENVELOPE`, from `MacpError::InvalidPayload` internally; the two gates covering them are `ensure_can_propose` (for `Proposal`) and `ensure_can_deliberate` (for `Evaluation` and `Objection`) in `crates/macp-modes/src/mode/decision.rs`, covered by its `proposal_after_voting_rejected`, `evaluation_after_voting_rejected` and `objection_after_voting_rejected` tests. This progression is enforced by the runtime, not by agents.

**Value normalization**: Recommendation values, vote values, and severity levels are stored in a canonical form (uppercase for recommendations and votes, lowercase for severity) to ensure deterministic comparison during policy evaluation.

**Commitment readiness**: The runtime requires at least one proposal to exist before accepting a commitment. If governance policies are bound to the session, they impose additional requirements -- vote quorum, confidence thresholds, and veto rules -- that must also be satisfied.

**An empty `participants` list is accepted -- for Decision alone.** Every other standards-track mode refuses `SessionStart` with an empty roster (and Task and Handoff refuse more than that: Task needs a participant other than the initiator, Handoff needs two parties). RFC-MACP-0001 §7.1 requires `participants` only "when required by the Mode", and RFC-MACP-0007 makes the initiator's authority role-based rather than membership-based, so for Decision the roster and the authority model are independent. The resulting session is well-defined and **inert**: `Proposal`, `Evaluation`, `Objection` and `Vote` are authorized only for declared participants, so with none declared **every** such message is `FORBIDDEN` -- the initiator's included -- no proposal can ever exist, and commitment readiness can never be met. Such a session can only expire or be cancelled. The carve-out is enforced in one place (`macp_core::session`), not by the Decision mode itself.

## Proposal Mode

**Source**: `crates/macp-modes/src/mode/proposal.rs` | **Identifier**: `macp.mode.proposal.v1`

The proposal mode handles offer-and-counteroffer negotiation. Its internal state tracks every proposal ever accepted into the session together with its **disposition**, per-participant acceptance records, and any terminal rejections. Acceptance is a separate sender-keyed relation (one proposal id per participant, latest wins per RFC-MACP-0008 §5 rule 5), never denormalized onto the proposal record.

**Convergence detection** happens automatically after each message. The `refresh_phase()` method checks the session's acceptance criterion (configurable via policy as `all_parties`, `counterparty`, or `initiator`) and transitions the phase to Converged when the criterion is met. Convergence does not auto-resolve the session -- an explicit commitment is still required.

**Counter-proposal semantics**: A `CounterProposal` creates a new entry with its own `proposal_id`. The `supersedes_proposal_id` field is informational only -- unless withdrawn, the original proposal stays live and participants can accept either (RFC-MACP-0008 §5 rule 2a: the field records semantic intent but does not retire the original, and every live proposal is tracked independently). Round limits are enforced at counter-proposal submission time, not just at commitment.

**Disposition and `Withdraw`**: each proposal record carries a disposition whose domain is `{Live, Withdrawn}` -- the `ProposalDisposition` enum in `crates/macp-modes/src/mode/proposal.rs`, pinned cross-implementation as `mode_state_dispositions` in `tests/parity/contract.json` → `sections.proposal_disposition`. A `Withdraw` message is authorized only for the referenced proposal's **author**; because a `CounterProposal` creates a new `proposal_id`, only the sender of that counter-proposal may withdraw it (RFC-MACP-0008 §2.1 authority matrix). An accepted `Withdraw` sets that one proposal's disposition to `Withdrawn`, and the consequences are scoped to it:

- It can no longer be accepted, rejected, or committed. `Accept` and `Reject` both resolve their target through `live_proposal()`, which filters on `Live`, and convergence detection only considers `Live` proposals -- so a withdrawn proposal can never satisfy the acceptance criterion. RFC-MACP-0008 §5 rule 4 requires the accept and commit halves of this; refusing a `Reject` against a withdrawn proposal is the runtime's own consistency choice (§5 rule 3 asks only that `Accept`, `Reject` and `Withdraw` reference an *existing* proposal).
- Any accepts naming it are dropped, and **its** terminal rejection is cleared -- which can take the session back out of the TerminalRejected phase. Terminal rejections recorded against *other* proposals survive untouched. Tests: `withdraw_clears_terminal_rejections`, `terminal_rejection_on_different_proposal_survives_withdraw`, `accept_on_withdrawn_proposal_rejected`, `reject_withdrawn_proposal_fails`.
- Withdrawing an already-withdrawn proposal, or naming an empty/unknown `proposal_id`, is rejected; a `Withdraw` from anyone but the author is `FORBIDDEN`.

Proposal's disposition domain deliberately excludes `Accepted`, where Handoff's `HandoffDisposition` includes it. That asymmetry is recorded as intentional (and not as drift) in `sections.proposal_disposition`'s `source` field, which also explains why the mode's `phase` is not pinned there.

**Terminal rejection**: A `Reject` message with `terminal: true` immediately transitions the session phase to TerminalRejected, making the session eligible for a negative-outcome commitment.

## Task Mode

**Source**: `crates/macp-modes/src/mode/task.rs` | **Identifier**: `macp.mode.task.v1`

The task mode manages bounded work delegation. Its internal state tracks the task request, the currently active assignee, any rejection records, progress updates, and the terminal report (complete or fail).

**Assignment lifecycle**: After the initiator sends a `TaskRequest`, an eligible participant can accept with `TaskAccept`, which sets them as the active assignee (RFC-MACP-0009 §5 rule 3a -- the first accepted `TaskAccept` designates the assignee, and a later one is refused while an assignee stands). Only the active assignee can send `TaskUpdate` messages -- this is validated against the authenticated sender, not a payload field. **`TaskAccept` is irrevocable** absent a reassignment policy: a participant who has accepted may not then send `TaskReject` for the same task (RFC-MACP-0009 §5 rule 3b). The runtime enforces this as `POLICY_DENIED` rather than `FORBIDDEN`, since what is missing is a policy permission rather than sender authority.

**Reassignment**: That irrevocability is exactly what the `allow_reassignment_on_reject` policy rule lifts (RFC-MACP-0009 §5 rule 3c). With it enabled, a `TaskReject` from the active assignee clears the assignee and returns the session to its pre-assignment state. Other eligible participants can then send `TaskAccept` to take over the task; no new `TaskRequest` is needed, since the original request remains active.

**Terminal reports**: Either `TaskComplete` or `TaskFail` records the outcome, but neither resolves the session. An explicit commitment from the initiator is required to bind the result.

## Handoff Mode

**Source**: `crates/macp-modes/src/mode/handoff.rs` | **Identifier**: `macp.mode.handoff.v1`

The handoff mode manages responsibility transfer through serial offers. Its internal state tracks offers and their associated context messages.

**Serial offer constraint**: Only one outstanding (unresolved) offer may exist at a time, so a new `HandoffOffer` is refused while a prior one is still pending. Once an offer is accepted, no further offers can be issued in that session, and only one final `Commitment` resolves it. A session may still carry several *sequential* offers to different targets (RFC-MACP-0010 §5 rule 5).

**Late context**: `HandoffContext` messages are accepted even after the offer they reference has been accepted or declined. RFC-MACP-0010 §2.1 licenses this explicitly -- `HandoffContext` SHOULD precede the accept or decline, but late context "is permitted but serves only as supplementary documentation, not as input to the accept/decline decision". The §5 rule 2 requirement that it reference an existing `handoff_id` still applies.

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

`semantics_rev` is one **shared counter across all modes**, not a per-mode version: each bump pins whatever behavior changed in the release that raised it, and the mode it concerns differs from row to row. Revisions 0-2 are all Handoff implicit-accept; revision 3 touches no Handoff behavior at all. The authoritative enumeration, with the reasoning for each step, is the doc comment on `macp_core::session::CURRENT_SEMANTICS_REV` (`crates/macp-core/src/session.rs`).

| `semantics_rev` | Mode affected | What the revision gates |
|---|---|---|
| `0` | `macp.mode.handoff.v1` | Implicit accept inferred inside `Commitment` handling, timed against the client-supplied `Envelope.timestamp_unix_ms`. Suspended time counts. |
| `1` | `macp.mode.handoff.v1` | Same inference, timed against the runtime's acceptance clock instead of the client's timestamp. Suspended time counts. |
| `2` | `macp.mode.handoff.v1` | The synthetic history entry described above (§5.1(2)). Suspended time is excluded (§5.1(1)). Client-submitted implicit accepts and reserved `implicit-accept:` ids are refused (§5.1(3)). |
| `3` (current) | `ext.multi_round.v1` | The `Contribute` decode gains a canonical-proto tie-break: a successful legacy-JSON parse is trusted only when the same bytes do **not** also round-trip byte-identically through the canonical `ContributePayload` proto encoding (issue #192 -- see [Multi-Round Mode](#built-in-extension-multi-round-mode)). Revisions 0-2 keep the unconditional JSON-first decode. |

Sessions started by this release are rev 3. Because every existing gate on this field tests `>= 2` or `<= 1` (never `== 2`), raising the counter to 3 was additive for Handoff, Quorum, Proposal, Task and Decision: a rev-3 session gets exactly the rev-2 Handoff behavior in the table above.

Sessions restored from a log written by an earlier release keep their recorded revision. A rev-0 or rev-1 handoff session continues to resolve through the interim in-`Commitment` path, with no synthetic entry and no reserved-id restriction; a rev-0, rev-1 or rev-2 multi-round session continues to decode `Contribute` JSON-first without the tie-break, including at the collision lengths, because that is how its history was originally accepted.

**Rollback is not a revert.** Once a session has written history under a newer revision -- a synthetic handoff accept, or a contribution decoded under the tie-break -- an older binary replays that history differently. The recovery path for a bad deployment of any release that raised this counter is therefore to **roll forward** to a fixed build, not to roll back to the previous one.

## Quorum Mode

**Source**: `crates/macp-modes/src/mode/quorum.rs` | **Identifier**: `macp.mode.quorum.v1`

The quorum mode tracks approval requests and ballots against a threshold. Its internal state records the approval request and a map of ballots (approve, reject, or abstain) keyed by sender.

**Threshold resolution**: A governance policy's `threshold` rule *replaces* the `required_approvals` value from the `ApprovalRequest` payload rather than supplementing it (RFC-MACP-0011 §5 rule 6). The arithmetic is ceiling-rounded with a floor of one approval, and lives in `QuorumThreshold::effective` (`macp-core`) -- the same function the policy evaluator calls, so the mode and the evaluator cannot derive two different bars from one policy.

**Reading the threshold from outside the runtime**: two public accessors on `QuorumMode` report the bar the runtime itself enforces, so a caller never has to re-derive it from policy rules and participant counts:

| Accessor | Returns |
|----------|---------|
| `QuorumMode::effective_threshold_for_session(&Session)` | `Result<Option<ApprovalThreshold>, MacpError>` |
| `QuorumMode::effective_threshold(&Session, &ApprovalRequestRecord)` | `ApprovalThreshold` |

Prefer the session-level form. The request-level form exists for a caller that already holds a record; build one with `ApprovalRequestRecord::new(request_id, required_approvals)`, assigning `action`, `summary`, `details` and `requested_by` afterwards if you need them (they play no part in threshold resolution) -- as of 0.8.0 the record is `#[non_exhaustive]` (see [Mode-state records are sealed](#mode-state-records-are-sealed)) and the struct-literal form no longer compiles outside `macp-modes`.

The session-level form decodes the accepted request out of `session.mode_state` itself. Each layer of its return type answers exactly one question:

- `Ok(Some(ApprovalThreshold::Approvals(n)))` -- `n` `Approve` ballots seal a positive commitment. Never zero; for a session whose request the mode accepted, never above the participant count. A session with no `threshold` rule (the common case) reports the payload's own `required_approvals` here, already resolved.
- `Ok(Some(ApprovalThreshold::Unsatisfiable))` -- the bound policy admits no positive commitment at any approval count (`threshold.type: "weighted"`, an unrecognised type, or a `percentage` over an empty participant set). `RegisterPolicy` refuses the first two, so reaching them requires a directly constructed `PolicyDefinition`; the third cannot be caught at registration, which has no participant count, and is blocked by `QuorumMode::on_session_start` rejecting an empty participant set instead. Such a session seals **neither** outcome.
- `Ok(None)` -- no `ApprovalRequest` has been accepted yet, so there is nothing to resolve. Deliberately distinct from `Unsatisfiable`: "not yet" and "never" are different answers.
- `Err(MacpError::InvalidModeState)` -- `session.mode_state` is not decodable quorum state, so no answer would be honest.

**Commitment readiness**: the runtime accepts a commitment when the approval threshold is met, or when it has become mathematically unreachable and at least one ballot has been cast -- the unreachability half being RFC-MACP-0011 §5 rule 4a's trigger for a *negative* commitment:

```text
approvals >= required || (counted > 0 && approvals + remaining < required)
                                         // remaining = participants - counted
```

The `counted > 0` guard stops a coordinator sealing a binding `quorum.rejected` before anyone has voted, which an over-participant policy threshold could otherwise reach (issue #145).

**This guard is a deliberate deviation from RFC-MACP-0011 §5 rule 6**, not a restatement of it. That rule says an `n_of_m` override above the eligible participant count "is not rejected at admission ... and simply makes the threshold unreachable, so the Session becomes eligible for `Commitment` under rule 4's second clause ... with the negative outcome of rule 4b" -- i.e. the RFC leaves such a session negatively committable with **zero** ballots cast. The runtime declines to: it requires at least one ballot before any decline is sealed. The reasoning is that every decline the RFC's own rules 4a and 4b *describe* has at least one ballot behind it (4a's arithmetic counts those who have voted; 4b's antecedent is abstentions and rejections), and rule 6 reaches a ballotless decline only as a side effect of an unreachable override that the schema admits because it cannot see a participant count. Sealing a binding `quorum.rejected` that no participant had any opportunity to influence is the defect class issue #145 was. A session with such an override therefore seals neither outcome until at least one participant ballots, at which point the decline the RFC expects becomes available.

Because readiness fires on *either* outcome, it is **non-monotonic in the approval count**, and it depends on the whole ballot box rather than the approval count alone. On three participants with `required = 3`: three rejections (zero approvals) are ready, one approval plus two rejections is ready, two approvals with one participant yet to vote is *not* ready, three approvals are ready. Probing readiness to discover the threshold -- by binary search especially -- returns a confident wrong answer; call the accessors above instead.

**Abstention handling**: `abstention.counts_toward_quorum` is **currently inert**. It is parsed into `AbstentionRules` and checked at registration, but no production path reads it: `QuorumThreshold::effective` divides a `percentage` threshold by the raw declared participant count, and Decision mode's `voting.quorum` percentage uses the same unadjusted denominator. An abstention therefore never shrinks a percentage denominator. The one abstention field that is read is `interpretation` -- and `evaluate_quorum_commitment` only *reports* it in the decision reasons rather than gating on it (see [Policy](policy.md#how-evaluation-works)). Separately, and not driven by these rules, Decision mode's *voting ratio* does exclude abstain ballots from its denominator -- that is RFC-MACP-0012 §4.1's **"Denominator"** rule, which fixes the denominator of the ratio-based algorithms (`majority`, `supermajority`, `weighted`) at the *decisive* votes, those cast as approve or reject.

## Built-in Extension: Multi-Round Mode

**Source**: `crates/macp-modes/src/mode/multi_round.rs` | **Identifier**: `ext.multi_round.v1`

The multi-round mode is a built-in extension for iterative convergence. It is discoverable via `ListExtModes` (not `ListModes`, which returns only standards-track modes).

Participants send `Contribute` messages with a `value` string. Each contribution overwrites the sender's previous value. When all declared participants have contributed the same value, the runtime marks the session as converged. Convergence does not auto-resolve the session -- an explicit commitment is required.

**`Contribute` wire format.** Like the standards-track modes, multi-round has a canonical protobuf payload: `ContributePayload` in `macp/modes/multi_round/v1/multi_round.proto` (compiled into `macp-pb` alongside the five standards-track mode protos), a single field 1 `value` of type `string`. That is the encoding clients should send. A legacy JSON object `{"value": "..."}` predates the proto and is **still accepted**, because replay must parse already-persisted bytes identically forever (RFC-MACP-0003 §1).

The decoder (`parse_contribute_value`, `crates/macp-modes/src/mode/multi_round.rs`) therefore tries JSON **first**, permanently -- trying proto first would let pathological JSON bytes decode as a *valid* proto message with a different value. The catch is that the reverse collision also exists: at certain value byte-lengths the proto tag byte and length varint are themselves insignificant JSON whitespace (or the literal `{`), so a genuine canonical-proto payload can also parse as a legacy JSON object. At `semantics_rev >= 3` the runtime applies a **canonicality tie-break** for this (issue #192): a successful JSON parse is trusted only when the same bytes do *not* also round-trip byte-identically through the canonical proto encoding; where they do, the proto reading wins. Revisions 0-2 keep the unconditional JSON-first reading -- see [Revision gating](#revision-gating).

The byte-level collision vectors are pinned, cross-implementation, in `tests/parity/contract.json` → `sections.contribute_payload` (`vectors`, `collision_*`) rather than restated here; both SDKs apply the same tie-break (macp-sdk-typescript issue #104, macp-sdk-python PR #77), and that section's `source` field records the agreed band and the one length where the three implementations historically differed.

One acceptance rule is this runtime's alone: an **empty** `Contribute` payload is rejected. `sections.contribute_acceptance` pins it with `applies_to: ["macp-runtime"]`, and that narrowing is deliberate and permanent, not pending agreement -- neither SDK gates an empty payload at decode time, and under proto3 `value` has no field presence, so an explicitly-empty contribution and an absent payload are the same zero bytes and indistinguishable to a decoder.

## Dynamic Extension Modes

Extensions can be registered at runtime via `RegisterExtMode`. Each registered extension is backed by the passthrough handler (`crates/macp-modes/src/mode/passthrough.rs`), which accepts any message type listed in the extension's descriptor and requires an explicit commitment from the initiator to resolve the session.

**This runtime refuses extension names in the reserved `macp.mode.*` namespace -- a deliberately stricter rule than the RFC's.** RFC-MACP-0002 §3 and §12 say only that implementation-defined and extension modes **SHOULD** avoid that namespace (§12 suggests `ext.*` or reverse-domain identifiers); the runtime makes it a MUST, rejecting both `RegisterExtMode` with such a name and any `PromoteMode` that would rename a mode into it (`crates/macp-modes/src/mode_registry.rs`). The promotion guard also drops the RFC's escape hatch: §12 permits such a rename when the mode has been published in the main MACP RFC repository or carries explicit community-governance approval, and the runtime has no way to verify either, so it refuses unconditionally. Without this, `RegisterExtMode("ext.x")` followed by `PromoteMode("ext.x" → "macp.mode.x")` would be a side door for a passthrough-backed extension to masquerade as a standards-track mode.

Built-in modes cannot be unregistered. Extensions can be promoted to standards-track status via `PromoteMode` -- which grants standards-track status *on this runtime*, not RFC namespace membership -- and all registry changes are broadcast to `WatchModeRegistry` subscribers.
