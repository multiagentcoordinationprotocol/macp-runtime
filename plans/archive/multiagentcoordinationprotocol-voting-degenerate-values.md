# CROSS-REPO — RFC amendments needed before macp-runtime can close #147 and #149

**Owning repo:** `multiagentcoordinationprotocol` (spec). **Requested by:** `macp-runtime`.
**Parent plan:** `plans/backlog-closeout-2026-09.md` (this repo).
**Authorization in force:** macp-runtime may file **issues only** upstream — no PRs, no commits, no
file edits (user, 2026-09-10).
**FILED: spec issue #98** — https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/98 (2026-09-10). The spec-repo Claude session was signalled the same day.

## Why

Three fail-open behaviours in the reference runtime are either **normatively specified** or
**schema-legal**, so the runtime cannot fix them without the spec moving first. All three were
reported by an external consumer (`zer07labs/seam-runtime`) that reproduced them live.

## Item 1 — RFC-MACP-0012 §4.1 "No decisive votes" is fail-open

**Current normative text** (`rfcs/RFC-MACP-0012-policy.md:113`): with `require_vote_quorum` false,
a positive commitment is not blocked by the absence of votes, **even under `majority` or
`unanimous`**. Restated at `:211` as the reason all three `policy.std.*` profiles set the flag.

**Observed consequence:** 0 ballots SEAL a `unanimous` policy; 1 APPROVE of 2 is REFUSED. Adding an
approving ballot flips ALLOW to DENY. The `unanimous` predicate — "every declared participant voted
APPROVE" — is decidable on an empty tally and is **false**; it simply never runs, because the
evaluator short-circuits before dispatch.

**Proposed amendment:** run each algorithm's predicate on the empty tally rather than short-circuiting.
`unanimous` and every ratio-with-threshold algorithm FAIL on zero ballots; keep a zero-denominator
guard inside `majority` only, where the arithmetic requires it. `none` is unaffected.

**Blast radius the spec owner must weigh:** this changes the meaning of every existing policy that
omits `require_vote_quorum`, and makes the flag's current documented purpose largely redundant.

## Item 2 — `voting.threshold: 0.0` is schema-legal and makes an all-REJECT round pass

`schemas/json/policy/decision-rules.schema.json` declares `voting.threshold` as
`{"type":"number","minimum":0,"maximum":1,"default":0.5}` — `0.0` is **in-schema**. With it, an
all-REJECT round gives `ratio = 0.0 >= 0.0` → `Passed`, on both `majority` and `weighted`.
`supermajority` is already protected by `exclusiveMinimum: 0.5` (`:152`), which shows the intended
shape.

**Proposed amendment:** change `minimum: 0` to `exclusiveMinimum: 0` for `voting.threshold`, so a
zero bar is unauthorable rather than silently permissive. Worth deciding separately whether `> 1.0`
should stay refused (already out-of-schema; fail-closed, but still an authoring error).

## Item 3 — all-zero `voting.weights` is schema-legal and yields "no votes" on a full ballot set

`voting.weights.additionalProperties` is `{"type":"number","minimum":0}` — zero weights are
**in-schema**. `{"a":0.0,"b":0.0}` with both participants voting gives `weighted_total == 0.0`, which
the evaluator reports as "no votes" and, combined with Item 1, ALLOWs.

**Proposed amendment:** require the weight **sum** to be positive (or make individual weights
`exclusiveMinimum: 0`). Note negative weights are *already* out-of-schema, so only the zero case
needs new text.

## What macp-runtime ships without waiting

Phase 2 of the parent plan enforces, at registration, everything the canonical schemas **already**
say — the algorithm enum, `threshold ∈ [0,1]`, `weights ≥ 0`, and quorum `threshold.value` integrality.
That closes #145's reachable path and #148's negative-weight half with no spec change. Only the three
items above need the spec to move.

## Acceptance for this cross-repo ask

One issue filed in the spec repo covering all three, linking to this file's blob URL on `main` once
the parent plan's PR lands. The runtime fix for Items 1–3 is planned but **not started** and is
explicitly out of scope until the amendment is ratified.
