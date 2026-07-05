# Phase D — Production Hardening

**Source:** master plan §3.1–§3.4, §4.1 (+§1.12), §4.3, §5.5, §5.6 · **Effort:**
3–4 weeks · **Depends on:** Phase B complete (B2's sequence contract in
particular). Internal order matters — benchmarks first, locking rework alone.

## Ordered tasks

### D1. Recovery/throughput benchmarks first (master §5.6)
Criterion benches: replay time vs log size (100/1K/10K), checkpoint vs full
replay, memory during large-session replay, message throughput under concurrent
sessions. This is the baseline that D2 and B3's sync-write costs are judged
against. Keep the bench harness in-repo (`benches/`).

### D2. Per-session locking (master §3.1) — largest change, land alone
Replace the global `registry.sessions` `RwLock` held across storage I/O
(`runtime.rs:359-486`, `:508-599`, and the `cleanup_expired_sessions` sweep at
`:904-949`) with per-session serialization (`DashMap<SessionId,
Arc<Mutex<SessionSlot>>>` or actor-per-session). RFC-0001 §8.1 requires
serialization only *within* a session.
Prereqs/invariants to preserve:
- `max_open_sessions` check-and-insert stays atomic (global counter or striped
  lock for the create path).
- RocksDB `next_seq` read-modify-write (`rocksdb.rs:43-76`) is currently safe
  *because* of the global lock — the per-session lock suffices (keys are
  session-scoped), but verify no cross-session key exists.
- Dedup-slot ordering invariant (append COMMIT POINT before slot insert) is
  per-session; unaffected, but re-run the full conformance + concurrency suites.
- **Tests:** concurrent SessionStart stress (from §5.4) still exact;
  cross-session throughput bench shows improvement vs D1 baseline; single-session
  ordering unchanged.

### D3. Memory bounds (master §3.2)
- Evict `log_store.logs` together with registry eviction
  (`runtime.rs:953-976`).
- Add `SessionStreamBus` channel removal (`stream_bus.rs:27-36` has no removal
  API) — evict on the same path and on terminal state with no receivers.
- Document `seen_message_ids` growth for long-lived sessions.
- **Test:** eviction leaves no entry in registry, log_store, or stream bus for
  the evicted session (memory-liveness assertion).

### D4. Server limits + graceful shutdown (master §3.3)
- tonic: `concurrency_limit_per_connection`, `max_concurrent_streams`, request
  timeout, TCP keepalive; set `max_decoding_message_size` from
  `MACP_MAX_PAYLOAD_BYTES` so the ingress bound is real.
- `serve_with_shutdown` + drain timeout on ctrl-c; final session snapshot after
  drain.
- **Tests:** in-flight unary completes during shutdown window; oversized gRPC
  frame rejected at the transport bound.

### D5. Metrics export (master §4.1, absorbing §1.12)
1. Wire `record_message_rejected`/`record_commitment_rejected` (zero callers
   today) into the `Send` error path (`server.rs:725`) and mode-rejection paths;
   add suspended/resumed to `MetricsSnapshot`.
2. Prometheus `/metrics` HTTP endpoint (feature-gated with `otel` or its own
   feature): mode counters, rejections, storage metrics (append latency, fsync
   failures, log sizes, compaction counts, recovery skips), auth metrics (auth
   failures, rate-limit hits, JWKS refreshes), stream metrics (lag drops).
3. Document scraping in `deployment.md`.
- **Test:** counter increments observable through the endpoint for one accepted
  + one rejected message.

### D6. On-disk retention/GC (master §4.3)
`MACP_SESSION_DISK_RETENTION_SECS`: terminal sessions past retention are archived
(JSONL export) or deleted (`storage.delete_session` — currently zero callers).
Runs in the existing cleanup task. **Test:** expired-then-retained session
disappears from disk and does not reload on restart.

### D7. Replay consistency validation (master §5.5 — after D5 and B2)
Warn-only replay-vs-snapshot comparison at startup (state, participants,
versions, dedup count) + `replay_mismatches` counter surfaced via D5. False
positives were the original deferral reason — B2's compaction fix removes the
main source; warn only on state/dedup-count mismatches.

## Exit criteria
- Throughput under multi-session load improves measurably vs D1 baseline
  (target: storage latency on one session no longer stalls others).
- Zero unbounded in-memory maps tied to session lifetime.
- An operator can scrape health, throughput, rejection, and storage metrics.
