# Coordination Modes

This page documents the runtime's implementation of each coordination mode -- the internal state machines, phase progression rules, and implementation-specific behavior. For mode specifications, message types, authority matrices, and protocol-level semantics, see the [protocol modes documentation](https://www.multiagentcoordinationprotocol.io/docs/modes) and the individual mode RFCs.

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

## Quorum Mode

**Source**: `crates/macp-modes/src/mode/quorum.rs` | **Identifier**: `macp.mode.quorum.v1`

The quorum mode tracks approval requests and ballots against a threshold. Its internal state records the approval request and a map of ballots (approve, reject, or abstain) keyed by sender.

**Threshold resolution**: The `effective_threshold()` method checks whether a governance policy overrides the `required_approvals` value from the payload. Policy thresholds (percentage or count) replace the payload value entirely rather than supplementing it.

**Commitment readiness**: The runtime accepts a commitment when either the approval threshold is met or the threshold becomes mathematically unreachable (remaining possible approvals plus current approvals is still below the threshold).

**Abstention handling**: When the policy specifies abstention rules, the effective voter count is adjusted accordingly. An abstention with `counts_toward_quorum: false` reduces the denominator for percentage-based thresholds.

## Built-in Extension: Multi-Round Mode

**Source**: `crates/macp-modes/src/mode/multi_round.rs` | **Identifier**: `ext.multi_round.v1`

The multi-round mode is a built-in extension for iterative convergence. It is discoverable via `ListExtModes` (not `ListModes`, which returns only standards-track modes).

Participants send `Contribute` messages with a `value` string. Each contribution overwrites the sender's previous value. When all declared participants have contributed the same value, the runtime marks the session as converged. Convergence does not auto-resolve the session -- an explicit commitment is required.

Unlike the standards-track modes, multi-round uses JSON-encoded payloads rather than protobuf.

## Dynamic Extension Modes

Extensions can be registered at runtime via `RegisterExtMode`. Each registered extension is backed by the passthrough handler (`crates/macp-modes/src/mode/passthrough.rs`), which accepts any message type listed in the extension's descriptor and requires an explicit commitment from the initiator to resolve the session.

Extension mode names must not use the reserved `macp.mode.*` namespace. Built-in modes cannot be unregistered. Extensions can be promoted to standards-track status via `PromoteMode`, and all registry changes are broadcast to `WatchModeRegistry` subscribers.
