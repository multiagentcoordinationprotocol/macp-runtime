# Phase A — Pre-Freeze Work (wire/API-breaking window)

**Source:** master plan §2.1–§2.5, §1.8–§1.10, §4.5, §4.4 · **Effort:** ~2 weeks ·
**Hard constraint:** everything here is breaking or wire-adjacent; after the freeze
each item becomes a major-version event. Nothing else in this phase's window should
widen the public surface.

## Ordered tasks

### A1. Make public types evolvable (master §2.1)
1. Add a builder to `Session` (`crates/macp-core/src/session.rs:37`) —
   `Session::builder(id, mode, initiator)` with setters; mark `#[non_exhaustive]`.
2. Migrate the ~10 full-literal constructors (mode tests, `step.rs`, `util.rs`,
   `session.rs` tests) to the builder — this simultaneously kills the test-fixture
   duplication noted in master §3.5.
3. Mark `#[non_exhaustive]`: `MacpError` (`error.rs:4`), `ModeResponse`
   (`mode.rs:10`), `PolicyDecision`/`PolicyError` (`policy/mod.rs:25,31`), mode
   phase enums.
4. `cargo build` all workspace crates + integration tests; fix downstream matches
   (each needs a `_` arm now — audit that the `_` arms fail safe, not silently).

**Exit:** adding a field to `Session` compiles without touching mode crates.

### A2. Unify `PolicyEvaluator` around `CommitmentContext` (master §2.2)
1. Define `CommitmentContext` in `macp-core` (mode id, accumulated mode state view,
   declared participants, `outcome_positive`, policy rules).
2. Single trait method `evaluate_commitment(&self, ctx: &CommitmentContext)
   -> Result<PolicyDecision, PolicyError>`; port `DefaultPolicyEvaluator`'s five
   mode paths onto it (the Decision outcome-aware path becomes the template — the
   other four modes gain outcome-awareness, fixing the outcome-blind decline
   denial in Quorum, master §2.2/§1.10 interaction).
3. Keep the old five methods as `#[deprecated]` shims delegating into the new one
   for one release.
4. While here: fix the Quorum evaluator non-conformance (master §1.10) — drop the
   participation reinterpretation of `threshold`; align with the mode's
   required-approvals reading per RFC-0012 §4.2. Percentage-unit fix per
   `rfc-changes.md` item 4 outcome (if upstream is slow: keep 0–100 semantics,
   document divergence from the schema's stated type).

**Tests:** conformance vector for quorum threshold semantics (happy + decline);
negative-outcome decline vectors for proposal/task/handoff/quorum (they now flow
through outcome-aware evaluation); all 96 policy unit tests green on the new trait.

### A3. `policy.default` echo contract (master §2.3)
Default direction (pending `rfc-changes.md` item 3): accept empty
`commitment.policy_version` as matching the session's resolved policy
(`util.rs:30`). Permissive is forward-compatible with either upstream outcome.
**Test:** tier-1 — SessionStart with empty `policy_version`, Commitment with empty
`policy_version` → accepted; with wrong non-empty value → rejected.

### A4. Session-ID validation fix (master §2.4)
`crates/macp-core/src/session.rs:236` — attempt UUID parse first, fall through to
the base64url branch on failure instead of hard-routing 36-char values.
**Test:** 36-char base64url token containing `-` is accepted; malformed UUIDs of
other lengths still rejected.

### A5. Ext-mode validation holes (master §1.8, §1.9)
1. `promote_mode`: apply the reserved-namespace guard to the promotion target
   (`mode_registry.rs:568`); re-validate the descriptor for standards-track
   expectations on promotion.
2. `validate_extension_descriptor` (`mode_registry.rs:468`): require ≥1 terminal
   message type present in `message_types`.
3. Version binding (§1.9 — respect the migration rule): at SessionStart, when the
   payload's `mode_version` is empty, bind the registered descriptor's version and
   **persist the bound value in the session/log**; replay uses the recorded value,
   never the live registry. Legacy histories with empty bound version keep the
   vacuous check.

**Tests:** promote-to-`macp.mode.*` rejected; descriptor without terminal type
rejected; ext commitment with wrong version rejected on new sessions; pre-fix log
fixture (empty-version history) still replays.

### A6. Handoff implicit-accept interim fix (master §2.5)
Depends on A2's trait work (Mode signature change rides the same breaking window).
Pass acceptance time into `on_message` (or via a context struct alongside A2);
handoff auto-accept uses it instead of `env.timestamp_unix_ms`. Migration rule
applies: new sessions only; legacy logs keep old semantics; pre-fix fixture test.
The RFC-0012 §4.5 timer is **not** built here (waits on `rfc-changes.md` item 2).

### A7. Multi-round protobuf payloads (master §4.5)
Waits on `rfc-changes.md` item 6 (`macp-proto` release). Then: generate in
`macp-pb`, proto-first-JSON-fallback parse in `multi_round.rs`, update
`conformance_loader.rs` + `src/bin/multi_round_client.rs`. JSON fallback retained
for replay compatibility until a log migration exists.

### A8. Roots capability decision (master §4.4 — decision only)
Decide implement-vs-disclaim now (it changes `Initialize` capability
advertisement); implementation, if chosen, lands in Phase E.

## Exit criteria for the phase
- All A-tests green; conformance suite extended with the new vectors.
- No remaining public type in `macp-core`/`macp-modes` that cannot absorb a new
  field/variant without a breaking change.
- CHANGELOG entries drafted for every observable behavior change (A3–A7).
