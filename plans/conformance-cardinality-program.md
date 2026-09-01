# PROGRAM PLAN — ballot cardinality, conformance corpus coverage, and the SDK backlog

**Written:** 2026-08-31.
**Verified against:** `macp-runtime` `08ff766` on `origin/main` (v0.7.1 release commit); spec repo `multiagentcoordinationprotocol` at `110add2` on `main`. Every `file:line` below was read in this session, not recalled.
**Scope:** four repos, thirteen open issues. This file is the *sequencing* authority. Per-repo execution plans live in `plans/cross-repo/` (see Repo ownership).

## Context

Thirteen issues are open across the workspace. None of them is in `macp-runtime` — the only runtime issue, #125, is a question addressed *to* the spec, and the three open runtime PRs are dependabot. The implementable work is concentrated in the spec repo (4 issues) and the two SDKs (6 issues, of which 3 are blocked on the spec).

Two facts discovered while triaging determine the shape of this program. Neither is stated in any of the issues.

### 1. A fixture added upstream turns three downstream repos red

The spec repo's `schemas/conformance/` is the single canonical corpus (18 JSON fixtures + `cmt-hash/`). All three implementations vendor byte-identical copies and enforce that with CI:

| Repo | Vendored at | Guard | Direction |
|---|---|---|---|
| `macp-runtime` | `tests/conformance/` | `conformance-oracle` job, `.github/workflows/ci.yml:506-531` | bidirectional (canonical→vendored loop added in #115) |
| `macp-sdk-typescript` | `tests/conformance/` | `make verify-fixtures`, `Makefile:44-51` | bidirectional |
| `macp-sdk-python` | `tests/conformance/` + `tests/vectors/cmt-hash/` | `make verify-fixtures`, `Makefile:49-83` via `FIXTURE_DIR_PAIRS` | bidirectional |

The runtime's job checks out the spec repo's **`main`** (`ci.yml:514-518`), so it does not even need a local sync to notice. Verified in this session: all three copies are currently in sync with canonical, and the file lists match exactly.

**Consequence.** Spec issues #81 and #84 both add fixtures. Merging them breaks the CI of three repos simultaneously, and stays broken until each vendors. They must therefore be authored and merged as **one spec PR**, so the fan-out break happens once rather than twice, and the three vendoring PRs must follow immediately behind it. This is why the fixture work is a single phase and not two, despite being two issues.

### 2. Spec #83 and runtime #125 are one decision, not two

RFC-MACP-0011 §5 rule 3 (`rfcs/RFC-MACP-0011-quorum-mode.md:65`) caps ballots but never says which of two stands — "at most one" is satisfied by last-wins. Issue #83 asks to close that.

The obvious fix is to mirror RFC-MACP-0007 §5 item 3, tightened in spec PR #79, which reads (`rfcs/RFC-MACP-0007-decision-mode.md:79`):

> A participant MUST cast at most one `Vote` **per `proposal_id`**. A runtime MUST reject a second `Vote` from the same sender for the same `proposal_id`; the first accepted `Vote` stands.

Mirroring that scoping into Quorum yields "per `request_id`". But the runtime keys ballots by **sender alone** — `ballots: BTreeMap<String, BallotRecord>` at `crates/macp-modes/src/mode/quorum.rs:41`, guarded by `state.ballots.contains_key(&env.sender)` in all three arms (`:164` Approve, `:184` Reject, `:204` Abstain). Both SDKs key on `(request_id, sender)`. The two keyings are equivalent today *only* because §5 rule 1 caps a session at one `ApprovalRequest`.

So the wording chosen for #83 either ratifies sender-only keying permanently, or writes a latent non-conformance into the reference runtime. That is precisely the question runtime #125 asks. **#125 cannot be answered separately from #83, and #83 cannot be drafted before #125 is decided.**

This is a one-way door (a public protocol contract, and — because `QuorumState` is `Serialize`/`Deserialize` and replayed from an append-only log — potentially a persisted-format change). It is routed to a Fable analysis before any text is written. Its outcome is the only input this program is waiting on.

### 3. Findings from the per-repo planning passes that change scope

The two SDK planning passes verified their issues against code rather than trusting them, and three findings change this program's shape:

**a. ~~Only one implementation of three replays reject-path fixtures.~~ CORRECTED 2026-09-01 — there is no harness gap.** The original finding misread both SDKs, and the correction *reduces* scope, so it is recorded rather than quietly dropped.

`macp-runtime` is indeed the only implementation that asserts the rejection itself (`tests/conformance_loader.rs:442-471`, matching `expected_error_code`). But that asymmetry is **correct by construction, not a deficiency**: only a runtime rejects, so only a runtime can be an oracle for the rejection decision. An in-process client projection cannot produce a NACK.

Both SDKs already handle reject fixtures deliberately:
- **Python** (`tests/conformance/test_conformance_projections.py:150-159`) replays the accepted *prefix* and skips individual rejected *messages* — commented "rejection is runtime-side ... in lockstep with the TypeScript harness." The earlier reading took `if msg.get("expect") != "accept": continue` as skipping the whole fixture; it skips one message.
- **TypeScript** (`tests/conformance/conformance.test.ts:379-401`) has a reject-path block that **does run**, asserting every reject message carries a canonical `expected_error_code` and a resolvable `payload_type`. Its single `it.skip` is documented and correct for the reason above.

**Consequence: Wave 2's harness-work prerequisite does not exist.** The three repos still must vendor the new fixtures, but no harness changes gate that. One genuine parity gap did survive the correction — Python never read `expected_error_code` at all — closed by `macp-sdk-python` #53.

**b. Five of the ten "no normative rule" sites in TS #59/#60 do have one, and three are shipped violations.** Both issues assert that none of the ten has a confirmed rule behind it. That is wrong for five: Proposal `Accept` supersession (RFC-0008 `:70` plus the determinism MUST at `:89`), `TaskAccept` first-accept-wins (RFC-0009 `:70`/`:72`), Handoff accept/decline settle-once (RFC-0010 `:68`, `:113-116`), and post-`Commitment` terminality (RFC-0001 `:216`/`:238`/`:247`). Three are violated in shipped code today — most concretely `ProposalProjection.acceptedProposal()` (`proposal.ts:201-207`) returns `undefined` where the acceptance set is unambiguous after a legal re-accept. **Wave 4 is therefore no longer triage-only**: it splits into real fixes for the sites with rules, and spec issues for the two genuinely ambiguous ones.

**c. `macp-sdk-python` 0.8.0 already shipped** (2026-08-31; HEAD is the release commit, PyPI latest is 0.8.0). The planned "cut 0.8.0" phase is stale and the SDK-divergence concern is **resolved** — both SDKs are at 0.8.0 and agree on vote/ballot cardinality. What remains is a real but different defect: release-please never consumed the hand-written `## Unreleased` block (`CHANGELOG.md:3`), inserting `## [0.8.0]` *below* it at `:162`, so shipped work is labelled unreleased and will recur every release.

### What is NOT in scope

- ~~TS #59 and #60 as triage-only.~~ **Superseded by finding (b) above.** The RFC read is done and five of ten sites have governing rules; those become code. Only the two genuinely ambiguous sites (`TaskComplete`/`TaskFail` accumulation) stay triage-and-file-a-spec-issue. Three sites are silent *coherently* and need nothing.
- **`macp-playground` #21** (specialists return `REVIEW`, conservative policies never finalize) and **`macp-ui-console` #17** (Jaeger trace proxy 502 on Railway). Both predate this work by two months, sit in unrelated domains, and share no dependency with anything here.
- **`auth-service`** and **`website`** — no open issues, and no MACP dependency (auth-service) / self-syncing (website).

## Repo ownership and the cross-repo rule

`/implement` is single-repo for writes: every phase's edits, commits, and test runs stay inside one repo root, and a cross-repo edit needs explicit per-change approval. This program therefore runs as **four separate `/implement` invocations**, one per repo, each from inside that repo.

Per `/plan`'s Cross-repo work rule, the plans for the other three repos are written **here**, in `macp-runtime`, and read from there — a cross-repo *read* needs no gate, a cross-repo *write* does:

| Repo | Plan file | Read by that repo as |
|---|---|---|
| `multiagentcoordinationprotocol` | `plans/cross-repo/multiagentcoordinationprotocol-ballot-cardinality-and-fixtures.md` | `../macp-runtime/plans/cross-repo/…` |
| `macp-sdk-typescript` | `plans/cross-repo/macp-sdk-typescript-conformance-and-transport.md` | `../macp-runtime/plans/cross-repo/…` |
| `macp-sdk-python` | `plans/cross-repo/macp-sdk-python-examples-docs-and-release.md` | `../macp-runtime/plans/cross-repo/…` |
| `macp-runtime` | this file, Wave 2 phase R1 | — |

## Waves

Waves are ordered by dependency, not priority. Wave 3 has no dependency on Waves 1–2 and may run concurrently from the start.

### Wave 0 — decide the one-way door — **DECIDED (pending one owner ratification)**

**Status:** DONE — Fable analysis complete and **ratified by the protocol owner**, 2026-08-31. Rule 1 is hardened as recommended.

**Decision: scoped-plus-invariant-note.** Rule 3 is scoped **per `request_id`** (mirroring RFC-0007's per-`proposal_id` house style, established by PR #79), and rule 1 is **hardened into an explicit permanent base-v1 invariant** stating that no `configuration_version`, `policy_version`, or `mode_version` within the v1 line may relax the one-`ApprovalRequest` cap, and that multi-request quorum, if ever standardized, arrives as a new mode revision with its own identifier.

Why this and not the alternatives: while rule 1 holds, per-`request_id` and per-Session scoping are *extensionally identical*, so the wording costs nothing today. Bare per-Session scoping would be semantically wrong on its own terms — ballot payloads carry `request_id`, so the natural unit of a ballot is the request, and encoding "per Session" bakes today's implementation shortcut into the protocol's data model. The latent-defect objection to per-`request_id` scoping dissolves once rule 1 is unrelaxable within v1: sender-only keying can never be *observed* to differ from `(request_id, sender)` keying inside v1, and any future multi-request mode is by definition a new revision.

**Answer to runtime #125: yes, the cap is permanent — for the `macp.mode.quorum.v1` line.** Multi-request quorum is not forbidden forever, it is forbidden as an *evolution of v1*. This ratifies intent the RFC already leans on in two places (§3 "Base Quorum Mode v1 assumes exactly one approval request per Session", and rule 1's existing "in base v1"). It goes **into the RFC text**, not just an issue comment — the entire point of #83 is that three implementations converged on unstated behaviour, and the fix for unstated behaviour is normative text.

**Consequence for `macp-runtime`: conforming as-is. Zero code changes, zero persisted-format changes.** The sender-keyed `BTreeMap<String, BallotRecord>` (`quorum.rs:41`) is behaviourally indistinguishable from `(request_id, sender)` keying under rule 1: every ballot arm first requires `payload.request_id == request.request_id` against the session's sole request (`:162-165`, `:182-185`, `:202-205`), and `BallotRecord` stores the `request_id` it was cast for (`:33`). `QuorumState`'s serde shape is untouched, so there is no replay or migration cost. This is the outcome that keeps Wave 2's R1 to pure fixture vendoring instead of a state re-key.

**Consequence for both SDKs: no behaviour change.** Both already implement exactly the proposed rule — TypeScript `src/projections/quorum.ts:24` (`Map<string, Map<string, BallotRecord>>`, first-wins at `:140-158`), Python `src/macp_sdk/quorum.py:57` (`dict[str, dict[str, BallotRecord]]`, "kept first ballot" at `:113`). Under the new text their keying stops being "stricter than required" and becomes the spec's own scope. Cosmetic follow-up only: the comments at `projections/quorum.ts:153-156` and its Python mirror, which currently say first-wins "is not stated" by RFC-0011, go stale and should cite rule 3 directly.

**Two findings that change Wave 1's scope:**
1. **RFC-0011 has no analogue of Decision's rule 2** ("MUST reference an existing `proposal_id`") — yet the runtime enforces exactly that, pinned by `wrong_request_id_rejected` (`quorum.rs:661`). Making per-`request_id` scoping meaningful requires stating it, so S1 gains that sentence.
2. **§2.1 repeats the same weak "MAY cast at most one ballot" phrasing** as rule 3 and must be aligned in the same PR, or the RFC contradicts itself.

**Owner ratification: GRANTED (2026-08-31).** Rule 1 is hardened as written. Recorded for the record: hardening rule 1 is a real commitment: a hypothetical "v1.1 with multiple requests" becomes impossible, and the only escape hatch is a new mode identifier. Fable recommends making it without hedging — the ecosystem already depends on it structurally (`request: Option<ApprovalRequestRecord>` at `quorum.rs:41` cannot represent two requests at all; `commitment_ready` counts ballots globally at `:91-97`; §3 already states the assumption). But if the protocol owner holds concrete intent to evolve v1 in place toward multi-request, the rule 1 text must be rejected and the runtime's state shape reworked now instead. **There is no wording that preserves both.** The owner ratified the hardening, so S1 proceeds with the text above.

**Left unpinned, deliberately:** the duplicate-ballot error code. The runtime surfaces `InvalidPayload`/`INVALID_ENVELOPE`; Decision's PR #79 text also leaves the code unpinned, so leaving it unpinned is consistent.

### Wave 1 — spec repo (`multiagentcoordinationprotocol`)

Three PRs, in this order. Phases S1 and S2 are wording-only and break nothing downstream; S3 is the fan-out.

- **S1 — RFC-MACP-0011 which-ballot-stands + rule 1 invariant (#83, and the #125 answer).** Depends on Wave 0 (decided). Wording only, four edits: rule 3 rewritten with per-`request_id` scoping and first-wins; rule 1 hardened with the permanence note; a new "MUST reference the Session's accepted `request_id`" sentence (the missing analogue of Decision's rule 2); and §2.1's summary sentence aligned so it stops contradicting rule 3. Note the issue's own constraint: this is *not* a MUST/MAY problem — §5 opens with "Implementations MUST enforce the following", so the *cap* is already mandatory; the gap is which-stands. Closes #83; closes #125 with a comment linking the PR.
- **S2 — `message_type` is mode-scoped (#82).** Independent of S1. RFC-MACP-0001 §6 constrains `message_type` exactly once, at `:166` ("MUST be non-empty"), and `schemas/json/macp-envelope.schema.json` describes it without mode-scoping. Two discriminators already collide in the corpus at the declaration level: `ProposalPayload` is declared by both `decision` and `proposal`, `RejectPayload` by both `proposal` and `quorum`. The rule already exists encoded — `schemas/conformance/schema.json` requires a `payload_type` matching `^(macp\.v1\.[A-Za-z]+|macp\.modes\.[a-z_]+\.v\d+\.[A-Za-z]+Payload)$` — it is just never written normatively. Deliverable is the normative statement plus the envelope schema description, not a new registry.
- **S3 — conformance fixtures (#84 + #81), one PR.** Depends on S1 (the wording must exist before a fixture pins it). Adds duplicate-`Vote` and duplicate-ballot reject-path fixtures — including at least one cross-type quorum case, e.g. `Reject` after an accepted `Approve`, since rule 3 caps across all three types together — plus the first fixtures for `ObjectionPayload`, `WithdrawPayload`, and `TaskUpdatePayload`. **This is the PR that turns three downstream repos red.** It must not merge until the three vendoring PRs of Wave 2 are staged and ready to go out behind it.

### Wave 2 — vendor the fixtures (three repos, parallel, immediately after S3)

- **R1 — `macp-runtime`.** Copy the new fixtures into `tests/conformance/`, confirm the Rust harness replays them, and confirm `conformance-oracle` goes green. Verify the harness handles the new reject paths and the three previously-unfixtured payload types; `Objection`/`Withdraw`/`TaskUpdate` have never been exercised by a fixture in any implementation. Also add the one unit test Wave 0 surfaced as missing: `duplicate_ballot_rejected` (`quorum.rs:609`) covers *same-type* duplicates, but nothing pins **cross-type** first-wins (a `Reject` after an accepted `Approve`), even though the same `contains_key` guard covers it. Test addition only — no behaviour change.
- **T2 — `macp-sdk-typescript`.** `make sync-fixtures`, replay, green `verify-fixtures`. The `duplicate-ballots.ts` guard is pre-verified as needing **no change** (`:117` skips non-accept before bucketing), and per corrected finding (a) the reject-path block needs no change either.
- **P3 — `macp-sdk-python`.** `make sync-fixtures` across both `FIXTURE_DIR_PAIRS` entries, replay, green `verify-fixtures`. Closes PY #51, whose own text says "Nothing to build here first." Reject-path hygiene is already covered by #53.

All three payload types (`Objection`/`Withdraw`/`TaskUpdate`) are pre-verified as decodable in both SDKs today, so the vendoring itself is mechanical; the harness work is the real content of T2/P3.

Each is its own PR in its own repo. All three should merge the same day S3 does; a red `main` in three repos is the cost of getting this wrong.

### Wave 3 — unblocked SDK work (no dependency; start immediately)

- **T1 — TS #58**, `GrpcTransportAdapter` never passes `afterSequence`, so every reconnect replays from 0. RFC-MACP-0006 §3.2 conformance issue. Note the coupling the issue calls out: the `message_id` dedup guard in `BaseProjection.applyEnvelope` is currently the *only* thing keeping this from duplicating projection state on every reconnect — the fix is additive and must not weaken it.
- **P1 — PY #49**, **six broken examples, not one.** Verified by running all nine against a live runtime: four auth/sender mismatches (`quorum_approval.py`, `handoff_escalation.py`, `proposal_negotiation.py`, `task_delegation.py`), two **wrong-API** defects (`policy_registration.py:72` `got.descriptor` → `AttributeError`; `task_delegation.py:46` `is_completed()` missing its `task_id` arg → `TypeError`), and one mode-semantics defect (`proposal_negotiation.py:23`). The wrong-API pair is the strongest argument for the real deliverable: `tests/unit/test_examples_smoke.py` only *compiles* examples and admits in its own docstring that renamed APIs escape it. The fix is an execution gate plus a coverage-parity test so a new example cannot silently escape it.
- **P2 — PY #50**, the doc errors are on **five mode pages, not one**, and `quorum.md` has a *third* instance of the bad `total_eligible` at `:133`. Also settled: `QuorumThreshold` should gain real integrality validation rather than the doc being weakened to match lenient code — the canonical schema declares `"value": {"type": "integer"}` for all threshold types and `macp-sdk-typescript/src/policy.ts:190-199` already throws, so Python is the outlier emitting schema-invalid descriptors. Note `tests/unit/test_policy.py:186,192` *actively asserts* the current `0.75` pass-through and must be edited.
- ~~**P4 — cut `macp-sdk-python` 0.8.0.**~~ **Already shipped 2026-08-31** — see finding (c). Both SDKs are at 0.8.0 and agree; the divergence this phase existed to close is gone, and `macp-playground` can now pin `^0.8.0` on both sides coherently. Replaced by **P5 — reclaim the CHANGELOG**: release-please skipped the hand-written `## Unreleased` block (`CHANGELOG.md:3`) and inserted `## [0.8.0]` below it at `:162`, so shipped work reads as unreleased. Recurs every release until fixed.

### Wave 4 — triage only, no code

- **T3 — TS #59 + #60 triage.** Ten sites, each mapped to its governing RFC section with a verdict of rule-exists / rule-absent / ambiguous. Output is a table and, where a rule is absent, a new spec issue. Explicitly not an SDK change.

## Long-term posture

- **The corpus gap is the real finding.** #81 notes the asymmetry plainly: `Vote` cardinality has fixtures, three implementations, and mechanical guards, while `Objection` severity — which can veto a decision outright — has one implementation's unit tests. The corpus covers the path that was already hard to get wrong and skips the one where a silent difference changes outcomes. Wave 1/S3 closes this specific instance; it does not close the general absence of a coverage policy over the corpus. Worth a follow-up: an explicit statement in `schemas/conformance/README.md` of what the corpus is *meant* to cover, so the next gap is a violated policy rather than an unnoticed hole. The README already flags deliberate omissions where they exist (the `cmt-hash/` "it is not an oversight" note), which is why these three read as a gap and not a scoped decision.
- **Three vendored copies of one corpus is the coupling being paid for here.** It is the right design — hermetic local runs, no network in the test path — but it means the canonical corpus can never be edited without a coordinated three-repo landing. Wave 2 exists solely because of that. Nobody should add a fourth vendoring implementation without a sync mechanism better than "three Makefiles and a CI job."
- **One-way doors in this program:** Wave 0's scoping decision (protocol contract, possibly a persisted-format change to `QuorumState`), and — if the Python SDK plan recommends it — adding runtime validation to `QuorumThreshold`, which changes behaviour on a public dataclass.

## Open questions

1. ~~Wave 0's scoping decision.~~ **Resolved** — see Wave 0. The runtime needs no code change; `QuorumState` is untouched.
2. ~~Whether the #125 answer belongs in the RFC or in an issue comment.~~ **Resolved** — RFC text, plus a comment on #125 linking the PR.
3. **Ratification of the rule 1 hardening — the one open question.** Declaring the one-`ApprovalRequest` cap unrelaxable within v1 forecloses evolving v1 in place toward multi-request quorum; the escape hatch becomes a new mode identifier. Recommended without hedging, but it is the protocol owner's call and there is no wording that preserves both options. **S1 does not start until this is answered.**
4. **Merge policy for the fan-out.** S3 and the three Wave 2 PRs should land as close together as possible. Confirmed with the user before S3 merges: whether to merge S3 and accept a short red window in three repos, or hold S3 until all three vendoring PRs are open and approved.

## Status

| Wave | Phase | Repo | Issues | Status |
|---|---|---|---|---|
| 0 | Scoping decision + rule 1 hardening | — | #83, #125 | **DONE** — ratified 2026-08-31 |
| 1 | S1 which-ballot-stands + rule 1 invariant | spec | #83, #125 | **MERGED** `cd5ac2b` — #83, #125 closed |
| 1 | S2 message_type mode-scoped | spec | #82 | **MERGED** `a1b29a1` — #82 closed |
| 1 | S3 conformance fixtures | spec | #84, #81 | **MERGED** PR #89 — #81, #84 closed |
| 2 | R1 vendor fixtures + `TaskUpdate` harness branch | runtime | — | **MERGED** PR #135 |
| 2 | T2 vendor fixtures | ts-sdk | — | **MERGED** PR #69 — harness needed no change |
| 2 | P3 vendor fixtures | py-sdk | #51 | **MERGED** PR #54 — #51 closed; harness needed no change |
| 3 | T1 afterSequence | ts-sdk | #58 | **MERGED** — PR #65, #58 closed |
| 3 | P1 execution gate + fix 6 broken examples | py-sdk | #49 | **MERGED** — PR #52, #49 closed |
| 3 | P2 doc sweep (5 pages) + QuorumThreshold validation | py-sdk | #50 | **MERGED** — PR #52, #50 closed |
| 3 | ~~P4 release 0.8.0~~ | py-sdk | — | **OBSOLETE** — shipped 2026-08-31 |
| 3 | P5 reclaim the CHANGELOG | py-sdk | — | **MERGED** — PR #52 |
| 4 | T3 fix the sites that have RFC rules | ts-sdk | #59, #60 | **MERGED** PR #68 — 4 of 5 fixed; #59/#60 stay open |
| 4 | T4 file follow-ups for the deferred sites | ts-sdk→spec | #59 | **DONE** — spec #90, ts #70, #71 |


## Wave 1/3 outcome — 2026-09-01

Merged: spec #85 (`cd5ac2b`), `macp-sdk-typescript` #65, `macp-sdk-python` #52, and this repo's #131.
Closed: spec #83, runtime #125, TS #58, PY #49, PY #50.
Open and green, one verify round from merge: spec #86.

**Five issues were filed from findings the planning and verification passes turned up.** Each is the
same shape the program exists to close — behaviour that is implemented and enforced with nothing
normative or nothing mechanical behind it:

| Issue | Repo | Finding |
|---|---|---|
| #87 | spec | RFC-0001 §6 contemplates only Signals as the empty-`mode` case, but the runtime also accepts ambient `Progress` (`src/server.rs:118-140`). The code comment cites an RFC for the Signal rule and cites nothing for the `Progress` rule directly beneath it. |
| #88 | spec | `lint_fixtures.py:101`'s initiator carve-out names `CancelSession`, which is never a `message_type` — the entry is dead and `SessionCancel` is uncovered. |
| #66 | ts-sdk | `Participant.run()` breaks on `!this.running` *after* the adapter counted the envelope. Under replay-from-0 that message returned next run; with the resume cursor it is permanently skipped. #65 is what made a latent bug consequential. |

**What verification caught that execution did not.** Worth recording, because in three of four phases
the verify gate changed the outcome rather than rubber-stamping it:

- **Spec #86, round 1:** the new normative text listed `CancelSession` among mode-independent
  *message types*. That is the RPC name; the envelope type is `SessionCancel` (RFC-0001 `:253`,
  `runtime.rs:817`). A normative sentence about interpreting `message_type` named a value that can
  never legally appear in `message_type`.
- **Spec #86, round 2:** the fix completed the Core-type list as exhaustive but omitted `Progress` —
  which the schema's own body binds to `ProgressPayload` unconditionally, the runtime dispatches in
  the same match arm as `Signal`, and both SDKs plus the control plane carry in their core maps.
  **As written, the new MUST NOT would have made four existing implementations non-conforming** — a
  clarification phase silently becoming a breaking requirement. Caught pre-merge.
- **TS #65:** the verifier mutation-tested the new tests and proved the off-by-one is caught in both
  directions against a live runtime (`+1` → first pass times out; `-1` → wrong count), rather than
  taking the green suite as evidence.
- **PY #52:** the executor caught a tautology in its *own* draft — the first coverage-parity test
  computed `RUN = {all examples} − EXCLUDED`, which can never fail. Independently re-proven at merge
  time: adding an unclassified example fails the gate, reverting one of the six fixes turns its case
  red. That mattered more than any other check here, because the PR's entire premise is that a
  compile-only test gave false assurance.

**Still open:** spec S3 (the #81 + #84 fixture fan-out) and Wave 2's three vendoring PRs. The harness
work finding (a) originally called a prerequisite **does not exist** — see the correction above; the
vendoring is mechanical. Wave 4's five TS projection sites with governing RFC rules are in progress.

## Wave 2/4 outcome — 2026-09-01

The fan-out landed as a coordinated set and behaved exactly as Context §1 predicted. Spec PR #89 was held unmerged until all three vendoring PRs were open; on merge, each vendoring PR's fixture guard flipped from red to green on a bare re-run with no further edits, and all three merged within minutes. No repo was left red at any point.

Two predictions in this file were wrong, and both are worth recording because they were wrong in the same direction — assuming a gap where the code already had one covered.

**The SDK harnesses needed no work.** Wave 2 was written as "vendor **+ teach harness to replay rejects**" for both SDKs. Neither needed teaching. `macp-sdk-typescript` already mapped `Objection`, `Withdraw`, and `TaskUpdate` in `src/proto-registry.ts`'s `MODE_MAP`; `macp-sdk-python` derives `PAYLOAD_BUILDERS` from `proto_registry.CORE_MAP`/`MODE_MAP` and already handled all three. Both discover fixtures dynamically, so no per-file wiring existed to add. This is the same error corrected in #134 — see that PR — reappearing one wave later.

**The one real harness gap was in `macp-runtime`, and it was in the test harness only.** `tests/conformance_loader.rs::encode_task_payload()` had no `TaskUpdate` branch, so the new task fixture could not be encoded at all — it panicked with `Unhandled task message: TaskUpdate`. Production `TaskMode` has always handled `TaskUpdate` correctly (`crates/macp-modes/src/mode/task.rs:273-277`, sender-vs-`active_assignee` → `Forbidden`). Fixed in #135 and proven load-bearing by removal.

### A measurement trap worth remembering

All three harnesses run **one test case per fixture *file***, not per message. Every vendoring PR therefore showed an unchanged test count (TS 65, Python 57) while genuinely gaining coverage — `proposal_reject_paths` alone went from 2 messages to 6. A pass/count comparison would have "confirmed" coverage that had not been checked. Each agent instead replayed the new payloads through the real projections directly. Any future corpus change should be verified the same way; the count is not evidence.

### Deferred, with issues filed

- **spec #90** — no fixture anywhere exercises `objection_handling.critical_severity_vetoes` or `critical_objection_action`. Note this is an *unpinned agreement*, not a divergence: `macp-runtime` (`crates/macp-core/src/policy/rules.rs:85-91`) and `macp-sdk-python` (`src/macp_sdk/policy.py:65-73`) declare the same fields with the same defaults today. Nothing holds them there. An earlier review reported this as "opposite defaults"; that was wrong, and the issue says so.
- **ts #70** — `TaskProjection` now reports the wrong `assignee` after a policy-permitted reassignment. #68 traded one divergence for another: the guard is policy-independent, but RFC-MACP-0009 rule 3c allows reassignment after `TaskReject` when policy permits, and the runtime implements it (`task.rs:257-260` clears `active_assignee`). The default case is far more common, so the trade was the right way round, but the projection needs session-policy input to close the other half.
- **ts #71** — that same guard is per-`task_id`; rule 1 scopes it per-session.

`#59` and `#60` remain open deliberately: #68 fixed the four sites with governing normative rules, and the remainder (`TaskComplete`/`TaskFail` accumulation, `Proposal` `Reject` cardinality) are genuinely ambiguous in the RFCs rather than merely unimplemented.
