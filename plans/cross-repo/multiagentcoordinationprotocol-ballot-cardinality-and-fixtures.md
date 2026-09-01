# PLAN — ballot cardinality wording, `message_type` scoping, and conformance corpus gaps (`multiagentcoordinationprotocol`)

**Owning repo:** `multiagentcoordinationprotocol` (the normative spec repo).
**This plan lives in `macp-runtime`** per `/plan`'s Cross-repo work rule. Execute it from inside the spec repo, reading it as `../macp-runtime/plans/cross-repo/multiagentcoordinationprotocol-ballot-cardinality-and-fixtures.md`. A cross-repo *read* needs no gate; every write this plan describes lands inside the spec repo's own root.

**Verified against:** spec repo `110add2` on `main` ("rfcs: state the client-side redelivery contract in RFC-0006 §3.2 (#80)"), 2026-08-31. Working tree carries only three untracked planning files (`ASSUMPTIONS.md`, `DECISIONS.md`, `PROGRESS.md`) — no tracked modifications. Every `file:line` below was read, not recalled.

**Parent:** `plans/conformance-cardinality-program.md` (Waves 0–1). Read its Wave 0 section before starting — it holds the decision this plan implements and the reasoning behind it.

**Issues closed:** #83, #82, #84, #81 here, plus `macp-runtime` #125.

## Context

Four open issues, all authored against a corpus and RFC set that three implementations already agree on. None of them reports a behavioural bug — every one reports **agreement with nothing normative behind it**. That distinction shapes the whole plan: the deliverable is text and fixtures that make existing consensus enforceable, not a change to what anyone does.

The four split cleanly into two independent tracks:

- **Ballot cardinality (#83, and `macp-runtime` #125).** RFC-MACP-0011 §5 rule 3 (`rfcs/RFC-MACP-0011-quorum-mode.md:65`) caps ballots at one per participant but never says which of two stands. "At most one" is satisfied by last-wins. Three implementations enforce first-wins on nothing but convention. This was analysed as a one-way door — see the parent plan's Wave 0 — because the scoping choice is coupled to whether `macp-runtime`'s sender-only ballot keying stays conforming. **Decision taken: per-`request_id` scoping, plus rule 1 hardened into a permanent base-v1 invariant.** Under that wording the runtime is conforming as-is with zero code and zero persisted-format change, and both SDKs' `(request_id, sender)` keying becomes the spec's own scope rather than a defensive over-implementation.
- **Corpus and envelope hygiene (#82, #84, #81).** `message_type` is never stated to be mode-scoped even though two discriminators already collide; and the corpus has no fixture for a *rejected* duplicate ballot, nor any fixture at all for three normatively-specified payload types.

### What the issues get right, and the two things they do not say

Both premises in the issues checked out against the code. Two facts material to sequencing appear in neither:

1. **`macp-runtime` vendors this corpus too, and its CI reads canonical from `main` directly.** The runtime's `conformance-oracle` job (`macp-runtime/.github/workflows/ci.yml:506-531`) checks out this repo at `main` and byte-compares bidirectionally. Both SDKs do the same via `make verify-fixtures`. So a fixture merged here turns **three** downstream repos red — not two — with no local action required to trigger it. Phase S3 is scoped and sequenced around that.
2. **RFC-0011 has no analogue of Decision's rule 2.** RFC-MACP-0007 §5 rule 2 (`:78`) requires `Evaluation`, `Objection`, and `Vote` to reference an existing `proposal_id`. RFC-0011 states no equivalent for ballots and `request_id` — yet `macp-runtime` enforces exactly that (`crates/macp-modes/src/mode/quorum.rs:163,183,203`, pinned by `wrong_request_id_rejected` at `:661`). Per-`request_id` scoping in rule 3 is meaningless without it, so S1 must add the sentence. This is a *third* piece of unwritten-but-enforced behaviour in the same section, found while fixing the first.

Additionally, §2.1 (`:34`) restates the same weak "MAY cast at most one ballot" phrasing as rule 3. Fixing rule 3 alone leaves the RFC self-contradicting, so §2.1 is in S1's scope.

## Phases

### S1 — RFC-MACP-0011 ballot cardinality, request scoping, and the v1 invariant

**Status:** TODO — **blocked on owner ratification of the rule 1 hardening** (parent plan, Open question 3).
**Delivers:** RFC-MACP-0011 states which ballot stands, scopes ballots to `request_id`, requires ballots to reference the accepted request, and declares the one-`ApprovalRequest` cap permanent for the v1 line. Closes #83; closes `macp-runtime` #125 with a comment linking this PR.
**Depends on:** the Wave 0 decision (taken) and its ratification (outstanding).
**Files:** `rfcs/RFC-MACP-0011-quorum-mode.md` only.

**Approach.** Four edits, all wording, no fixture or schema change — so this PR breaks nothing downstream and can merge independently of S3.

1. **§5 rule 3** (`:65`) — replace:

   > 3. Each eligible participant MAY cast at most one ballot across `Approve`, `Reject`, or `Abstain`.

   with:

   > 3. Each ballot (`Approve`, `Reject`, or `Abstain`) MUST reference the Session's accepted `request_id`; a runtime MUST reject a ballot that references any other `request_id` or that precedes the accepted `ApprovalRequest`. Each eligible participant MUST cast at most one ballot per `request_id`, counted across `Approve`, `Reject`, and `Abstain` combined. A runtime MUST reject a second ballot from the same sender for the same `request_id`, regardless of the type of either ballot; the first accepted ballot stands. Configuration or policy MAY bind a *stricter* rule (for example, restricting which participants may ballot at all), but MUST NOT relax this one. A permissive re-ballot or last-wins rule would need replacement semantics that this mode does not define, and without them two conforming implementations could derive different quorum state from identical accepted history — which Section 7's semantic-deterministic claim forbids.

   Note this folds in the missing rule-2 analogue (first sentence) rather than adding a separate numbered rule, keeping the existing numbering stable — renumbering would churn every cross-reference to §5 in three repos.

2. **§5 rule 1** (`:63`) — replace:

   > 1. A Session MUST accept at most one `ApprovalRequest` in base v1.

   with:

   > 1. A Session MUST accept at most one `ApprovalRequest` in base v1. This cap is a design invariant of `macp.mode.quorum.v1`, not a provisional restriction: no `configuration_version`, `policy_version`, or `mode_version` within the v1 line may relax it. Multi-request quorum, if ever standardized, would arrive as a new mode revision with its own identifier and validation rules, and MUST NOT be introduced by reinterpreting this document. Because at most one `request_id` can ever be accepted per Session, the per-`request_id` scope in rule 3 is equivalent to per-Session scope throughout base v1, and an implementation MAY rely on that equivalence in its internal state.

   The final sentence is what makes `macp-runtime`'s sender-only keying explicitly conforming rather than accidentally so. Do not drop it as redundant — it is the entire reason the runtime needs no change.

3. **§2.1** (`:34`) — replace the summary sentence so it stops contradicting rule 3:

   > Each eligible participant casts at most one ballot across `Approve`, `Reject`, or `Abstain`; the first accepted ballot stands (Section 5, rule 3). Runtimes MUST reject messages from senders not authorized per this matrix.

4. **§3** (`:49`) — leave "Base Quorum Mode v1 assumes exactly one approval request per Session" as-is. It is now backed by rule 1's hardened text rather than standing alone. Verify it does not need a cross-reference; add one only if the surrounding prose reads as unsupported without it.

**What was rejected.** Bare per-Session scoping (semantically wrong — ballot payloads carry `request_id`, so the request is the natural unit, and per-Session bakes an implementation shortcut into the data model). Rewriting rule 3's cap into a MUST (issue #83 is explicit that this is *not* a MUST/MAY problem: §5 opens at `:62` with "Implementations MUST enforce the following:", so the cap is already mandatory — rewriting it would be churn against a rule that is already correct). Pinning a specific error code for the duplicate-ballot rejection (the runtime surfaces `InvalidPayload`/`INVALID_ENVELOPE`, but Decision's PR #79 text leaves its code unpinned too; consistency wins, and pinning it would over-constrain transports).

**Edge cases & failure modes.**
- A ballot arriving *before* any `ApprovalRequest` is accepted — covered by rule 3's new first sentence ("or that precedes the accepted `ApprovalRequest`"). Already pinned by an existing fixture in `schemas/conformance/quorum_reject_paths.json`; confirm the new wording does not contradict what that fixture asserts.
- Cross-type duplicates (`Reject` after an accepted `Approve`) — rule 3 now says "regardless of the type of either ballot" explicitly, because the cap is across all three types combined and a reader could otherwise take per-type to be the unit.
- The rule 1 hardening is the irreversible part. Once published, walking it back to allow multi-request in v1 is itself a breaking spec change.

**Acceptance criteria.**
1. `rfcs/RFC-MACP-0011-quorum-mode.md` §5 rule 3 contains the phrase "the first accepted ballot stands" and scopes the cap "per `request_id`".
2. §5 rule 3 states the requirement that a ballot reference the Session's accepted `request_id`.
3. §5 rule 1 states that no `configuration_version`, `policy_version`, or `mode_version` within the v1 line may relax the one-`ApprovalRequest` cap.
4. §5 rule 1 states the per-`request_id`/per-Session equivalence that permits sender-keyed internal state.
5. §2.1 no longer contains the bare "MAY cast at most one ballot" phrasing.
6. Rule numbering in §5 is unchanged (1–6, with 4a/4b intact) — no cross-reference elsewhere in the corpus breaks.
7. No file outside `rfcs/` is modified; `schemas/conformance/` is byte-identical to `main`, so all three downstream drift guards stay green.
8. `macp-runtime` #125 is closed with a comment quoting the rule 1 answer and linking this PR.

**Tests.** No executable tests in this repo for RFC prose. The verification is (a) criteria 1–7 checked by reading the diff, and (b) a grep across `rfcs/`, `docs/`, and `schemas/` for any surviving text that still describes ballot cardinality in the old weak form — the §2.1 duplicate was found that way and there may be others in `docs/`.

**Docs.** Check `docs/` for any quorum-mode summary page that restates §5; update in the same commit if found.

---

### S2 — `message_type` is mode-scoped

**Status:** TODO. Independent of S1 and S3 — may go first, or in parallel.
**Delivers:** the specification states that `message_type` is meaningful only relative to `mode`. Closes #82.
**Depends on:** nothing.
**Files:** `rfcs/RFC-MACP-0001-core.md`, `schemas/json/macp-envelope.schema.json`.

**Approach.** The rule already exists — encoded, not written. `schemas/conformance/schema.json` requires a `payload_type` on every fixture message matching `^(macp\.v1\.[A-Za-z]+|macp\.modes\.[a-z_]+\.v\d+\.[A-Za-z]+Payload)$`, which is mode-qualified by construction. Two discriminators already collide at the *declaration* level, not merely in fixtures: `ProposalPayload` is declared by both `decision` and `proposal`, and `RejectPayload` by both `proposal` and `quorum`. Every other declared payload name is unique across modes.

Add a normative sentence to RFC-MACP-0001 §6, adjacent to the existing constraint at `:166` ("`message_type` MUST be non-empty"), stating that `message_type` is scoped to the Envelope's `mode` and that `(mode, message_type)` is the discriminating key; a bare `message_type` does not identify a payload type. Then update the `message_type` description in `schemas/json/macp-envelope.schema.json` to match — it currently reads "Message type discriminator (e.g., Signal, SessionStart, Commitment)" with no scoping note, and its examples are all mode-independent types, which is precisely what makes the omission easy to miss.

**Explicitly out of scope:** creating a message-type registry. The issue notes there is none; building one is a much larger change with its own governance questions, and the gap being closed here is a missing *statement*, not missing *infrastructure*. If a registry is wanted, it is a separate RFC.

**Edge cases & failure modes.**
- Mode-independent types (`Signal`, `SessionStart`, `Commitment`, `CancelSession`) are meaningful across all modes and must not be described as mode-scoped in a way that makes them non-conforming. The wording must scope the *interpretation* of `message_type` to `mode` without asserting that every type is mode-specific. Signals in particular carry an **empty** `mode` by rule (per `macp-runtime`'s CLAUDE.md §7 and the runtime's validation), so any phrasing that requires a non-empty `mode` to interpret `message_type` would make every conforming Signal non-conforming. Handle this explicitly — it is the one way this phase can go wrong.
- Check whether `payload_type` should be described in the envelope schema at all. The issue notes it appears nowhere in `envelope.proto` or the envelope JSON Schema — it is a *fixture*-level field only. Do not accidentally imply it is a wire field.

**Acceptance criteria.**
1. RFC-MACP-0001 §6 states that `message_type` is interpreted relative to `mode`, and that `(mode, message_type)` is the discriminating key.
2. The statement explicitly accommodates mode-independent types and empty-`mode` Signals without making either non-conforming.
3. `schemas/json/macp-envelope.schema.json`'s `message_type` description carries the scoping note.
4. No message-type registry is introduced.
5. `schemas/conformance/` is untouched — downstream drift guards stay green.

**Tests.** If `schemas/json/` has a schema-lint or example-validation step, run it. Confirm the envelope schema still validates every fixture in `schemas/conformance/`.

**Docs.** Any `docs/` page describing the envelope's fields.

---

### S3 — conformance fixtures: duplicate ballots/votes, and the three unfixtured payloads

**Status:** TODO.
**Delivers:** reject-path fixtures for duplicate `Vote` and duplicate ballot, and the first-ever fixtures for `ObjectionPayload`, `WithdrawPayload`, and `TaskUpdatePayload`. Closes #84 and #81.
**Depends on:** **S1** — a fixture pinning "the first accepted ballot stands" must not merge before the sentence it pins exists. #84's content is otherwise independent.
**Files:** `schemas/conformance/decision_reject_paths.json`, `schemas/conformance/quorum_reject_paths.json`, plus fixtures for the three payload types (extend the existing per-mode files or add new ones — decide from how the harness discovers fixtures, and prefer extending, since three downstream harnesses enumerate this directory).

**⚠ This is the fan-out PR.** Merging it turns `macp-runtime`, `macp-sdk-typescript`, and `macp-sdk-python` CI red simultaneously, because all three byte-compare against this directory. See the parent plan's Wave 2. **Do not merge until all three vendoring PRs are open and green against this branch.** Coordinate the landing; do not treat it as an ordinary docs-adjacent merge.

**Approach.** Two groups of fixtures.

*Group A — duplicate reject paths (#84).* Both files already contain accept-path and single-vote/ballot reject shapes (e.g. `quorum_reject_paths.json`'s first message rejects an `Approve` arriving before any `ApprovalRequest` exists), but none models an **accepted first** followed by a **rejected duplicate** from the same sender. Add:
- `decision_reject_paths.json` — a second, distinct `Vote` from a sender who already cast one for the same `proposal_id`, `"expect": "reject"`, `"expected_error_code": "INVALID_ENVELOPE"`. Pins RFC-MACP-0007 §5 rule 3.
- `quorum_reject_paths.json` — a second, distinct ballot from a sender who already cast one for the same `request_id`, including **at least one cross-type case** (`Reject` after an accepted `Approve`), since the cap is across all three types combined, not per-type. This is the case the existing corpus most conspicuously omits and the one an implementer is most likely to get wrong.

Each fixture must contain the *accepted* first vote/ballot as a preceding message, or the duplicate has nothing to be a duplicate of.

*Group B — unfixtured payloads (#81).* Author the first fixtures for three types that have zero coverage today, honouring the non-obvious rules that make each worth fixturing:
- **`Objection`** (decision) — RFC-MACP-0007: participant-matrix row (`:38`), explicitly distinct from a `BLOCK` Evaluation (`:62-64`), severity `low|medium|high|critical`, interaction with `objection_handling` governance including `critical_severity_vetoes`, validation rule at `:78`. Cover at minimum a valid Objection and the `:78` reject path. This is the highest-value fixture in the whole plan: `has_blocking_objection` feeds veto governance in both SDKs and has **no cross-implementation oracle** today.
- **`Withdraw`** (proposal) — RFC-MACP-0008 `:32`: a `CounterProposal` mints a *new* `proposal_id`, and only the sender of that `CounterProposal` may withdraw it. Validation rule at `:68`. Fixture the authorship rule specifically — it is the non-obvious part.
- **`TaskUpdate`** (task) — RFC-MACP-0009 `:31`: active-assignee-only, with authorship validated via the Envelope `sender` rather than a payload field, explicitly unlike `TaskComplete`/`TaskFail` which carry `assignee` redundantly (`:58`). Validation rule at `:73`. Fixture a non-assignee `TaskUpdate` reject path.

**Edge cases & failure modes.**
- **Downstream harness capability is a prerequisite, not an afterthought.** Before authoring, confirm each of the three vendoring repos can actually replay these shapes: a `"expect": "reject"` message with an `expected_error_code`, and payload decoding for `Objection`/`Withdraw`/`TaskUpdate` — types no fixture has ever exercised, so their decode paths are unproven in every implementation. If a harness cannot replay them, that repo's Wave 2 phase grows a harness change and S3's merge waits for it. **Discover this before merging, not after three CIs go red.**
- `macp-sdk-typescript`'s `tests/conformance/duplicate-ballots.ts` guard asserts no canonical fixture contains a duplicate **accepted** vote or ballot, deliberately scoped to `expect === 'accept'` with a comment stating a rejected duplicate is legitimate content the corpus lacks. Group A is exactly what that comment anticipated — but confirm by reading the code that the scoping is real and the guard welcomes rather than blocks it.
- Run `schemas/conformance/lint_fixtures.py` before opening the PR; every new fixture must satisfy `schemas/conformance/schema.json`, including the `payload_type` pattern (which S2 documents but does not change).
- Fixture messages need coherent session state — a duplicate ballot fixture needs a preceding `SessionStart` and `ApprovalRequest` with matching bound versions, or it fails for the wrong reason and pins nothing.

**Acceptance criteria.**
1. `decision_reject_paths.json` contains an accepted `Vote` followed by a rejected second `Vote` from the same sender for the same `proposal_id`.
2. `quorum_reject_paths.json` contains an accepted ballot followed by a rejected second ballot from the same sender for the same `request_id`, including at least one cross-type pair.
3. At least one fixture exercises each of `ObjectionPayload`, `WithdrawPayload`, and `TaskUpdatePayload`, covering both a valid instance and the RFC-cited reject path for each.
4. `lint_fixtures.py` passes and every new fixture validates against `schemas/conformance/schema.json`.
5. All three downstream repos have an open, green vendoring PR before this merges.
6. `schemas/conformance/README.md` is updated if it makes any coverage claim this changes.

**Tests.** `lint_fixtures.py`, plus replay against all three implementations from their vendoring branches — that cross-replay *is* the test, and is the entire reason the corpus exists.

**Docs.** `schemas/conformance/README.md`. The README already flags deliberate omissions where they exist (the `cmt-hash/` "it is not an oversight" note), which is what made these three read as a gap rather than a scoped decision — consider whether closing them warrants a positive statement of what the corpus is meant to cover.

## Long-term posture

- **Rule 1's hardening is the one-way door.** Everything else here is additive and reversible. Once published, permitting multi-request quorum inside v1 becomes itself a breaking change; the sanctioned path becomes a new mode identifier. This is recommended deliberately — the ecosystem already depends on it structurally (`macp-runtime`'s `request: Option<ApprovalRequestRecord>` cannot represent two requests at all) — but it should be ratified consciously, not absorbed.
- **Three vendored copies of one corpus.** S3 pays the coordination cost of that design for the first time in this program. It is the right design for hermetic test runs, but nobody should add a fourth vendoring implementation without a better sync mechanism than three Makefiles and a CI job.
- **The corpus has no stated coverage policy.** #81 is one instance of a general absence. `Vote` cardinality had fixtures, three implementations, and mechanical guards; `Objection` severity, which can veto a decision outright, had one implementation's unit tests. Closing three specific gaps does not prevent the fourth. Worth a follow-up issue proposing an explicit policy in the README.

## Open questions

1. **Ratification of the rule 1 hardening.** The only genuine fork. Declaring the one-`ApprovalRequest` cap unrelaxable within v1 forecloses evolving v1 in place toward multi-request quorum. Recommended without hedging, but there is no wording that preserves both options, and it is the protocol owner's call. **S1 does not start until this is answered.**
2. **Whether the rule 1 equivalence note belongs inline or as a §5 trailing note.** Pure style; inline is assumed. Not blocking.
3. **Whether Group B fixtures extend the existing per-mode files or land as new files.** Decide from how the three downstream harnesses enumerate the directory — extending is assumed as lower-risk, since a new filename must be picked up by three separate vendoring mechanisms.
