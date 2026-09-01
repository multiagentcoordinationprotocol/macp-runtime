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

The built-in `policy.default` is always present and cannot be registered or removed. The `policy.std.` namespace is reserved the same way -- see [Reserved `policy.std.` profiles](#reserved-policystd-profiles).

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

At registration, the runtime validates the rules against the target mode's schema. It enforces structural constraints: a `weighted` voting algorithm requires a non-empty `weights` map, `supermajority` requires a threshold above 0.5, and `designated_role` commitment authority requires a non-empty `designated_roles` list. The `schema_version` must be `1`. Rules that fail to deserialize into the target mode's Rust struct are rejected with `INVALID_POLICY_DEFINITION`. A `policy_id` under the reserved `policy.std.` prefix is rejected the same way unless it is the canonical definition (see below).

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

#### Voting algorithm semantics

These are the rules the evaluator actually applies (RFC-MACP-0012 §4.1):

| Algorithm | Bar |
|-----------|-----|
| `none` | No voting constraint; the mode's built-in logic applies |
| `majority` | `approve / decisive >= threshold` (default `0.5`) |
| `supermajority` | `approve / decisive >= threshold`; the schema requires `threshold > 0.5` |
| `unanimous` | Every **declared participant** cast an approve vote and no reject exists; `threshold` is not consulted |
| `weighted` | Weighted approve share `>= threshold`, using `weights` (unlisted voters weigh `1.0`) |
| `plurality` | More approve than reject; a tie fails; no threshold |

- **Denominator.** For `majority`, `supermajority` and `weighted` the denominator is the *decisive* votes -- those cast as approve or reject. Abstentions are excluded and neither help nor hinder the ratio.
- **Inclusive comparison.** Every threshold comparison is `ratio >= threshold`, so `majority` at its default `0.5` approves an even split. A rule where a tie fails is `plurality`, not `majority` at `0.5`.
- **Ratios are binary64.** Comparisons are Rust `f64`. With `threshold: 0.6666666666666666` (the binary64 value nearest two-thirds, and what `2.0 / 3.0` produces) 2-of-3, 4-of-6, 20-of-30 and 67-of-100 pass while 66-of-100 does not.
- **`voting.quorum` is inert on its own.** It states the participation bar but gates nothing until `commitment.require_vote_quorum` is `true`. A policy that sets `voting.quorum` without it imposes no participation requirement.
- **No decisive votes.** With any algorithm other than `none`, when no decisive vote has been cast the algorithm produces no result. A *positive* commitment is then blocked only if `commitment.require_vote_quorum` is `true` -- so a policy that means its voting algorithm to be binding must set it. A *negative* commitment is always blocked in this case, because a decline needs at least one explicit reject (RFC-MACP-0007 §6.2).

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
| `INVALID_POLICY_DEFINITION` | A policy fails schema validation at registration time, or claims a reserved `policy.std.` identifier | InvalidArgument |

`RegisterPolicy`/`UnregisterPolicy` report failures in band (`ok: false` plus an `error` string) rather than as a gRPC status, so reserved-namespace rejections carry the literal `INVALID_POLICY_DEFINITION` at the head of that string. The InvalidArgument mapping in the table applies where the code travels as a `MacpError` (for example the `SessionStart` path).

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

## Reserved `policy.std.` profiles

Every identifier beginning with `policy.std.` is reserved for the governance profiles published in RFC-MACP-0012 §5.2. The runtime enforces this in `crates/macp-policy/src/registry.rs`:

- A `policy_id` under the prefix is refused unless the descriptor is the canonical §5.2 definition for that identifier -- same `mode`, same `schema_version`, and rules that *resolve* to the canonical values (a parameter left to its schema default counts as that default). The rejection carries `INVALID_POLICY_DEFINITION`.
- An identifier under the prefix that the RFC has not assigned -- `policy.std.nonesuch`, say -- is refused outright and does not resolve. A `SessionStart` naming it is rejected with `UNKNOWN_POLICY_VERSION`.
- A pre-registered `policy.std.` profile cannot be unregistered, the same guard `policy.default` has.
- Both routes into the registry are covered: the `RegisterPolicy` RPC and the `MACP_POLICIES_DIR` preload, which funnels through the same `register` path. A policies directory containing a `policy.std.` file aborts startup.

Short unnamespaced identifiers such as `policy.majority` are **not** reserved and remain available. Deployments should still use their own namespace (`policy.{org}.{name}`).

This runtime pre-registers all three profiles, so they appear in `ListPolicies` and resolve at `SessionStart`. Provisioning them is optional under §5.2 -- a runtime that ships none of them is still conformant -- but the reservation guarantees that an identifier which *does* resolve resolves to these rules everywhere. All three target `macp.mode.decision.v1` at `schema_version: 1`, and all three set `commitment.require_vote_quorum: true`: without it the voting algorithm would not be binding on an unvoted positive commitment, which would make each profile vacuous in exactly the case it exists to govern.

| Policy ID | Governance bar |
|-----------|----------------|
| `policy.std.majority` | At least half of the decisive votes approve (an even split approves) |
| `policy.std.supermajority` | At least two-thirds of the decisive votes approve, with at least two voters |
| `policy.std.unanimous` | Every declared participant has approved and no reject was cast |

```json
{
  "policy_id": "policy.std.majority",
  "mode": "macp.mode.decision.v1",
  "schema_version": 1,
  "description": "Simple majority — at least half of the decisive votes approve",
  "rules": {
    "voting": {
      "algorithm": "majority",
      "threshold": 0.5,
      "quorum": { "type": "count", "value": 1 }
    },
    "commitment": { "require_vote_quorum": true }
  }
}
```

```json
{
  "policy_id": "policy.std.supermajority",
  "mode": "macp.mode.decision.v1",
  "schema_version": 1,
  "description": "Two-thirds supermajority with a minimum of two voters",
  "rules": {
    "voting": {
      "algorithm": "supermajority",
      "threshold": 0.6666666666666666,
      "quorum": { "type": "count", "value": 2 }
    },
    "commitment": { "require_vote_quorum": true }
  }
}
```

```json
{
  "policy_id": "policy.std.unanimous",
  "mode": "macp.mode.decision.v1",
  "schema_version": 1,
  "description": "Unanimous — every declared participant approves and no reject is cast",
  "rules": {
    "voting": {
      "algorithm": "unanimous",
      "quorum": { "type": "count", "value": 1 }
    },
    "commitment": { "require_vote_quorum": true }
  }
}
```

Note that `policy.std.unanimous` counts *declared participants*, not decisive votes: the session initiator is a declared participant under RFC-MACP-0007 §2, so it must vote too. A participant who abstains or never votes blocks the commitment.
