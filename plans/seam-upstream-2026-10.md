# PLAN: everything a library embedder (Seam) needs from macp-runtime, phased for one bump

**Verified against:** `macp-runtime` origin/main `fb10fb2` (workspace 0.8.8). I read it in a detached scratch worktree. Every `file:line` below was read, not recalled. The local checkout was not modified.

**Spec baseline:** this plan follows the proposal. The proposal is `rfcs/proposals/seam-embedder-findings-2026-10.md`:

- branch: `proposal/seam-embedder-findings-2026-10`;
- spec draft PR #186, tracking issue #185;
- revision: the 2026-10-16 revision, which adds S12–S14 and I12–I17.

The **what** and the **why** of each item live there, as S-numbers (spec) and I-numbers (implementation). This plan restates neither. It gives the **order**, the **files**, the **tests**, the **acceptance** and the **release** for each.

**Related filings:**

- macp-runtime #227: the implementation tracking issue;
- macp-runtime #167: closed by-design, and reopened in substance by S2/I6;
- a private security advisory, in draft, covering the items in Phase 0.

**Owner decisions (2026-10-16), applied throughout:**

1. S4 and S7: the RFC is normative and the implementation conforms to it. S7's casing sentence is wrong in the RFC and is fixed in the text.
2. S13 is accepted, including the wire-status break.
3. Phase 0a ships in 0.8.9 now, and Seam still bumps once, to 0.9.0.
4. Phase 0b is accepted. Seam renames `rules.seam.capture_dissent` to the `x-<reverse-domain>` form. The registry validates `x-` keys, and the embedder enforces them.

**Write scope:** all work lands in `macp-runtime`, except the Phase 0b spec dependency on S1. Nothing here touches `seam-runtime`.

**Goal:** Seam bumps once, to the release this plan produces, and then carries no workaround for any of the items below. Today Seam carries these workarounds:

- `novotes_guard::SeamPolicyEvaluator` (removable Seam-side; see Sequencing);
- its own R1 policy/mode guard;
- its own `voting.threshold` and weight range checks;
- objector redaction by string-splitting on `", from: "`;
- an envelope clock of 0;
- a hand-rolled expiry (`session.state = Expired`);
- no session-id, version or canonical-payload check on `SessionStart`.

---

## Context

### W0-4 reproduction (I4)

This is a scratch integration test against `fb10fb2`, run in `crates/macp-modes/tests/` and then deleted. The setup is a handoff session at the builder-default `semantics_rev` 3 (`crates/macp-core/src/session.rs:241`), with `implicit_accept_timeout_ms: 1000` and `step` throughout.

| Offer at | Commitment at | `due_synthetic_envelope` | `step(Commitment)` |
|---|---|---|---|
| 1 000 000 | 1 010 000 | `Some` | `Err(InvalidPayload)` |
| 0 (envelope clock 0) | 0 | `None` | `Err(InvalidPayload)` |
| 0 | 10 000 | `None` | `Err(InvalidPayload)` |

The causes are:

- `step` (`crates/macp-modes/src/step.rs:127-145`) never calls `due_synthetic_envelope`.
- `validate_message` (`:79-87`) calls `on_message`, which uses the envelope clock (`handoff.rs:338-344`), and not `on_message_at` (`:346-364`).
- `due_synthetic_envelope` (`handoff.rs:403-460`) needs `offered_at_ms > 0`.
- The interim lazy path (`handoff.rs:607-652`) runs only at rev < 2.

The server performs the synthesis in `src/runtime.rs:748` (`synthesize_due_accept`). The library does not.

### Already satisfied (I17): document these, do not build them

- `StepOutcome::Duplicate` (`step.rs:111-116`).
- `pub fn validate_session_id_for_acceptance` (`crates/macp-core/src/session.rs:686`).
- `pub fn commitment_hash` (`crates/macp-core/src/commitment_hash.rs:44`).
- `MacpError::error_code()` (`crates/macp-core/src/error.rs:55`).
- Wildcard policies validated against every standards-track mode (`crates/macp-policy/src/registry.rs:384-392`).
- `voting.threshold` in (0, 1] (`:585`).
- Weights `<= 0` or NaN refused (`:596`).
- `effective_max_suspend_ms` (`session.rs:270-276`).

Seam can delete its own `"*"`-binds-decision-only guard and its `voting.threshold` / weight checks once it is on any release of the crate. That is a Seam change, not an upstream one.

### Release mechanics

- `release-plz` runs with `semver_check = true` and one version group across the 7 crates.
- `StepOutcome` is **not** `#[non_exhaustive]` (`step.rs:110`), so any variant or field added to it is a major change. In 0.x, a major change becomes a minor bump.
- `MacpError` **is** `#[non_exhaustive]` (`error.rs:5`).
- Two releases are planned:
  - **0.8.9** (patch): additive and tightening-only work that needs no spec change beyond what the schemas already say;
  - **0.9.0** (breaking): everything that changes a public signature or waits on a spec item. Seam bumps to 0.9.0 once.

---

## Phase 0a: admission value-domain checks (private advisory scope; I16)

- **Proposal ref:** S11 range vectors. Scope and wording follow the private advisory, and the public description stays neutral.
- **Files:**
  - `crates/macp-policy/src/registry.rs`: add a helper next to `validate_decision_voting` (`:575`). It enforces the bounds that `decision-rules.schema.json` already states, for every field the schema bounds (`objection_handling.veto_threshold`, `evaluation.minimum_confidence`, and the enumerated string fields). It is called from `validate_conditional_constraints` (`:435`) for both `decision` and `"*"`.
  - `crates/macp-policy/src/evaluator.rs`: add defence in depth at `:230`. An out-of-domain value that reaches evaluation (for example a stored legacy descriptor) denies, and is never treated as allowing.
- **Tests:**
  - registry unit tests, one per bound, at the boundary value and one past it;
  - an evaluator test showing that a descriptor built without `register` still fails closed;
  - `tests/` vendors `invalid-policy-rules/veto-threshold-zero.json` and `minimum-confidence-above-one.json` from spec S11 once they land.
- **Acceptance:** every `invalid-policy-rules/*` fixture in the spec is refused by `PolicyRegistry::register` with an `INVALID_POLICY_DEFINITION` prefix. Every `valid-policy-rules/*` fixture is admitted.
- **Semver:** patch. There is no API change. Admission tightens only for values the schema already forbids.
- **Release:** 0.8.9, cut **now** (owner decision 3) and coordinated with the advisory's publication. It does not wait for any spec PR, because the bounds are already in the published schemas. Seam does not bump to 0.8.9; it picks the checks up with 0.9.0.

## Phase 0b: closed rule objects plus the vendor-extension slot (advisory scope; S1)

- **Decision:** accepted (owner decision 4). The registry validates `x-` keys: their name, their object type, and whether a `RuleExtension` is registered for them. The embedder's evaluator enforces their meaning.
- **Blocked on:** spec S1 merging.
- **Seam follow-up:** in the 0.9.0 bump, Seam renames `rules.seam.capture_dissent` to `x-<reverse-domain>.capture_dissent`, using the reverse-domain name S1 fixes. It also registers its `RuleExtension` and migrates stored policies. Under the owner's no-backward-compat rule this is a recreate, not a dual-read.
- **Files:**
  - `crates/macp-core/src/policy/rules.rs`: add closed-key enforcement for every rules struct. Use either `#[serde(deny_unknown_fields)]` with the `^[_$]` annotation keys and top-level `x-` keys stripped first, or an explicit key-set walk. The walk is preferred, because it can report the offending path.
    - `"*"` validates against the union of the standards-track key sets.
    - `voting.weights` keys are participant ids and are exempt.
  - `crates/macp-policy/src/registry.rs` (`:384-414`): validate each `x-` key's name and object type, then reject any `x-` key the registry does not implement.
  - Add `PolicyRegistry::with_rule_extensions(impl IntoIterator<Item = (String, Arc<dyn RuleExtension>)>)`, where `RuleExtension` is a `#[non_exhaustive]`-friendly trait in `macp-core::policy` with `validate(&Value) -> Result<(), String>`. Enforcement stays in the embedder's evaluator wrapper, which is restrict-only by contract.
  - `src/server.rs`: advertise `rule_extensions` (field 4) in `PolicyRegistryCapability`.
  - Update `macp-pb` after the proto change.
- **Tests:**
  - closed-key rejection at a nested level and at the top level;
  - `_`/`$` keys admitted;
  - an unimplemented `x-` key refused;
  - an implemented `x-` key admitted and passed to its validator;
  - §8 equality: an `x-` key difference is not equal, and an `_` key difference is equal;
  - every S1 fixture.
- **Acceptance:** all S1 vectors pass. `policy.std.*` canonical resolution (`registry.rs:353`) is unchanged.
- **Semver:** minor. There is a new public trait and builder, and admission becomes stricter for keys that 1.4.0 already forbids.
- **Release:** 0.9.0.

## Phase 1: `schema_version` admission (S2, I6)

- **Files:**
  - `crates/macp-policy/src/registry.rs:301`: replace the `== 0` test with membership in a public `macp_policy::SUPPORTED_SCHEMA_VERSIONS`. Move the constant from `evaluator.rs:24` and re-export it.
  - The evaluator keeps `check_schema_version` (`evaluator.rs:26`) as defence in depth.
- **Tests:**
  - `register_unsupported_schema_version_fails` for 4 and 99;
  - the existing `register_zero_schema_version_fails`;
  - a `load_from_dir` case;
  - once it lands, vendor the spec transcript `policy_admission_schema_version.json`.
- **Acceptance:** `RegisterPolicy` with 99 returns `INVALID_POLICY_DEFINITION`.
- **Semver:** patch. There is one new public constant.
- **Release:** 0.8.9.

## Phase 2: RFC-0013 verify API (S14, I3)

- **Files:**
  - `crates/macp-core/src/commitment_hash.rs`: add `pub fn verify_commitment_hash(wire: &[u8], claimed: &str) -> VerifyOutcome` and `#[non_exhaustive] pub enum VerifyOutcome { Match, Mismatch, CannotVerify }`. Unknown-field detection has to scan the wire tags itself, because prost drops unknown fields:
    - `CommitmentPayload` tags 1–9 are known;
    - inside `supersedes` (tag 9), tags 1–2 are known;
    - any other tag is `CannotVerify`;
    - a decode failure is `CannotVerify`;
    - a non-canonical `claimed` (reuse `is_canonical_commitment_hash`, which is now promoted from `macp-modes` `mode/util.rs:86` to `macp-core`) is `Mismatch`.
  - Also add `pub fn commitment_hash_of_wire(wire: &[u8]) -> Result<String, CannotVerify>` for embedders that seal a hash computed from bytes received over the wire.
- **Tests:**
  - every `cmt-hash/*.json` vector, including the new `006`–`009`;
  - a property test: `verify(encode(p), commitment_hash(p)) == Match` for arbitrary `p`.
- **Acceptance:** the S14 vectors pass, and `commitment_hash` output is byte-identical to 0.8.8.
- **Semver:** patch. The change is additive.
- **Release:** 0.8.9.

## Phase 3: library acceptance path (S12; I4, I5, I13, I14)

This is the core of the Seam bump.

- **Files:**
  - `crates/macp-modes/src/step.rs`:
    - `check_preconditions` (`:43-61`) gains a `macp_version` check against `macp_core::MACP_VERSION`, checks for non-empty `message_id` and `sender`, and applies a TTL test that includes the suspend and cycle caps. That cap test is lifted from `src/runtime.rs:1248-1256` into a `Session` method.
    - `validate_message` (`:79-87`) takes `now_ms` and dispatches through `on_message_at` with `MessageContext { accepted_at_ms: now_ms, .. }`.
    - Before validation, `step` loops over `mode.due_synthetic_envelope(session, now_ms)`. It dispatches, commits and dedups each synthetic envelope following the four-step contract in `mode/mod.rs:110-160`, and collects them.
    - `StepOutcome::Accepted` becomes `Accepted { state, synthesized: Vec<Envelope> }`, so a durable embedder can append the synthetic envelopes in order. The enum is also made `#[non_exhaustive]`.
  - `crates/macp-modes/src/step.rs`, new function: `pub fn accept_session_start(env: &Envelope, now_ms: i64, mode: &dyn Mode, policy: Option<PolicyDefinition>, opts: StartOptions) -> Result<Session, MacpError>`. It lifts `src/runtime.rs:384-500` and performs, in order:
    1. the version check;
    2. `validate_session_id_for_acceptance`, with `StartOptions::session_id_validator` as an override hook for deployments with a stricter format;
    3. `validate_canonical_session_start_payload_for_mode`;
    4. TTL and `max_suspend_ms` resolution;
    5. RFC-0012 §6.1 step 5, the policy/mode binding;
    6. an explicit `semantics_rev`;
    7. `mode.on_session_start`.

    The server's `handle_session_start` then calls this function, so the server and the library cannot drift.
  - `crates/macp-core/src/session.rs`: add `pub fn expire(&mut self, now_ms: i64) -> Result<(), MacpError>`, which is OPEN or SUSPENDED to EXPIRED and records the time. Add `pub fn deadline_passed(&self, now_ms) -> bool`, which includes the caps. In the builder docs (`:207`, `:241`), state that `semantics_rev` defaults to `CURRENT_SEMANTICS_REV` and `ttl_expiry` to unbounded, and point to `accept_session_start` as the supported constructor.
  - `src/runtime.rs`: replace `synthesize_due_accept` (`:748`) and the session-start block with calls to the library functions. Behaviour stays unchanged.
- **Tests:**
  - port the W0-4 scratch test as `crates/macp-modes/tests/step_implicit_accept.rs`. At an accepted-at clock with envelope timestamp 0, the Commitment is `Accepted`, `synthesized.len() == 1`, the synthetic `message_id` is `implicit-accept:h1`, and a suspended interval is excluded;
  - `accept_session_start` rejection cases: a weak id, an uppercase UUID, version `"2.0"`, a mode-mismatched policy, and a non-canonical payload;
  - a cap-exceeded session expires through `step`;
  - extend `coordination_library.rs` to use `accept_session_start`;
  - the existing server suites pass unchanged;
  - vendor S12's `handoff_implicit_accept.json` and `session_start_reject_paths.json` into the conformance harness, together with its `accepted_at_ms` support.
- **Acceptance:** an embedder that uses only `accept_session_start` and `step`, passing a real clock, passes every spec conformance transcript, including the new S12 ones, with no server code.
- **Semver:** **breaking**:
  - the `StepOutcome` shape changes;
  - `validate_message` gains a parameter;
  - `step` rejects envelopes it used to accept (wrong version, empty sender).
- **Release:** 0.9.0.

## Phase 4: error status and structured denial (S13, I12)

- **Files:**
  - `crates/macp-core/src/error.rs`:
    - add `pub fn http_status(&self) -> u16` and `pub fn transport_status(&self) -> TransportStatus`. `TransportStatus` is a `#[non_exhaustive]` enum mirroring the gRPC codes in S13. Defining it in `macp-core` keeps a tonic dependency out of the crate.
    - `PolicyDenied { reasons: Vec<String> }` becomes `PolicyDenied { reasons: Vec<DenyReason> }`, with `#[non_exhaustive] pub struct DenyReason { pub code: DenyCode, pub message: String, pub participants: Vec<String> }`.
  - `crates/macp-policy/src/evaluator.rs`: every deny site, the veto at `:230` among them, fills `participants` and leaves identifiers out of `message`. There are 12 `PolicyDenied {` construction sites across `crates/` and `src/`.
  - `src/server.rs:759-788`: map through `transport_status()`. This changes `POLICY_DENIED` to `PERMISSION_DENIED`, and the not-found, not-open and invalid cases to the registry status. The `macp-error-details-bin` JSON carries the structured reasons.
- **Tests:**
  - a table test asserting that `transport_status` and `http_status` equal the parity-contract `error_status` table for every variant;
  - an evaluator test that no deny `message` contains a participant id;
  - server status tests updated (`server.rs:1908`, `:1930`, `:2026`, `policy_engine.rs:146-206`).
- **Acceptance:** the parity-contract `error_status` check passes, and Seam's `redact_objectors` becomes unnecessary.
- **Decision:** accepted, including the wire break (owner decision 2).
- **Semver:** **breaking**, because the `PolicyDenied` field type changes and the wire status changes for gRPC clients.
- **SDK follow-up, required before the 0.9.0 release notes go out:**
  - Retarget any branching on `FAILED_PRECONDITION` for `POLICY_DENIED`, `SESSION_NOT_FOUND`, `SESSION_NOT_OPEN` and `INVALID_ENVELOPE` to the registry statuses. That means `PERMISSION_DENIED`, `NOT_FOUND`, `FAILED_PRECONDITION` and `INVALID_ARGUMENT` respectively.
  - Parse the structured `macp-error-details-bin` reasons.
  - In macp-sdk-python, the change belongs in its error mapping and `MacpAckError` surface.
  - In macp-sdk-typescript, it belongs in the equivalent error classes.
  - File one issue per SDK when Phase 4 merges. Release each SDK in lockstep with 0.9.0.
- **Release:** 0.9.0.

## Phase 5: mode-rule alignment (S4–S7; I7–I10)

- **Decision:** the RFC is normative and the implementation conforms to it (owner decision 1).
  - S4 needs no spec change, only vectors.
  - S7 needs a text fix to RFC-0007 L80, and an explicit sentence after L76 that an unlisted severity is rejected.
  - S5 and S6 remain spec clarifications, because the RFC is ambiguous there.
- **Blocked on:** the S5, S6 and S7 spec PRs. The S4 code can land as soon as its vectors do.
- **Files:**
  - `crates/macp-modes/src/mode/task.rs:228-243` (S6, rule 2a);
  - `handoff.rs:483-503` (S5, the declined-target set in state);
  - `quorum.rs:384-411` (S4): remove the `ApprovalRequest` refusal for an effective threshold outside `1..=participants`. Rule 2's bound on the payload's `required_approvals` stays.
  - `quorum.rs:327-328` (S4): drop the `counted > 0` gate, so an unreachable threshold is eligible for a negative `Commitment` at once under rule 4. Update the #145 rationale comment to cite rule 6.
  - `decision.rs:179,190,207-208,226` (S7): make all comparisons case-sensitive, so recommendation and vote values are UPPER_CASE and severity is lowercase.
  - `decision.rs:203-205` (S7): remove the `medium` default, so an empty severity returns `InvalidPayload`.
- **Tests:** vendor the reject-path transcript additions from S4–S7, plus unit tests per rule. Replay of pre-change histories must stay identical, because replay does not revalidate client-boundary rules.
- **Semver:**
  - S5, S6 and S7 tighten behaviour, which is breaking for clients that send non-canonical casing or an empty severity.
  - S4 loosens behaviour: requests refused today are accepted, and a negative `Commitment` with no ballot becomes possible under an unreachable override.
  - Call out S4 in the release notes as a deliberate reversal of #145 that brings the implementation into line with RFC-0011 rule 6.
- **Release:** 0.9.0.

## Phase 6: suspend-cap resolver and library replay (I11, I15)

- **Files:**
  - `crates/macp-core/src/session.rs`: add `pub fn resolve_max_suspend_ms(payload_value: i64) -> i64`, which lifts `src/runtime.rs:487-491`. `accept_session_start` (Phase 3) uses it.
  - `crates/macp-modes/src/replay.rs` (new): `pub fn replay(entries: impl IntoIterator<Item = (Envelope, i64 /*accepted_at_ms*/)>, modes: &ModeRegistry, policy: Option<PolicyDefinition>) -> Result<Session, MacpError>`. It re-drives history through `on_message_at` at each recorded acceptance time, without client-boundary revalidation, as `src/replay.rs:133` does. The server's replay then delegates to it.
- **Tests:**
  - round-trip: `step` a session with a real clock, then `replay` it, and the `Session` is equal, including an implicit-accept history;
  - the existing `src/replay.rs` tests pass.
- **Semver:** additive. The changes can ride 0.8.9 if they are ready, or 0.9.0 otherwise.
- **Release:** 0.8.9 or 0.9.0.

## Phase 7: embedder documentation (I5, I17)

- **Files:**
  - a new `docs/embedding.md`, linked from `docs/README.md`, `docs/API.md` and the `macp-modes` crate docs. It covers:
    - the S12 obligations and which library function discharges each one;
    - the I17 already-public functions;
    - the durable-consumer split (`check_preconditions`, `validate_message`, `commit`);
    - the clock rule (always pass a recorded acceptance time, never 0);
    - the `semantics_rev` default.
- **Acceptance:** each S12 obligation maps to a function or an explicit "caller's job" line.
- **Release:** with 0.9.0.

---

## Sequencing

1. **Spec first.**
   - Spec PRs S2, S12, S13 and S14 unblock Phases 1–4. They are clarifications or additive, so they can merge independently of the discussion around S1 and S3–S7.
   - S1 unblocks Phase 0b.
   - S5–S7 unblock Phase 5. S4 needs only its vectors.
2. **0.8.9, now.** Ships Phase 0a with the advisory. Phases 1 and 2, and Phase 6 if ready, ship in 0.8.9 if they make the cut, or else in a later 0.8.x. Seam does **not** bump to 0.8.x. Its single bump is 0.9.0 (owner decision 3).
3. **0.9.0.** Ships Phases 0b, 3, 4, 5 and 7, and any remainder of Phase 6. Seam bumps once, to 0.9.0, and then deletes:
   - `novotes_guard`. It fail-closes empty tallies only for `schema_version <= 2`, whose fail-open reading RFC-0012 §4.1 preserves on purpose, and upstream already fails closed at 3 (`evaluator.rs:456-462`). Seam can drop it by admitting only `schema_version` 3, which is a Seam decision and needs no upstream change;
   - the R1 guard;
   - its local range checks;
   - `redact_objectors`;
   - `expire()`'s direct state write.

   It also switches `SessionStart` to `accept_session_start`, passes a real clock to `step`, and maps `MacpError::transport_status()` to its gRPC errors.

## Acceptance for the plan as a whole

- `seam-runtime` builds against 0.9.0 with no macp workaround left, as listed in the goal.
- Every spec conformance transcript, the S12 ones included, passes through the library-only path in `crates/macp-modes/tests/`.
- `cargo semver-checks` reports exactly the breaks declared in Phases 3 and 4, and in Phase 0b's stricter admission, and nothing else.
