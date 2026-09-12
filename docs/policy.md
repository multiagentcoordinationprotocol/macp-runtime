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

### What registration checks

The runtime does **not** run a JSON-Schema evaluator: it carries no `jsonschema` dependency, and the canonical `schemas/json/policy/*.schema.json` documents live in the spec repository, not here. Registration instead applies three layers of hand-written checks, and only the constraints listed below are enforced. A rule the canonical schema forbids but this list does not name is accepted.

1. **Deserialization.** Rules must parse into the target mode's Rust struct. Every field has a default and unknown fields are ignored, so this catches type errors (a string where a number belongs), not missing or misspelled keys. Extension modes (`ext.*`) and unrecognized mode names accept any JSON object.
2. **Value domains, mirroring the canonical schemas.** Enum membership and numeric bounds, copied from the schema text and pinned to it by a parity test that runs in CI:

   | Constraint | Rule |
   |---|---|
   | `voting.algorithm` | One of `none`, `majority`, `supermajority`, `unanimous`, `weighted`, `plurality` |
   | `voting.threshold` | Greater than `0.0`, at most `1.0`; at least `0.5` for `majority`, above `0.5` for `supermajority` |
   | `voting.weights[*]` | `> 0`, **exclusive** — a zero or negative weight is refused |
   | `voting.weights` | Non-empty whenever the key is supplied, whatever the algorithm |
   | `voting.quorum.type` | One of `count`, `percentage`. `n_of_m` is **not** legal here, though the evaluator would accept it |
   | `voting.quorum.value` | `>= 0` (a number, not necessarily an integer) |
   | Quorum `threshold.type` | One of `n_of_m`, `percentage`, `count`. `weighted` is refused — the canonical vocabulary no longer contains it, see below |
   | Quorum `threshold.value` | A non-negative **integer**; additionally `<= 100` when `threshold.type` is `percentage` |

   A wildcard (`"*"`) policy must satisfy **every** standards-track mode's schema and every mode's constraints above, not just Decision's, because `SessionStart` binds it to every mode's sessions. Before this release it was validated against the Decision schema alone, which has no top-level `threshold` — so a Quorum `threshold` inside a `"*"` policy was silently dropped at registration and then read, unchecked, by the Quorum mode. A `"*"` policy carrying an out-of-domain `threshold` is now refused. Fields one mode's schema does not know are still ignored rather than refused, so a Decision-shaped wildcard (including the built-in `policy.default`) registers unchanged.

   The bounds at zero used to be inclusive, which made `voting.threshold: 0.0` and an all-zero `voting.weights` map degenerate but schema-legal; the runtime deferred the question upstream rather than deciding it at registration. Spec #99 settled it, and both bounds are now **exclusive at zero** in `decision-rules.schema.json` (`exclusiveMinimum: 0`, plus `minProperties: 1` on `weights`). `threshold: 0.0` made an all-`REJECT` round return `Passed` under both `majority` and `weighted`; an all-zero or empty `weights` map yielded "no votes" on a *complete* ballot set. Both are refused at registration now. Nothing is lost by the weights rule: the `weights` map **is** the weighted electorate, so a legitimately zero-weighted observer is expressed by omission from the map rather than by an explicit `0`. Because `exclusiveMinimum: 0` excludes negatives a fortiori, a mixed-sign map such as `{"a": 1.0, "b": -1.0}` is refused on `b` and named in the error. `threshold.value` follows JSON Schema's `integer` keyword, which matches any number with a zero fractional part: `75` and `75.0` are both accepted, `75.5` is not.

   The `threshold` floor is unconditional and therefore reaches `unanimous` and `plurality`, which never read the field. That is deliberate per RFC-MACP-0012 §4.1 — a `threshold` a policy author believed was in force should never be silently ignored. A rules object that *omits* `threshold` is unaffected, since the default is `0.5`.
3. **Conditional constraints.** A `weighted` voting algorithm requires a non-empty `weights` map; a supplied `weights` map must be non-empty whatever the algorithm; `majority` requires a threshold of at least `0.5` and `supermajority` one above `0.5` — the asymmetry is deliberate, because the reserved `policy.std.majority` profile sets exactly `0.5` and RFC-MACP-0012 §2.2 pins it byte-identical on every runtime; and `designated_role` commitment authority requires a non-empty `designated_roles` list.

   `supermajority` additionally requires the `threshold` **key**, not merely a legal value: RFC-MACP-0012 1.2.0-draft (spec #112) added `required: ["threshold"]` to that arm, because without it `{"algorithm": "supermajority"}` validated and the schema's own `0.5` default then supplied a value the same arm forbids — a supermajority that was a bare majority wearing the name. This runtime already refuses it, and not by accident: the default is `0.5` and the check is `threshold <= 0.5`, so the value the schema would have filled in is exactly the illegal one. `majority` stays deliberately asymmetric — it constrains the value without requiring the key, since blocks that omit `threshold` rely on the legal `0.5` default.

`schema_version` must be non-zero, and `0` is the only value **registration** rejects: RFC-MACP-0012 §3 constrains which versions a runtime *supports* at evaluation, not which ones the registry admits, and `PolicyRegistry` has never enumerated versions. The **evaluator** supports `{1, 2, 3}` and denies anything else at commitment time with `unsupported policy schema version`. The three are not interchangeable:

- **`1`** — the original rule set.
- **`2`** — **additive**. It only signals that the descriptor may carry the Decision decline-gating fields (`commitment.allow_decline_over_approval`, `objection_handling.critical_objection_action`); the new fields are optional and default to legacy behaviour, so a `schema_version: 1` descriptor stays valid forever.
- **`3`** — the first **semantic** version. It changes how existing fields are evaluated rather than adding any: every voting algorithm other than `none` becomes binding on an empty decisive tally (see "No decisive votes" under Decision Mode below). The same `rules` bytes therefore evaluate differently at `2` and at `3`. §3 SHOULDs that a new policy using a voting algorithm other than `none` declare `3`.

Which semantics apply is a property of the **stored descriptor**, never of the runtime release (§8): two sessions started by the same binary, one binding a `schema_version: 1` policy and one a `schema_version: 3` policy, evaluate the empty tally differently, and versions 1 and 2 keep the legacy reading forever so stored sessions replay identically. Every rejection **of the definition itself** — including a `policy_id` under the reserved `policy.std.` prefix that is not the canonical definition (see below) — is reported with `INVALID_POLICY_DEFINITION` at the head of the message, because `RegisterPolicyResponse` carries no structured error code. A duplicate `policy_id` is the one rejection that carries no such prefix: the descriptor may be entirely valid and the only problem is that the id is taken, so it is a conflict rather than an invalid definition.

Both routes into the registry apply the same checks: the `RegisterPolicy` RPC and the `MACP_POLICIES_DIR` preload, which funnels through the same `register` path. "The same checks" means the same set for a given `mode` — as the Quorum rows above note, which checks run at all still depends on the policy's `mode`.

### Validating a policies directory before startup

A `MACP_POLICIES_DIR` file that fails any check aborts startup, and loading stops at the first rejection. To check a directory without starting the server, run the binary with `MACP_POLICIES_DRY_RUN=1`:

```bash
MACP_POLICIES_DRY_RUN=1 MACP_POLICIES_DIR=/etc/macp/policies macp-runtime
```

It reports every file by name — `OK` or `REJECTED` with the reason — and exits `0` if the directory would load, `1` otherwise. Nothing is bound, opened, or replayed. Run it before upgrading a runtime whose policies directory predates a release that tightened registration.

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
| `weighted` | Weighted approve share `>= threshold`; the `weights` map **is** the electorate, so a declared participant absent from it weighs `0` and is non-decisive |
| `plurality` | More approve than reject; a tie fails; no threshold |

- **Denominator.** For `majority`, `supermajority` and `weighted` the denominator is the *decisive* votes -- those cast as approve or reject. Abstentions are excluded and neither help nor hinder the ratio. Under `weighted`, so are the ballots of participants outside the `weights` map -- see the next bullet.
- **The `weights` map is the weighted electorate.** Under `weighted`, a declared participant absent from `weights` has weight `0` (an observer is expressed by omission; registration refuses an explicit `0` and an empty map). Such a ballot is accepted as a message and kept in history, but it is **non-decisive**: it contributes to neither side of the weighted ratio, it does not enter the decisive tally, and it does not satisfy the decline guard. It still counts as a vote cast for the `voting.quorum` participation floor, which this rule leaves alone. A ballot set cast entirely by unlisted participants therefore has a total decisive weight of `0`, which *is* the empty decisive tally -- see the two "no decisive votes" bullets below. This rule is keyed on nothing: RFC-MACP-0012 §4.1 states it is "normative for **every** schema version", and `decision-rules.schema.json` spells out "normative for EVERY schema version, not only 3". It is one of two rules in this release that can change how an already-stored session replays -- the other is the decline guard's reach onto a `Passed` round, two bullets below; §8's "Bounded exception -- weight-`0` decisiveness" accepts *this* one in writing (it does not reach the other), and [Deployment](deployment.md#6-the-weights-map-is-now-the-weighted-electorate-and-this-one-can-break-a-stored-session) carries both predicates, the operator-facing blast radius and a pre-upgrade audit query. Under every other algorithm `weights` is not consulted and none of this applies.
- **A negative weighted total fails the round.** If the weights of the decisive voters sum below zero, `weighted` fails the round outright rather than dividing by a negative denominator, which would invert `ratio >= threshold` and could report a pass on a reject. Registration refuses a negative entry in `voting.weights` (`exclusiveMinimum: 0` excludes negatives a fortiori), so this is reachable only from a `PolicyDefinition` constructed directly rather than registered. A total of exactly `0.0` is treated as no decisive result, not as a failure -- see the two "no decisive votes" bullets below. The operational consequences of the change, including the one commitment it moves from denied to allowed, are in [Deployment](deployment.md#4-a-negative-weighted-total-now-fails-the-decision-round).
- **Inclusive comparison.** Every threshold comparison is `ratio >= threshold`, so `majority` at its default `0.5` approves an even split. A rule where a tie fails is `plurality`, not `majority` at `0.5`.
- **Ratios are binary64.** Comparisons are Rust `f64`. With `threshold: 0.6666666666666666` (the binary64 value nearest two-thirds, and what `2.0 / 3.0` produces) 2-of-3, 4-of-6, 20-of-30 and 67-of-100 pass while 66-of-100 does not.
- **`voting.quorum` is inert on its own.** It states the participation bar but gates nothing until `commitment.require_vote_quorum` is `true`. A policy that sets `voting.quorum` without it imposes no participation requirement.
- **No decisive votes (`schema_version >= 3`).** With any algorithm other than `none`, an empty decisive tally is binding on its own: a *positive* commitment is denied whatever `commitment.require_vote_quorum` says (RFC-MACP-0012 §4.1, "Empty tally"). The denial names the algorithm and the version. The runtime reports the result as *no votes*, never as a failed round, because §4.1 requires that so two conformant runtimes agree on what they observed -- and because `unanimous`'s "every declared participant approved" predicate is *vacuously true* over an empty participant list, which §4.1 says must not be read as a pass.
- **No decisive votes (`schema_version <= 2`), the legacy rule.** At versions 1 and 2 the algorithm instead produces *no result* on an empty tally, and whether that blocks a *positive* commitment is governed entirely by `commitment.require_vote_quorum` -- so a version-1 or -2 policy that means its voting algorithm to be binding must set it. This arm is fail-open and is retained **solely** so that stored sessions replay identically (§4.1 "Retaining the legacy arm", §8); §4.1 also forbids applying it to `schema_version >= 3`. The selector is the *policy's* declared version, so both readings are live in the same binary at the same time.
- **A *vote-authorized* decline on an empty tally is denied at every schema version**, because such a decline needs at least one *decisive* explicit reject and an empty tally has none (RFC-MACP-0007 §6.2). The schema versions differ only in the positive direction. An *objection-authorized* decline is the exception -- see the next bullet.
- **Vote-authorized versus objection-authorized declines.** A decline whose authorization comes from the tally is *vote-authorized* and is gated by the voting tri-state and the decline guard, as above. When the policy sets `objection_handling.critical_objection_action: "finalize_decline"` and a standing critical objection blocks the positive direction, a decline is instead **objection-authorized**: its authorization is the recorded critical `Objection`, so RFC-MACP-0007 §6.2 exempts it from both the tri-state and the decline guard -- the objection *is* the explicit, attributable dissent the guard exists to require. It is therefore available at every tally, the empty one included, and the allow reason names the veto rather than a vote result. Without this channel a `schema_version >= 3` session with a non-`none` algorithm, an empty tally and a standing critical objection could terminate only by expiry -- precisely the stuck state `finalize_decline` exists to resolve. Three limits. The exemption is one-directional: a *positive* commitment under `finalize_decline` is still denied by the veto, and at `schema_version >= 3` the empty-tally denial is reported alongside it. It is scoped to the action: `deny` (the default) and `hold` remain hard-stops in both directions. And it waives only the tri-state and the reject-floor: `commitment.require_vote_quorum` and the `evaluation.minimum_confidence` gate are evaluated as separate, outcome-agnostic checks and still apply to an objection-authorized decline. That is the fail-closed reading. §6.2 names the tri-state and the decline guard explicitly; the quorum condition sits *inside* its decline-guard sentence, so a wider reading that also waives `require_vote_quorum` is arguable, and this release does not take it.
- **The decline guard counts only decisive rejects, and applies to all three voting results.** RFC-MACP-0007 §6.2: a vote-authorized negative commitment must be backed by at least one decisive explicit `REJECT`, and "the guard applies across all three voting results and at every policy `schema_version`". Two consequences. Under `weighted`, a `REJECT` from a participant outside `weights` does not satisfy it -- so a round with no listed dissenter cannot finalize a decline, whatever the unlisted voters cast. And `commitment.allow_decline_over_approval` waives the *approval result*, not the guard: on a `Passed` round the knob permits a decline only when a decisive reject also exists. An all-approve round, or one whose only rejects are non-decisive, is denied with a reason that names the missing decisive reject rather than the knob. This runtime previously applied the guard on `Failed` and `NoVotes` only, so the `Passed` arm is a behaviour change and, unlike the rest of the guard, it needs no `weights` map to reach: an ordinary `majority` policy with `allow_decline_over_approval: true` and an all-approve tally moves from allow to deny. It is therefore the second of the two rules that can change how an already-stored session replays -- see [Deployment](deployment.md#6-the-weights-map-is-now-the-weighted-electorate-and-this-one-can-break-a-stored-session), predicate B. §8's bounded exception does **not** cover it, because §6.2 carried "applies across all three voting results" before spec #99: this is a pre-existing gap being closed, not new semantics.

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
  "threshold": { "type": "percentage", "value": 66 },
  "abstention": { "counts_toward_quorum": false, "interpretation": "neutral" },
  "commitment": { "authority": "initiator_only" }
}
```

The threshold field is spelled `type`, not `threshold_type`: the latter is the Rust field name, and a policy that uses it silently falls back to the default `n_of_m`.

Threshold types: `n_of_m`, `percentage`, and `count` — a documented alias for `n_of_m` that both the mode and the evaluator already treat as one. `threshold.value` must be a non-negative integer, and at most `100` for `percentage`.

`weighted` is **refused**, and the canonical vocabulary now agrees. RFC-MACP-0012 1.2.0-draft (spec #110) removed it from `threshold.type`'s enum and **reserved** the identifier: it was enum-legal but never had a weights map, an electorate rule, or a weighted analogue of RFC-MACP-0011 §5's count-only termination arithmetic, so no conformant evaluation of it ever existed. It must not be reused with a different meaning. This runtime had refused it as unimplemented since before the removal, so nothing changes here except the reason: `weighted` now fails the ordinary enum check rather than a dedicated arm.

`count` is the one place this runtime still departs from that enum, and the departure is now permanent rather than pending: #110 ruled the alias out of MACP vocabulary for good (spec issue #98 item 4), on the grounds that `count` names a *participation floor* in Decision Mode's `voting.quorum` while Quorum Mode's `threshold` is an *approval bar*, and that policy identity is byte-level `rules` equality (RFC-MACP-0012 §8), so an alias makes two semantically identical policies compare unequal forever. This runtime keeps accepting it because `docs/policy.md` has documented it and both layers already treat it as `n_of_m`; **prefer `n_of_m` in new policies**, and expect `count` to be withdrawn in a future release.

How the threshold resolves to an approval bar (RFC-MACP-0011 §6 — a policy threshold *replaces* the ApprovalRequest's `required_approvals`, it does not supplement it):

- `n_of_m` / `count`: `value` approvals. `percentage`: that share of the **declared participants**.
- Fractional results are **ceiled**, and the bar has a floor of **one approval**. Before this release the mode truncated (`0.5` → `0`) while the evaluator ceiled (`0.5` → `1`), so one policy meant two different bars; a bar of `0` was also reached before any ballot was cast, which let a negative commitment seal with zero approvals. Both layers now resolve through one function (`QuorumThreshold::effective`).
- `value: 0` (the default) leaves the rule **inert**: the ApprovalRequest's own `required_approvals` stands.
- A bar outside `1..=participants` — including an unrecognised `type`, `weighted` among them, which resolves to "unsatisfiable" rather than to a raw count — makes the positive outcome impossible, so the **ApprovalRequest is refused** rather than opening a session that can only decline. Registration already refuses those types; this guard covers a policy edited under a running session.
- A negative commitment needs at least one ballot. RFC-MACP-0011 §4a makes an unreachable threshold the trigger for a decline, but with an empty ballot box "unreachable" only means the bar exceeds the participant pool, which is a misconfiguration rather than a decision.

Abstention interpretations: `neutral`, `implicit_reject`, `ignored`.

## How evaluation works

Each standard mode has a dedicated evaluator in `crates/macp-policy/src/evaluator.rs`. Evaluation runs when a `Commitment` envelope arrives, after the mode's own validation has passed. It is a pure function of three inputs: the resolved policy rules, the accumulated accepted message history, and the session's declared participants. No wall-clock time, external calls, or out-of-session state are involved.

| Evaluator | What it checks |
|-----------|---------------|
| `evaluate_decision_commitment` | Qualifying evaluations meet the confidence threshold, critical objection count stays below veto threshold, vote quorum is met, voting algorithm threshold is satisfied. REVIEW-type evaluations are excluded from confidence checks. |
| `evaluate_proposal_commitment` | Counter-proposal count is within `max_rounds` |
| `evaluate_task_commitment` | Output is present if `require_output` is set |
| `evaluate_handoff_commitment` | Always allows (implicit timeout is handled by the mode) |
| `evaluate_quorum_commitment` | Approval count meets the effective threshold for a positive commitment; a decline is not gated by it. Abstention interpretation is reported, not enforced |

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
| `UNKNOWN_POLICY_VERSION` | The `policy_version` in SessionStart is not found in the registry | FailedPrecondition |
| `POLICY_DENIED` | A commitment is rejected because governance rules are not satisfied | FailedPrecondition |
| `INVALID_POLICY_DEFINITION` | A policy fails one of the [registration checks](#what-registration-checks), or claims a reserved `policy.std.` identifier | InvalidArgument |

Two caveats on that status column, both visible in `Self::status_from_error` (`src/server.rs`):

- On the `Send` path these codes travel **in the `Ack`** -- the RPC itself succeeds, and the rejection surfaces as `ack.ok = false` with `ack.error.code` set to the string above. The gRPC status only applies where an error escapes as a `Status`. `POLICY_DENIED` additionally attaches its structured reasons as `macp-error-details-bin` metadata.
- `RegisterPolicy`/`UnregisterPolicy` report failures in band too (`ok: false` plus an `error` string, since `RegisterPolicyResponse` has no code field), so reserved-namespace rejections carry the literal `INVALID_POLICY_DEFINITION` at the head of that string.

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

This runtime pre-registers all three profiles, so they appear in `ListPolicies` and resolve at `SessionStart`. Provisioning them is optional under §5.2 -- a runtime that ships none of them is still conformant -- but the reservation guarantees that an identifier which *does* resolve resolves to these rules everywhere. All three target `macp.mode.decision.v1` at `schema_version: 1`, and all three set `commitment.require_vote_quorum: true`: under schema-version-1 semantics that flag is what makes the voting algorithm binding on an unvoted positive commitment, without which each profile would be vacuous in exactly the case it exists to govern. Under `schema_version >= 3` the algorithm is binding on its own and the flag's remaining contribution is the `voting.quorum` participation floor (RFC-MACP-0012 §5.2). The profiles stay at `schema_version: 1` regardless -- bumping them would redefine the canonical rules that §2.2 pins byte-identical on every runtime, and a deployment wanting the fail-closed reading registers its own `schema_version: 3` policy instead. In this runtime the three profiles produce the **same decision** at every tally under both readings, and differ only in the deny reason: because all three set `require_vote_quorum: true`, the legacy arm denies an unvoted positive commitment regardless of whether abstentions cleared the participation floor. (An earlier revision of this paragraph claimed a divergence here; it was wrong, and probing the shipped code against the canonical definitions showed all three deny under `schema_version` 1 and 3 alike.)

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
