# Governance Policy

This page covers the runtime's implementation of the governance policy framework: how to register policies via gRPC, what rule schemas look like in practice, how the evaluation engine works internally, and how errors are surfaced. For the protocol-level policy specification -- identifiers, lifecycle, determinism guarantees, and the full rule schema definitions -- see the [protocol policy documentation](https://www.multiagentcoordinationprotocol.io/docs/policy).

## Managing policies

Policies are managed through five gRPC RPCs. Any authenticated sender can perform these operations.

| RPC | Purpose |
|-----|---------|
| `RegisterPolicy` | Add a new policy to the registry |
| `UnregisterPolicy` | Remove a policy (does not affect sessions already using it) |
| `GetPolicy` | Retrieve a policy by its identifier |
| `ListPolicies` | List all policies, optionally filtered by target mode |
| `WatchPolicies` | Stream notifications when the registry changes |

The built-in `policy.default` is always present and cannot be registered or removed.

## Registering a policy

Here is a complete example of registering a Decision Mode policy that requires majority voting with a confidence threshold:

```json
{
  "policy_id": "policy.fraud-review.majority-vote",
  "mode": "macp.mode.decision.v1",
  "description": "Require majority vote with 0.7 confidence threshold",
  "schema_version": 1,
  "rules": {
    "voting": {
      "algorithm": "majority",
      "threshold": 0.5,
      "quorum": { "type": "percentage", "value": 60 }
    },
    "evaluation": {
      "required_before_voting": true,
      "minimum_confidence": 0.7
    },
    "objection_handling": {
      "critical_severity_vetoes": true,
      "veto_threshold": 1
    },
    "commitment": {
      "authority": "initiator_only"
    }
  }
}
```

At registration, the runtime validates the rules against the target mode's schema. It enforces structural constraints: a `weighted` voting algorithm requires a non-empty `weights` map, `supermajority` requires a threshold above 0.5, and `designated_role` commitment authority requires a non-empty `designated_roles` list. The `schema_version` must be `1`. Rules that fail to deserialize into the target mode's Rust struct are rejected with `INVALID_POLICY_DEFINITION`.

## Rule examples by mode

### Decision Mode

```json
{
  "voting": {
    "algorithm": "supermajority",
    "threshold": 0.67,
    "quorum": { "type": "count", "value": 3 },
    "weights": {}
  },
  "evaluation": {
    "required_before_voting": true,
    "minimum_confidence": 0.7
  },
  "objection_handling": {
    "critical_severity_vetoes": true,
    "veto_threshold": 1
  },
  "commitment": {
    "authority": "initiator_only",
    "designated_roles": [],
    "require_vote_quorum": true
  }
}
```

Supported voting algorithms: `none`, `majority`, `supermajority`, `unanimous`, `weighted`, `plurality`.

### Proposal Mode

```json
{
  "acceptance": { "criterion": "all_parties" },
  "counter_proposal": { "max_rounds": 5 },
  "rejection": { "terminal_on_any_reject": false },
  "commitment": { "authority": "initiator_only" }
}
```

Acceptance criteria: `all_parties`, `counterparty`, `initiator`.

### Task Mode

```json
{
  "assignment": { "allow_reassignment_on_reject": true },
  "completion": { "require_output": true },
  "commitment": { "authority": "initiator_only" }
}
```

### Handoff Mode

```json
{
  "acceptance": { "implicit_accept_timeout_ms": 30000 },
  "commitment": { "authority": "initiator_only" }
}
```

### Quorum Mode

```json
{
  "threshold": { "threshold_type": "percentage", "value": 66 },
  "abstention": { "counts_toward_quorum": false, "interpretation": "neutral" },
  "commitment": { "authority": "initiator_only" }
}
```

Threshold types: `n_of_m`, `percentage`, `count`. Abstention interpretations: `neutral`, `implicit_reject`, `ignored`.

## How evaluation works

Each standard mode has a dedicated evaluator in `crates/macp-policy/src/evaluator.rs`. Evaluation runs when a `Commitment` envelope arrives, after the mode's own validation has passed. It is a pure function of three inputs: the resolved policy rules, the accumulated accepted message history, and the session's declared participants. No wall-clock time, external calls, or out-of-session state are involved.

| Evaluator | What it checks |
|-----------|---------------|
| `evaluate_decision_commitment` | Qualifying evaluations meet the confidence threshold, critical objection count stays below veto threshold, vote quorum is met, voting algorithm threshold is satisfied. REVIEW-type evaluations are excluded from confidence checks. |
| `evaluate_proposal_commitment` | Counter-proposal count is within `max_rounds` |
| `evaluate_task_commitment` | Output is present if `require_output` is set |
| `evaluate_handoff_commitment` | Always allows (implicit timeout is handled by the mode) |
| `evaluate_quorum_commitment` | Effective voter count (adjusted for abstention rules) satisfies the threshold |

## Commitment authority

The `commitment.authority` rule determines who can send the terminal commitment. This is enforced in `crates/macp-modes/src/mode/util.rs` and applies across all modes:

| Value | Who can commit |
|-------|---------------|
| `initiator_only` (default) | The session initiator |
| `any_participant` | Any declared participant or the initiator |
| `designated_role` | Only agents listed in the `designated_roles` array |

## Error handling

| Error code | When it occurs | gRPC status |
|-----------|----------------|-------------|
| `UNKNOWN_POLICY_VERSION` | The `policy_version` in SessionStart is not found in the registry | InvalidArgument |
| `POLICY_DENIED` | A commitment is rejected because governance rules are not satisfied | PermissionDenied |
| `INVALID_POLICY_DEFINITION` | A policy fails schema validation at registration time | InvalidArgument |

When a commitment is denied, the error includes structured reasons explaining which rules were not met:

```json
{
  "reasons": [
    "vote quorum not met: 1 voters of 3 participants (quorum: 60 percentage)",
    "no qualifying evaluation meets minimum confidence threshold: 0.70"
  ]
}
```

## Default policy

The default policy (`policy.default`) is always registered with mode `"*"` and no governance constraints:

```json
{
  "voting": { "algorithm": "none", "quorum": { "type": "count", "value": 0 } },
  "objection_handling": { "critical_severity_vetoes": false, "veto_threshold": 1 },
  "evaluation": { "required_before_voting": false, "minimum_confidence": 0.0 },
  "commitment": { "authority": "initiator_only", "designated_roles": [], "require_vote_quorum": false }
}
```

Sessions with an empty `policy_version` automatically resolve to this default. It allows commitment whenever the mode's own built-in rules are satisfied.
