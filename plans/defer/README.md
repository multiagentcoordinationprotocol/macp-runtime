# Deferred Work

**Last reviewed**: 2026-07-05 (against v0.5.0 — the improvement plan is
EXECUTED and released; all seven crates at 0.5.0 on crates.io)

Two kinds of remaining work live here:

- **`follow_ons.md`** — actionable, scoped items that deliberately did not
  gate the v0.5.0 release (handoff timer implementation, ListSessions
  pagination, JSON client-fallback deprecation, persistence optimizations,
  small/cosmetic items). Not blocked; pick up any time.
- **The hard-blocked table below** — items with external blockers,
  unchanged.

History: on 2026-07-03 this directory was merged into
**`plans/IMPROVEMENT_PLAN.md`** (the master plan), which was decomposed on
2026-07-04 into execution plans under `plans/current/`. Both were DELETED on
2026-07-06 after the full-implementation audit (git history retains them;
`plans/BUILD_STATUS.md` remains as the execution record). All actionable
deferred items had been promoted into that roadmap — and, as of 2026-07-05,
all of them shipped (multi-round proto in 0.1.4/A7; test gaps in Phase B;
replay validation D7; conformance pack C5/E4 + CI oracle; recovery benches
D1; policy engine + audit E3; visualizer + buf.build publishing in Phase F
— BSR push verified on spec-repo main). Their absorbed-item table is kept
for the paper trail:

| Absorbed item | Now lives at |
|---|---|
| Multi-round protobuf payloads | `IMPROVEMENT_PLAN.md` §4.5 (Phase A) |
| Test gaps (TTL, Watch RPCs, Signal mutation, FileBackend atomicity, concurrent stress) | §5.4 (Phase B) |
| Replay consistency validation | §5.5 (Phase D) |
| Conformance pack publishing + canonical fixture names | §5.7 (Phases C/E) |
| Recovery benchmarking | §5.6 (Phase D) |
| Pluggable policy-engine trait, policy-driven audit logging | §4.6 (Phase E) |
| Transcript visualizer, buf.build schema publishing | §4.7 (Phase F) |

## Still deferred (hard external blockers)

| Item | Plan file | Blocker |
|---|---|---|
| Session composition (parent/child, causal links) | `session_composition.md` | Needs RFC proto changes; spec-first design |
| Workflow primitives (triggers, guards, retry, cross-session deadlines) | `workflow_primitives.md` | Blocked on session composition; needs real usage patterns |
| Cross-runtime conformance validation | — | Requires a second MACP implementation |
| Tenant isolation | — | Requires multi-tenant demand + tenant identity in `AuthIdentity` |
| SQLite / PostgreSQL backends | — | No demand; file/RocksDB/Redis suffice |
| Fixture generator, state-diagram generator, `macp-mode-derive` proc-macro | — | No community mode-author audience yet |

## Obsolete (won't do — unchanged from 2026-04-07 review)

| Item | Why |
|---|---|
| New experimental modes (Review, Debate, Negotiation, Planning, Escalation, Approval Chain) | No demand; existing 5 modes cover use cases |
| Dynamic mode plugin loading (shared libs/WASM) | Trait-based `ModeFactory` registration suffices |
