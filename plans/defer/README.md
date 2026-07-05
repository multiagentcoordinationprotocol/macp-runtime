# Deferred Work

**Last reviewed**: 2026-07-03 (against v0.4.0 codebase)

On 2026-07-03 this directory was merged into **`plans/IMPROVEMENT_PLAN.md`** (the
master plan), which was decomposed on 2026-07-04 into execution plans under
`plans/current/`. All actionable deferred items were promoted into that roadmap and
their plan files deleted (content absorbed, stale pre-workspace paths corrected):

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
