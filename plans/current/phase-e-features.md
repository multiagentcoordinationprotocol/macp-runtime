# Phase E — Features & Debt Paydown (+ Phase F tooling)

**Source:** master plan §4.2, §4.4, §4.6, §5.7 (steps 3–4), §3.5, §4.7 · **Effort:**
2–3 weeks · **Depends on:** Phase A (§2.2 trait for E3; A8 decision for E2),
Phase C (C5 for E4).

## Tasks

### E1. `MACP_POLICIES_DIR` file-loaded policies (master §4.2)
Load policy JSON at startup; validate against mode rule schemas; pre-register with
`register_policy=false, list_policies=true` profile (RFC-0012 §9). Companion: ship
`policy.majority`/`policy.supermajority`/`policy.unanimous` as optional built-ins
once upstream reserves the names (`rfc-changes.md` follow-up).
**Tests:** tier-1 — session binds a file-loaded policy; malformed policy file
fails startup with a clear error; `RegisterPolicy` RPC rejected when profile says
read-only.

### E2. Roots (master §4.4 — per the A8 decision)
Either the minimal static-config provider (config file → `ListRoots` +
`WatchRoots` change events) with a tier-1 test, or the disclaim path (capability
off) with a test asserting `Initialize` no longer advertises it. Also document
`WatchModeRegistry`'s ping-style events.

### E3. Pluggable `PolicyEngine` trait + audit verbosity (master §4.6)
On top of A2's `CommitmentContext`. Settle before coding: deny-on-error semantics;
the determinism boundary (async external engines may govern only non-replayed
decisions — session access — or must be snapshot-recorded into history);
composition with the sync `PolicyEvaluator`.
Audit logging: `audit` rules block (verbosity per mode/policy) consumed at kernel
tracing call sites.
**Tests:** test-double engine denying one sender proves all three hook points;
two sessions under different policies emit different audit detail.

### E4. Conformance pack publishing + CI oracle (master §5.7 steps 3–4)
Package `fixtures/` + schema + pass/fail rules; coordinate with the spec repo so
`schemas/conformance/` becomes the single canonical source (`rfc-changes.md` item
15); add the CI job running the runtime against the spec repo's fixtures.
Cross-runtime validation stays deferred (needs a second implementation).

### E5. Consistency cleanups (master §3.5 — ongoing debt paydown, PR-sized each)
- Extract the commitment epilogue into `util::finalize_commitment` (5 modes).
- Generic mode-state codec; dedupe `extract_commitment_rules`.
- Normalize SessionStart participant validation across modes (verify each
  against its RFC before aligning).
- Malformed policy rules at eval time → loud `PolicyError`, not
  `unwrap_or_default()`.
- Passthrough state semantics: accumulate or document replace-only.
- Delete dead code: `registry.rs` persistence path, `recovery::recover_session`,
  or wire them; extensions provider registry (wire the RFC-0001 §7.4.2 hooks or
  remove until a consumer exists).
- Remove dead `HandoffContext` authorize branch (`handoff.rs:84`).

### E6 (Phase F rider). Lightweight tooling (master §4.7)
- `macp-transcript-viz` bin: fixture JSON or session log → Mermaid
  `sequenceDiagram`. Acceptance: every fixture in `tests/conformance/` renders
  valid Mermaid.
- buf.build schema publishing: file upstream (protos live in the spec repo);
  reference the BSR module in docs once live.

## Exit criteria
- Operators can preload policies from disk; roots surface is honest either way.
- External policy engines have a documented, tested integration point.
- The conformance pack is consumable by a third party without this repo.
- Master plan §3.5 checklist fully closed.
