# Phase B — Security & Correctness (P0 defects)

**Source:** master plan §1.1–§1.7, §1.11, §5.4 · **Effort:** 3–4 weeks (honest
estimate — §B3 spans three backends, §B4 includes an async refactor) · **Rule:**
every fix lands with its regression test; no wire-behavior changes except where the
current behavior is itself a spec-conformance bug (B2).

## Workstream 1 — streaming correctness

### B1. Lag surfacing + WatchSessions sync race (master §1.3) — small, do first
- `watch_signals` (`server.rs:1060`) and `watch_sessions` (`server.rs:1109`):
  match `RecvError::Lagged` → terminate with `ResourceExhausted` (pattern:
  `server.rs:528`).
- `watch_sessions` subscribe-before-snapshot race: dedupe buffered **`Created`**
  events by `session_id` against the initial snapshot; non-`Created` events pass
  through.
- **Tests:** slow-consumer stream ends with `ResourceExhausted`; session created
  inside the subscribe window appears exactly once.

### B2. Passive-subscribe sequence contract (master §1.2 + §1.4 — one design item)
Contract (decided in Phase A window even if built here): per-session sequence =
**1-based ordinal of accepted Incoming envelopes**; `after_sequence` exclusive,
`0` = from start (RFC-0006 §3.2).
1. `log_store::get_incoming_after` (`log_store.rs:66-79`): filter on Incoming
   ordinal, exclusive comparison (fixes raw-index + off-by-one).
2. Duplicate-delivery race (`server.rs:348-401`): dedupe the drained broadcast
   buffer against the last replayed ordinal. Do **not** take the global registry
   lock across subscribe+read (conflicts with Phase D locking rework).
3. Compaction coupling (master §1.4): `replace_log` updates the in-memory
   `log_store` in the same operation; the checkpoint records the
   discarded-Incoming ordinal count; post-compaction history replay returns a
   clear error (not silence) when the requested range was compacted away.
4. Correct CLAUDE.md's `MACP_CHECKPOINT_INTERVAL` description (compaction is
   unconditional on terminal state).
- **Tests:** tier-1 resume across interleaved suspend/resume/checkpoint entries;
  accept-during-subscribe-window delivered exactly once; resume across
  compaction+restart (error case asserted); RFC exclusive-after conformance.

## Workstream 2 — storage durability

### B3. Backend durability parity (master §1.5)
1. RocksDB: `WriteOptions::set_sync(true)` on log appends (config knob, sync
   default). Benchmark before/after with Phase D's §5.6 harness if available.
2. Redis: document non-durable/cache-tier in `deployment.md`; atomic
   `replace_log` (MULTI/EXEC or Lua); startup warning or explicit
   `MACP_REDIS_ACKNOWLEDGE_NON_DURABLE=1` opt-in.
3. Corrupt-entry parity: per-entry skip + warn on all backends; fatal under
   `MACP_STRICT_RECOVERY`.
4. File backend: fsync tmp file before rename + parent-dir fsync in
   `atomic_write` (`file.rs:34-38`); persistent handle per active session log.
- **Tests:** RocksDB kill-after-ack recovery test; Redis `replace_log` failure
  injection; corrupt-line fixture per backend (skip vs strict); FileBackend
  `.tmp` cleanup test (from §5.4 item 4).

## Workstream 3 — auth hardening

### B4. JWT/JWKS (master §1.6)
1. Remove HS256 from the default allowlist (`security.rs:154-159`) — explicit
   config only. Release-note: coordinate with `auth-service` (RS256, unaffected).
2. JWKS fetch: dedicated `reqwest::Client` with connect/total timeouts
   (`jwt_bearer.rs:122`).
3. Stale-cache fallback with bounded stale window (`jwt_bearer.rs:107-118`).
4. `kid`-based key selection (`jwt_bearer.rs:219-240`).
5. Replace `block_in_place`/`block_on` (`security.rs:244-252`) with async
   authentication end-to-end.
- **Tests:** HS256 token rejected under default config; JWKS endpoint down →
  auth continues on stale keys within window, fails after; hanging JWKS endpoint
  times out; `kid` mismatch falls back correctly.

### B5. Dev-mode fallback gate (master §1.7)
Refuse startup with no configured auth unless `MACP_ALLOW_INSECURE=1`; fix the
false comment at `security.rs:90`. **Ships together with** the Dockerfile
`MACP_ALLOW_INSECURE` removal (Phase C, C4) + quickstart doc updates + release
note (compound break, intentional).
- **Test:** startup matrix — no auth + no flag → exit non-zero; no auth + flag →
  dev mode with warning log.

### B6. WatchSignals auth + rate limiter (master §1.1, §1.11)
- `watch_signals`: require `authenticate_metadata` (`server.rs:1055`). Record the
  decision on `WatchModeRegistry`/`WatchRoots` (stay open as discovery) and the
  `ListSessions`/`WatchSessions` scoping asymmetry in `docs/deployment.md`.
- Rate limiter (`security.rs:295-332`): implement the capped prune the comment
  promises (or periodic prune task); fix the comment.
- **Tests:** unauthenticated `WatchSignals` → `Unauthenticated`; limiter
  behavior unchanged under cap (existing tests) + a many-sender prune test.

## Exit criteria
- All master §5.4 Phase-B regression tests green, including the five promoted
  `test_gaps.md` tests (TTL expiry, Watch RPCs initial event, Signal-no-mutation,
  FileBackend atomicity, concurrent SessionStart stress).
- No P0 section of the master plan remains OPEN except §1.12 (refiled to Phase D).
