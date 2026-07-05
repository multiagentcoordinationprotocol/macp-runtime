# Current Work — Execution Plans

**Created:** 2026-07-04 · **Source:** `plans/IMPROVEMENT_PLAN.md` (the audited master
plan; section references like §1.2 point there) · **Baseline:** v0.4.0 (`732b689`)

The master plan was decomposed into one execution plan per phase, plus an RFC-changes
plan for upstream spec work. Phases A and C can start immediately and in parallel;
the RFC filings in `rfc-changes.md` should go out first since three of them block
Phase A decisions.

| Plan file | Scope | Depends on | Effort |
|---|---|---|---|
| `rfc-changes.md` | Upstream RFC/proto changes + issue filings | — (file first) | ~2 days to file; upstream latency unknown |
| `phase-a-prefreeze.md` | Wire/API-breaking work that must precede the freeze | RFC items 1–3 of `rfc-changes.md` (decisions only) | ~2 weeks |
| `phase-b-security-correctness.md` | The 11 verified P0 defects + regression tests | — (parallel with A; §1.2 needs A's contract decision) | 3–4 weeks |
| `phase-c-ci-foundation.md` | CI matrix, stable toolchain, PR-gating, hygiene | — (no code risk, start anytime) | ~1 week |
| `phase-d-hardening.md` | Locking rework, memory bounds, metrics, disk GC | B (and D-internal ordering) | 3–4 weeks |
| `phase-e-features.md` | Policies dir, roots, policy-engine trait, conformance pack, cleanups | A (§2.2 for the trait), D (§4.1 for §5.5) | 2–3 weeks |

Phase F (transcript visualizer, buf.build publishing) is opportunistic and small; it
rides along in `phase-e-features.md` §6.

## Ground rules (from the master plan)

- **Freeze discipline**: nothing in Phase B–E may change wire behavior or public API
  shape; if a task turns out to need that, it moves to Phase A or waits for the next
  breaking window.
- **Replay migration rule** (master §1.9/§2.5): any semantic change on message
  acceptance applies to **new sessions only**, gated on a recorded marker; legacy
  logs replay under legacy semantics; every such change ships with a pre-fix log
  fixture test.
- **Every defect fix lands with its regression test** (master §5.4 lists them).
- Completed items move the relevant plan file to `plans/done/` (create it) with a
  completion note; do not silently delete.

## Status board

| Phase | Status |
|---|---|
| RFC filings | not started |
| A — pre-freeze | not started |
| B — security & correctness | not started |
| C — CI foundation | not started |
| D — hardening | not started |
| E — features | not started |
