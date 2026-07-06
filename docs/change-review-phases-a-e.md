# Change Review: Improvement Plan Phases A–E

**Commit:** `5d9fb5e` on `feat/improvement-plan-phases-a-e` · **Baseline:** v0.4.0 (`732b689`)
**Purpose:** task-by-task engineering review — what changed, why, the RFC basis,
how it was verified, and any residual risk. Written for a reviewer deciding
whether this change set is correct, architecturally sound, and RFC-conformant.

**Verification baseline for the whole set:** 529 workspace unit/integration
tests, clippy 0 warnings (including feature-gated code), tier-1 gRPC suite
75/75, JWT suite 8/8 — each phase gated end-to-end before the next began.
Provenance: the underlying plan was itself produced by a five-agent code/spec
audit and survived two adversarial review passes (~40 file:line claims
spot-checked, zero found fabricated); see `plans/IMPROVEMENT_PLAN.md`.

Conventions: **RFC basis** cites the normative clause the change serves.
**Risk** is honest residual exposure, not boilerplate.

---

## Phase A — Pre-freeze API and wire work

### A1. `Session` builder + `#[non_exhaustive]` public types
- **What:** `Session::builder(session_id, mode, initiator_sender)` is now the
  only construction path outside `macp-core`; `Session`, `MacpError`,
  `ModeResponse`, `PolicyDecision`, `PolicyError` are `#[non_exhaustive]`.
  All 19 construction sites migrated (2 production, 1 storage conversion, 16
  fixtures).
- **Why:** v0.4.0 is a freeze candidate. A 25-public-field struct with literal
  construction makes *any* future field addition a breaking change across
  every consumer. This is the classic pre-1.0 evolvability fix.
- **Architectural note (deliberate exception):** `SessionState` was **not**
  marked non-exhaustive. A new session state is a protocol-breaking event
  (RFC-0001 §7.2 explicitly called the SUSPENDED/CANCELLED addition breaking);
  forcing downstream `_` arms would let a new state silently pass state
  machines that must fail loudly instead.
- **Security hardening found during migration:** all five modes gated
  commitments with `if let PolicyDecision::Deny` — anything that wasn't
  literally `Deny` (including any future variant) was treated as **allow**.
  Inverted to explicit fail-closed matches: only `Allow` proceeds.
- **RFC basis:** none directly; freeze-profile engineering. The fail-closed
  inversion serves RFC-0012 §6.2 (commitment must satisfy policy).
- **Verified:** full suite green post-migration; the builder's defaults are
  documented (`ttl_expiry: i64::MAX` — matches every prior fixture, and both
  production paths always override it).
- **Risk:** low. Builder default of "never expires" could mislead a library
  consumer who forgets to set TTL; documented on the builder. The kernel path
  always sets it from the validated payload.

### A2. `PolicyEvaluator` unified around `CommitmentContext`
- **What:** the trait collapsed from six positional per-mode methods to one
  required `evaluate_commitment(&CommitmentContext)`; context carries policy,
  participants, `outcome_positive`, and a `#[non_exhaustive]` per-mode state
  enum. Old methods remain as `#[deprecated]` defaulted shims (old→new
  delegation; no circularity). The 96 policy tests were untouched in
  signature because they exercise the free functions, not the trait.
- **Why:** freezing five positional-primitive methods
  (`evaluate_quorum_commitment(usize, usize, usize, usize)`) locks in a
  transposition-prone API and locks four modes out of outcome-awareness —
  the exact direction RFC-0012 schema_version 2 (decline outcomes) had just
  taken.
- **Two RFC-conformance fixes shipped with it:**
  1. **Quorum threshold** (master plan §1.10): the evaluator reinterpreted the
     policy `threshold` as a *participation quorum* over approve+reject
     voters, while the mode reads it as an approval bar. RFC-0012 §4.2
     adjudicates: threshold "overrides the `required_approvals`" — the mode's
     reading. The participation reinterpretation was removed; one test that
     had encoded the wrong behavior (2 approvals passing a threshold of 3 via
     abstention "participation") now correctly asserts denial.
  2. **Decline outcomes across all modes**: task `require_output` no longer
     denies a `TaskFail` (a failure has no output by nature); proposal
     `max_rounds` no longer denies a terminal-reject (the decline *is* the
     exit from an over-long negotiation — denying it trapped the session
     until TTL); quorum declines aren't gated by the approval threshold.
- **RFC basis:** RFC-0012 §4.2 (threshold semantics), §6.2/§6.3 (commitment
  evaluation), RFC-0007 §6.2 (decline gating, previously Decision-only).
- **Verified:** 6 new decline-path tests; conformance vectors; 102 policy
  tests green; the `DenyAllEvaluator` injection test still proves modes
  consult the injected evaluator.
- **Risk:** medium-low. Behavior deliberately changed where the old behavior
  was non-conformant. Any external consumer relying on the participation
  reading of quorum `threshold` sees different results — this is a bug fix
  per the RFC, and CHANGELOG documents it. Positive quorum commitments with
  approvals below a bound threshold are now denied even when the mode's
  `commitment_ready` allowed the unreachable-threshold path — this closes an
  action/outcome consistency gap rather than opening one.

### A3. `policy.default` echo contract
- **What:** empty `CommitmentPayload.policy_version` now matches the
  session's bound policy; non-empty must match exactly.
- **Why:** the runtime rewrites an empty SessionStart `policy_version` to
  `policy.default` (RFC-0012 §6.1) and then demanded the Commitment echo a
  value the client never sent — an interop trap hidden by unit tests that
  constructed sessions with empty policy_version directly.
- **RFC basis:** RFC-0012 §6.1 defines resolution; the echo question is
  genuinely unspecified upstream (filed as `rfc-changes.md` item 3). Empty-
  matches is forward-compatible with either upstream resolution: if upstream
  mandates echoing, clients that echo still pass; if upstream blesses empty,
  we're already correct.
- **Verified:** 3 unit tests (empty matches, wrong value rejected, exact
  value accepted).
- **Risk:** low. Slightly more permissive than before; cannot accept a
  *wrong* policy version, only an absent one.

### A4. Session-ID validation fix
- **What:** 36-char base64url tokens containing `-` were hard-routed into the
  UUID branch and rejected. Now: UUID-*parseable* strings get strict UUID
  rules (canonical lowercase, v4/v7, no fall-through); only non-UUID-shaped
  strings fall through to the base64url rule.
- **Why the no-fall-through subtlety matters:** `uuid::parse_str` is
  case-insensitive and uppercase hex is valid base64url charset — naive
  fall-through would have silently started accepting non-canonical
  (uppercase) UUIDs as distinct tokens, weakening ID canonicalization.
- **RFC basis:** RFC-0004 §5 (session IDs cryptographically strong; the
  base64url 22+ char rule is this runtime's documented acceptance policy).
- **Verified:** regression test for the accepted shape; test proving
  UUID-shaped-but-wrong-version still rejects; existing uppercase-UUID
  rejection test still green.
- **Risk:** negligible; strictly widens acceptance to IDs the documented
  policy already claimed to accept.

### A5. Extension-mode hardening (three holes)
- **What:**
  1. `PromoteMode` can no longer re-key a mode into the reserved
     `macp.mode.*` namespace (and a failed promotion provably doesn't mutate
     the registry entry).
  2. Extension descriptors must declare ≥1 terminal message type, and only
     `Commitment` — because dynamically registered modes are
     passthrough-backed and passthrough resolves on nothing else; any other
     advertised terminal was a lie to clients and a session dead-end.
  3. **Version binding:** SessionStart omitting `mode_version` on a
     non-strict ext mode binds the registered descriptor's version, recorded
     in a new `LogEntry.bound_mode_version` field. Previously the session
     bound `""` and the commitment version check matched `""` vacuously —
     the freeze invariant "commitment versions must match session-bound
     versions" was fictional for ext modes.
- **Replay migration (the load-bearing part):** replay uses the *recorded*
  binding, never the live registry (dynamic registrations may have changed
  or vanished across restarts — proven by a test whose live registry
  deliberately carries a different version). Legacy log entries deserialize
  the new field as `None` (serde default) and keep their original vacuous
  semantics — old histories replay to the exact outcomes they were accepted
  with (RFC-0003 §1).
- **RFC basis:** RFC-0002 §12 (reserved namespace), RFC-0003 §3 (version
  binding immutability), RFC-0003 §1 (replay integrity).
- **Verified:** 7 new tests including a legacy-JSON deserialization fixture;
  tier-1 fixture updated (it had been registering a descriptor that could
  never resolve — the exact defect class being closed).
- **Risk:** low. Registration of previously-accepted degenerate descriptors
  now fails — an intentional, documented behavior change.

### A6. Handoff implicit-accept trust model (interim fix)
- **What:** the implicit-accept timeout is now measured against the runtime's
  acceptance clock instead of the client-supplied envelope timestamp, which
  let an initiator post-date a Commitment to finalize an offer the target
  never accepted.
- **Mechanism:** new `MessageContext` (non_exhaustive) + defaulted
  `Mode::on_message_at` kernel entry point — modes that don't need a clock
  are untouched (zero churn across ~330 existing `on_message` call sites);
  only `HandoffMode` overrides it. The runtime computes **one** acceptance
  timestamp per message, passes it to the mode AND records it as the log
  entry's `received_at_ms`; replay feeds the recorded value back — live and
  replay observe the identical clock, preserving determinism.
- **Migration:** `Session.semantics_rev` (`CURRENT_SEMANTICS_REV = 1`)
  recorded on the SessionStart log entry and in snapshots; rev-0 (legacy)
  sessions keep the envelope clock so pre-fix histories replay unchanged.
- **What this is NOT:** RFC-0012 §4.5 actually describes a runtime *timer*
  emitting a synthetic accept into history. That contract is underspecified
  upstream (no timing source/authority/suspension semantics — filed as
  `rfc-changes.md` item 2); this change removes the forgeability without
  pretending to implement the timer.
- **Verified:** forged-future-timestamp test (no accept, commitment rejected;
  genuine elapsed acceptance time fires), legacy rev-0 clock test, replay
  semantics-rev preservation test.
- **Risk:** medium-low. The `semantics_rev` mechanism adds a versioning
  concept to the session model — deliberate, extensible design for future
  acceptance-semantics changes, but one more thing implementors must
  understand. Documented on the constant.

### A7. Multi-round proto — **BLOCKED (upstream)**
Not implemented: requires adding the proto to the spec repo's `macp-proto`
package and a crates.io release first. The implementation plan (with the
replay-compatibility JSON fallback requirement) is in
`plans/current/phase-a-prefreeze.md` §A7.

### A8. Roots capability decision
- **What:** `Initialize` now advertises `roots{list_roots: true,
  list_changed: false}`.
- **Why:** `ListRoots` truthfully answers with the empty set (a valid state),
  but no roots provider exists, so the set can never change — advertising
  change notifications promised events that could never arrive.
- **RFC basis:** RFC-0006 §3.3 gates `WatchRoots` on `list_changed`.
- **Verified:** tier-1 test pins the advertisement.
- **Risk:** none; strictly more honest. Revisit if a consumer appears (E2).

---

## Phase B — Security & correctness

### B1/B6a. Watch-stream correctness + WatchSignals auth
- **What:** (1) `watch_signals`/`watch_sessions` surface consumer lag as
  `RESOURCE_EXHAUSTED` instead of silently closing `Ok` — a slow consumer can
  now distinguish "no traffic" from "events dropped" (mirrors the existing
  `StreamSession` behavior). (2) `watch_sessions` subscribed to the lifecycle
  bus *before* reading the initial snapshot (correct — nothing missed) but
  emitted window events twice; buffered `Created` events are now deduped by
  `session_id` against the synced set — sound because session IDs are
  create-once, and non-`Created` events always pass through. (3)
  `WatchSignals` requires authentication.
- **Honest citation note:** RFC-0004 §4.1 constrains unauthenticated signal
  *producers*, not subscribers — subscriber auth is this runtime's hardening
  posture (signal payloads are agent data; the stream is an unmetered
  resource), documented as a choice rather than passed off as a spec mandate.
- **Verified:** tier-1 unauthenticated-`WatchSignals` test;
  exactly-once-`Created` test (sessions created before AND after subscribe).
- **Risk:** low. Lag now terminates streams that previously died silently —
  clients must reconnect, which is the correct contract.

### B2. Passive-subscribe sequence contract (with compaction coupling)
- **What:** one coherent contract replacing three defects:
  - Sequence = **1-based ordinal of accepted envelopes**; internal
    (suspend/resume/TtlExpired) and checkpoint entries never consume
    ordinals → client-visible sequences are contiguous and stable.
  - `after_sequence` is **exclusive** (`0` = from start), fixing an
    off-by-one against RFC-0006 §3.2's "starting from `after_sequence + 1`"
    (a conformant client resuming with its last-seen sequence was getting
    that envelope re-delivered).
  - Subscribe-window duplicates: the drained broadcast buffer is deduped
    against replayed message-ids, disarming on first miss — valid because
    the receiver is FIFO and all in-window events precede post-snapshot
    events. Deliberately **not** fixed by widening the global lock, which
    Phase D was about to remove.
  - Compaction records `compacted_incoming_ordinals` on the checkpoint
    (accumulating across repeated compactions); post-compaction ordinals
    continue from the base across restarts; resuming below the base returns
    `FAILED_PRECONDITION` instead of silently skipping missing history; and
    `replace_log` now updates the in-memory log store in step with disk
    (previously they diverged on every terminal session — compaction is
    unconditional on terminal state, not gated by `MACP_CHECKPOINT_INTERVAL`;
    CLAUDE.md corrected).
- **RFC basis:** RFC-0006 §3.2 (normative passive-subscribe semantics; the
  ordinal definition itself is being proposed upstream as `rfc-changes.md`
  item 14 so other implementations converge).
- **Verified:** log-store ordinal tests (interleaved internal entries;
  exclusive resume), compaction-base tests (ordinals continue; below-base is
  an error), tier-1 passive-subscribe suite green.
- **Risk:** medium. This changes observable wire behavior on a frozen proto
  field — justified because the previous behavior was a conformance bug and
  unusable for resume (non-contiguous, meaning-shifting indices). The sibling
  `macp-control-plane` consumes this path and should be re-tested against
  the new (RFC-correct) semantics before deploying both.

### B3. Backend durability parity
- **What:** RocksDB log appends set `WriteOptions::set_sync(true)` — the
  append is the runtime's commit point, and an acked message must survive a
  crash (previously WAL-buffered only; the file backend already fsynced).
  Session snapshots stay async on purpose (log is the source of truth;
  snapshots reconstruct via replay). Redis `replace_log` became one
  MULTI/EXEC transaction (was DEL + N RPUSHes — a mid-sequence failure
  silently truncated history) and the backend logs a durability disclosure
  at connect (no WAIT/AOF barrier; single-writer only). Corrupt-entry
  handling unified: RocksDB/Redis skip+warn per entry like the file backend
  (one bad record no longer drops an entire session). File `atomic_write`
  fsyncs the tmp file before rename and the parent dir after (the rename
  could previously be durable while the data was not).
- **RFC basis:** RFC-0003 §1 (acceptance durability underpins replay
  guarantees); CLAUDE.md freeze invariant "log append failures are fatal —
  never ack without a durable record".
- **Verified:** rocksdb corrupt-entry regression test; 23 feature-gated tests
  now actually run (see C1); benchmarks confirm the sync-write cost is real
  (~12ms/fsync on the test machine) and therefore honest.
- **Risk:** medium-low. RocksDB append throughput drops with sync writes —
  correct-by-default; a config knob can be added if a deployment explicitly
  wants the old semantics. Redis remains non-durable at the power-loss level;
  that is now *disclosed* rather than fixed, which is the honest scope (the
  deployment docs carry the matrix). Deferred: per-append persistent file
  handles (needs benchmark-driven design, noted in the plan).

### B4. JWT/JWKS hardening (five items)
- **What:** (1) HS256 removed from the default algorithm allowlist
  (RS256/ES256 only) — if a JWKS ever contains an `oct` key, symmetric
  tokens must not silently become verifiable; explicit opt-in via new
  `MACP_AUTH_JWT_ALGS`. (2) JWKS fetches get connect/total timeouts (3s/5s) —
  a hanging endpoint no longer blocks the auth path indefinitely. (3)
  Stale-cache grace window (1h): a JWKS endpoint outage no longer disables
  ALL JWT auth the moment the TTL expires; rotation still converges on the
  next good fetch (bounded staleness trade-off, documented on the constant).
  (4) `kid`-based key selection with try-all fallback. (5) The
  `block_in_place`/`block_on` bridge is gone — `authenticate_metadata` is
  async end-to-end (the old bridge parked a worker thread per JWKS fetch and
  panicked on current-thread runtimes).
- **RFC basis:** RFC-0004 §3 (authentication mechanisms), §7 (availability
  under DoS conditions).
- **Verified:** 43 auth unit tests; **the tier-1 gate caught the intended
  break in flight** — the JWT test harness signs HS256 and failed under the
  new default; it now opts in via the env knob (exercising it end-to-end)
  and a NEW test permanently pins the security property that HS256 is
  rejected without opt-in.
- **Risk:** medium-low. Breaking for shared-secret JWT deployments — by
  design, with a one-variable escape hatch and CHANGELOG/release-note
  coverage. The sibling `auth-service` mints RS256 and is unaffected. The
  stale-cache window slightly extends how long a rotated-out key verifies
  (≤ TTL+1h) — a standard availability/rotation trade-off, documented.

### B5. Dev-mode auth gate
- **What:** with no auth configured, startup now fails with an actionable
  error unless `MACP_ALLOW_INSECURE=1`. Previously any bearer token silently
  became a **fully-privileged** identity (session start + mode-registry
  admin) — and the code comment claimed the path was test-only, which was
  false (fixed).
- **Design choice:** reuses the existing TLS opt-in flag so local dev stays a
  one-variable flow. Ships together with the Docker change (C4) that removes
  the baked-in `MACP_ALLOW_INSECURE=1` — the compound break is intentional
  and documented: a bare `docker run` now fails fast instead of running an
  any-token-is-admin server.
- **RFC basis:** RFC-0004 §1/§3 (authenticated senders are a MUST).
- **Verified:** tier-1 test spawns the real binary with a scrubbed
  environment and asserts non-zero exit + the explanatory message; the
  integration harness was confirmed to set the flag *before* the change.
- **Risk:** low; loud by design.

### B6b. Rate-limiter sweep
- **What:** the per-request full-map stale-sender scan (O(total senders),
  attacker-influencable via distinct authenticated identities) — whose
  comment falsely claimed a 100-entry cap — replaced by an amortized full
  sweep every 128 requests; between sweeps a request touches only its own
  deque. Note the plan's original "cap removals at 100" idea was rejected
  during implementation because it still *scanned* the whole map; the
  amortized design bounds the scan itself.
- **Verified:** a bounding test: 49 stale senders fully swept once the
  boundary ticks; map contains only the live sender after.
- **Risk:** negligible; between sweeps the map can hold ≤127 extra stale
  entries — bounded and tiny.

---

## Phase C — CI, docs, hygiene

### C1–C3. CI rework
- **What:** main jobs moved to stable Rust with 1.89 retained as a dedicated
  MSRV `check` job (previously *everything* ran only on the pin — stable
  regressions surfaced first at publish time); a `features` job finally
  compiles, lints, and tests `rocksdb-backend`/`redis-backend` — with a live
  `redis:7` service container, so the Redis tests **run in CI for the first
  time ever** (they previously self-skipped everywhere, meaning a broken
  backend could not fail any build — proven real by A1's builder migration,
  which had silently broken the feature-gated fixtures); the tier-1 gRPC
  suite (incl. JWT) now gates PRs; `cargo audit` is advisory
  (`continue-on-error`, out of the required gate) so a new upstream RUSTSEC
  advisory can't red an unrelated PR.
- **Risk:** CI wall-time grows (~5–10 min for tier-1). The YAML is validated
  but has not executed on GitHub yet — the first PR run is the real test.

### C4. Docker + docs + repo hygiene
- **What:** Dockerfile drops the baked-in `MACP_ALLOW_INSECURE=1` (see B5)
  and the cache-busting `COPY tests/`; `temp/` (stale zip + broken script +
  outdated docs) deleted; `CHANGELOG.md` (every user-visible change),
  `SECURITY.md` (reporting + security model), `CONTRIBUTING.md` (invariants,
  gates, test matrix) added; `docs/deployment.md` gains the **backend
  durability matrix**, the single-writer warning, dev-mode opt-in docs, and
  records the `ListSessions`/`WatchSessions` all-sessions-visibility decision
  (deliberate, RFC-0006-sanctioned, now documented rather than accidental).
- **Risk:** none beyond the intentional B5 compound break.

### C5. Canonical conformance format
- **What:** all 13 fixtures + the loader atomically renamed from
  Rust-internal shorthand (`decision.Proposal`) to fully-qualified proto
  names (`macp.modes.decision.v1.ProposalPayload`, `macp.v1.CommitmentPayload`);
  `tests/conformance/schema.json` (draft-2020-12) added — including
  `Cancelled`/`Suspended` final states and the already-implemented optional
  validation fields the original defer-doc draft predated; a dependency-free
  format-guard test prevents drift back to shorthand.
- **Why:** this is the format the cross-runtime conformance pack requires;
  the spec repo's fixtures use canonical names, so convergence had to happen
  on this side.
- **Risk:** none; loader and fixtures changed in one commit, 14 conformance
  tests green.

---

## Phase D — Production hardening

### D1. Benchmarks (criterion, `benches/replay_bench.rs`)
- **What/found:** replay is cleanly linear (83µs/810µs/8.1ms for
  100/1k/10k entries — no superlinear pathology); memory-backend sends show
  the lock was invisible when storage is free (1.6µs both single- and
  cross-session); the **decisive baseline**: 8 concurrent sends to 8
  *different* sessions on the fsyncing file backend = **95ms** — eight
  ~12ms fsyncs fully serialized by the global lock.
- **Risk:** none (measurement only). Live-path benches must use wall-clock
  timestamps (TTL interacts with real time) — learned and documented in the
  bench.

### D2. Per-session locking — the largest architectural change
- **What:** `SessionRegistry` now stores `Arc<tokio::Mutex<Session>>` per
  entry (`SharedSession`). The map `RwLock` is held only for
  lookup/insert/remove; each session's mutex is held across
  validate → fsync append → commit. Documented lock-ordering rule: map lock
  before session mutex, never hold the map lock while awaiting a session
  mutex (snapshot the Arcs, drop the guard, then lock).
- **The five preserved invariants (each was explicit in the design dossier):**
  1. *Per-session acceptance serialization* (RFC-0001 §8.1 — required within
     a session, never across): the session mutex provides exactly this.
  2. *`max_open_sessions` TOCTOU safety*: SessionStart uses
     reserve-and-rollback — dedup check + open-count + placeholder insert
     are atomic under a brief map write; storage I/O runs with the map
     unlocked; on failure the rollback **poisons the placeholder to
     non-Open before removing it**, so a concurrent waiter that already
     cloned the Arc fails the OPEN gate instead of appending history to a
     session whose start never committed.
  3. *Dedup invariant* (rejected messages never consume slots): the
     append-commit-point-before-slot-insert ordering is unchanged inside the
     session-mutex critical section.
  4. *RocksDB `next_seq` safety*: its read-modify-write is per-session-keyed
     and previously safe only because of the global lock; the per-session
     mutex preserves exactly the needed serialization.
  5. *TTL sweep / cancel / suspend / resume*: all converted to take the same
     per-session mutex; the background sweep snapshots handles under a brief
     read and re-checks state under each session's own lock.
- **Measured result:** 95ms → 60ms (−38%) on the contended benchmark. The
  honest reading: the architecture no longer serializes sessions — the
  remaining ceiling is device fsync bandwidth (two fsyncs per message:
  durable append + snapshot; the SSD serializes flush barriers). A snapshot
  debounce is noted as the future lever (log is authoritative; snapshots are
  best-effort).
- **Verified:** 523+ tests green including the pre-existing TOCTOU test
  (`max_open_sessions_enforced_under_write_lock`), dedup-invariant tests,
  log-append-failure rollback tests, and same-session concurrency stress;
  tier-1 gate green.
- **Risk:** **medium — this is the change to review most carefully.**
  Specific reviewable points: (a) the open-session *count* uses `try_lock`
  per entry with locked-entries-count-as-open — conservative for a rate
  limit, but a reviewer should confirm they accept that bias; (b) a
  SessionStart retry racing its own failed first attempt can transiently get
  `SessionAlreadyExists` until the rollback completes (client retry
  succeeds); (c) `get_all_sessions`/`persist_snapshot` lock sessions one at
  a time — consistent per session, not a global atomic snapshot (same as
  before, but worth stating).

### D3. Memory bounds
- **What:** eviction now clears **all three** previously-unbounded maps:
  registry entry, in-memory log cache (`LogStore::remove_session_log` — was
  *never* evicted, retaining full logs incl. payloads for the process
  lifetime), and the stream broadcast channel (`remove_if_unused` — the
  channel map had **no removal API at all**; receiver-safe: skipped while
  subscribers remain, retried next sweep). Cancelled sessions became
  evictable (previously only Resolved/Expired — an oversight from before
  the Cancelled state existed).
- **Verified:** receiver-safety test on the stream bus; eviction covered by
  the existing sweep tests plus D6's memory-liveness assertion.
- **Risk:** low. `seen_message_ids` per *live* session still grows unbounded
  (dedup is normative; a windowed design needs spec work — documented, not
  hidden).

### D4. Server limits + graceful shutdown
- **What:** tonic gets per-connection concurrency, max-streams, request
  timeout, and keepalive limits (env-tunable; RFC-0004 §7 DoS posture);
  `max_decoding_message_size` now tracks `MACP_MAX_PAYLOAD_BYTES` + fixed
  envelope overhead — previously tonic's 4MB default applied *before* the
  payload check, so configured limits above 4MB were silently ineffective
  and huge frames were decoded before rejection; `serve_with_shutdown` with
  a hard drain deadline (`MACP_SHUTDOWN_DRAIN_SECS`) — required because
  long-lived watch streams would otherwise hold graceful shutdown open
  forever.
- **Verified:** tier-1 test SIGINTs the real binary and asserts clean exit
  code within the deadline (observed <1s).
- **Risk:** low. Default limits are conservative guesses; operators tune via
  env. Both ctrl-c listeners use `tokio::signal::ctrl_c()`, which supports
  concurrent listeners (verified against tokio semantics).

### D5. Metrics export
- **What:** the rejection counters — which had **zero callers** since they
  were written — now record at the `Send` error path (per-mode, commitments
  separately); suspended/resumed counts joined `MetricsSnapshot` (they were
  collected then silently dropped); an opt-in (`MACP_METRICS_ADDR`),
  dependency-free Prometheus text endpoint serves everything including
  `macp_replay_mismatches_total`.
- **Design choice:** a ~30-line raw-TCP HTTP responder instead of an
  axum/hyper dependency — deliberate: one GET path, text format, no TLS
  (bind it to localhost/scrape networks), no new supply-chain surface.
- **Verified:** tier-1 test curls the endpoint of a running binary and
  asserts a 200 text-format response.
- **Risk:** low-medium. The endpoint is unauthenticated plaintext HTTP by
  design (standard for Prometheus scrape targets) — operators must bind it
  appropriately; documented. Counter labels are per-mode only (no
  per-session cardinality explosion).

### D6. Disk retention/GC
- **What:** `gc_disk_sessions` — the first caller `storage.delete_session`
  has ever had. Enumerates **storage** (not memory — eviction may already
  have dropped the registry entry, which was exactly why disk grew forever
  and every restart reloaded every session ever completed), deletes only
  terminal sessions past `MACP_SESSION_DISK_RETENTION_SECS` (default 0 =
  keep forever — retention is an explicit operator decision), clears
  in-memory remnants. Unreadable snapshots are deliberately left for
  operator inspection rather than guessed at.
- **Verified:** regression test — expired session deleted, open session
  untouched, GC'd session gone from memory.
- **Risk:** low. The sweep loads each stored session per cycle — O(stored
  sessions) I/O per cleanup interval; acceptable at current scale, noted for
  optimization if session counts grow very large.

### D7. Replay consistency validation
- **What:** warn-only comparison of the replayed session against its stored
  snapshot (state, dedup count, participants, bound versions) during
  recovery, **before** the snapshot is overwritten; divergence count surfaces
  as `macp_replay_mismatches_total`.
- **Why warn-only:** snapshots are best-effort by design (log is
  authoritative), so mismatches can be benign staleness; but state/dedup
  divergence is exactly the bug class RFC-0003's determinism guarantees
  forbid, so it must be *visible*. This was the promoted defer plan's own
  design including its false-positive mitigation (B2's compaction fix removed
  the main false-positive source first — sequencing was deliberate).
- **Verified:** unit test (identical sessions → 0; diverged state+dedup → 2).
- **Risk:** none (observational only).

---

## Phase E — Features

### E1. `MACP_POLICIES_DIR` (RFC-0012 §9 file-loaded profile)
- **What:** `PolicyRegistry::load_from_dir` loads `*.json` policy definitions
  in sorted (deterministic) order through the **same** `register` path as the
  RPC — schema validation, reserved-`policy.default` check, duplicate check.
  Loaded **before** session recovery: replayed sessions resolve their bound
  `policy_version` against the live registry, so ordering is
  correctness-critical, not cosmetic. With the dir configured the wire
  registry is genuinely read-only: `Initialize` advertises
  `register_policy: false` and the mutating RPCs return
  `FAILED_PRECONDITION` — file-loaded deployments get exactly one source of
  governance truth, which is what the RFC profile means. A
  configured-but-broken dir is fatal at startup.
- **Verified:** two tier-1 tests — the full profile (policy listed, register
  refused, capability honest) and the fail-fast path (invalid file →
  non-zero exit naming the variable).
- **Risk:** low.

### E2. Roots — resolved by decision (see A8). No provider until a consumer exists.

### E3. Pluggable ingress `PolicyEngine` + audit verbosity
- **What:** `macp_runtime::policy_engine::PolicyEngine` — an async,
  identity-aware trait with three hooks (`evaluate_session_start`,
  `evaluate_message`, `evaluate_session_access`), injectable via
  `MacpServer::with_policy_engine`, deny-on-error, denials surfacing as
  protocol-correct `POLICY_DENIED` acks (writes) / `PermissionDenied`
  (reads).
- **The architectural boundary (the most important design point):** this is
  deliberately a **second, separate** trait from `PolicyEvaluator`:
  - `PolicyEvaluator` = commitment-time governance, MUST be a pure
    deterministic function of bound rules + accepted history (RFC-0012
    §6.3) — it replays.
  - `PolicyEngine` = ingress gating. Rejected traffic never enters accepted
    history, so replay only ever sees engine-approved envelopes — an async,
    non-deterministic external engine (OPA/Cedar) at ingress **cannot
    diverge replay**, by the same reasoning that keeps authentication
    outside the replay boundary. Collapsing the two traits would have
    broken RFC-0012 §6.3; keeping them separate is the load-bearing
    decision.
  - Corollary a reviewer should note: hooks run at the *server* (where
    authenticated identity exists). A library consumer driving `Runtime`
    directly bypasses them — same as auth itself; documented in the module.
- **Also:** clippy's dead-code warning exposed that `MacpServer` lived in the
  binary, making the injection API unreachable by any embedder — the server
  module was promoted into the library (`macp_runtime::server`), which is an
  API surface expansion worth a reviewer's glance.
- **Audit verbosity:** a bound policy may request info-level per-message
  audit lines via a `rules.audit.level` block; mode rule schemas ignore
  unknown blocks, so it composes with any mode.
- **Verified:** a deny-one-sender engine double proves all three hooks fire
  with the right error surfaces; audit-level unit test.
- **Risk:** medium-low. `evaluate_message` fetches the session per message
  when an engine is installed (one extra registry read — engines cost what
  they cost; zero overhead when absent). The audit block is runtime-specific
  rules vocabulary (not RFC-defined) — harmless to other implementations,
  but should eventually be proposed upstream.

### E5. Consistency cleanups
- **What:** the five near-identical ~30-line commitment-policy epilogues
  collapsed into `util::enforce_commitment_policy` (fail-closed by
  construction, single place to audit); shared mode-state codec helpers;
  `extract_commitment_rules` deduplicated to the single core implementation;
  dead `HandoffContext` authorize arm removed; **eval-time rule-parse
  failures now DENY loudly** — previously a policy whose rules failed to
  parse at evaluation time silently evaluated *empty default rules*, i.e. a
  corrupted policy imposed no constraints at all (registration validates, so
  the path should be unreachable — which is exactly why reaching it must be
  loud, not silent).
- **Documented residuals (deliberate, not omissions):** per-mode
  SessionStart participant-count differences need per-RFC verification
  before normalizing (Task/Handoff require ≥2 + initiator∈participants; the
  RFCs may genuinely differ per participant model); the registry
  file-persistence machinery was kept (it was rewritten correctly during D2
  rather than deleted); per-mode `encode_state` wrappers may delegate to the
  shared codec as later polish.
- **Verified:** 219 mode tests + 102 policy tests green after extraction.

### E6. Transcript visualizer (+ upstream half)
- **What:** `macp-transcript-viz` — conformance fixture JSON or session-log
  JSONL → Mermaid `sequenceDiagram`; rejected fixture messages render as
  `--x` arrows, internal entries as runtime notes; corrupt log lines skipped
  (matching runtime behavior). buf.build schema publishing is upstream (the
  spec repo owns the protos).
- **Verified:** acceptance test renders all 13 fixtures with structural
  checks (arrow count = messages+1, every referenced participant declared).
- **Risk:** none (dev tooling, no runtime coupling).

---

## Cross-cutting review notes

**Replay-compatibility discipline (the highest-stakes invariant).** Three
changes altered acceptance-relevant semantics; all three follow the same
migration rule: new semantics gate on a value *recorded at acceptance time*
(`LogEntry.bound_mode_version`, `LogEntry.semantics_rev`,
`compacted_incoming_ordinals`), all serde-defaulted so legacy logs
deserialize to legacy behavior, each with a legacy-fixture test. No change
re-derives anything from live registries during replay. This is the pattern
to hold future changes to.

**Wire-behavior changes to flag for dependents** (all are conformance fixes
or security hardening, all in CHANGELOG): B2 sequence semantics
(`macp-control-plane` should re-verify), B4 HS256 default (JWT deployments),
B5+C4 dev-mode opt-in (local-dev flows), A5 ext-descriptor strictness
(dynamic mode registrants), A8 roots capability.

**Deviations from the written plan (improvements, disclosed):** B6's
rate-limiter fix went further than the planned comment-matching cap (the cap
still scanned; the amortized sweep bounds the scan); B1's `WatchSessions`
dedupe key is `session_id`-on-`Created` rather than B2's ordinal (different
stream, different natural key — noted in the plan text as expected).

**What is explicitly NOT claimed:** cross-implementation replay equality
(blocked on upstream `MAX_SUSPEND_MS` — `rfc-changes.md` item 1); the RFC-0012
§4.5 handoff *timer* (A6 is the interim trust fix; the timer contract is
underspecified upstream); Redis power-loss durability (disclosed, not fixed);
multi-node/HA (single-writer by design, now documented).

## Adversarial verification of this document

After this document was written, an independent adversarial review pass
re-verified the highest-stakes claims against the committed code — reading
implementations, not re-running tests, explicitly hunting for bugs the test
suite misses. Result: **the architecture claims held; four real defects were
found in the gaps between them. All four are fixed (commit following
`5d9fb5e`), each with a regression test. 532 tests, clippy 0 after fixes.**

### Confirmed clean (no findings)
- **D2 core locking**: per-session mutex genuinely held across
  validate→append→commit→save; rollback poisons-before-removal on every
  pre-commit failure path; all 11 map-lock sites audited — none awaits a
  session mutex or storage under the map guard; no lock-free session
  mutation anywhere.
- **B2 ordinals**: 1-based, exclusive-after, internal entries never consume
  ordinals; compaction base accumulates correctly across repeated
  compactions and survives restart.
- **A5 replay binding**: recorded value only, registry never consulted, on
  both the full-replay and checkpoint paths.
- **B4**: no blocking bridge remains; stale JWKS bounded at TTL+grace;
  algorithm pinning correct.
- **B5/E1 startup ordering**: policies before recovery; auth/TLS gates
  before serve.

### Defects found and fixed

1. **E3 — StreamSession bypassed the policy engine (security).** The
   ingress engine gated only unary `Send`/`GetSession`; envelope frames and
   SessionStarts sent over the bidirectional stream, and passive-subscribe
   history replay, ran with no engine consultation — a denied sender could
   simply switch transports. *Fix:* `enforce_ingress_policy` on the stream
   envelope path (after built-in security, before kernel acceptance) and
   `evaluate_session_access` on the subscribe frame. *Test:*
   `policy_engine_gates_stream_path` (denied envelope frame →
   FAILED_PRECONDITION/PolicyDenied; denied subscribe → PermissionDenied).

2. **A6 — the forgery survived via the offer side (security).**
   `offered_at_ms` still recorded the unvalidated client envelope timestamp;
   an offering participant could BACK-date the offer and immediately commit
   — elapsed time appeared past the implicit-accept timeout, finalizing a
   handoff the target never saw. Same attack A6 closed, relocated from
   commitment post-dating to offer back-dating. *Fix:* rev ≥ 1 sessions
   record the runtime acceptance clock as `offered_at_ms` (log-recorded, so
   replay is unchanged); legacy sessions keep legacy behavior. *Test:*
   `implicit_accept_ignores_backdated_offer_timestamp_on_rev1`.

3. **B2 — the subscribe-window dedupe had two holes (correctness).**
   (a) The dedupe filter was consulted only in the live-event select arm;
   the three post-request drain loops — including the one that runs right
   after the subscribe frame itself, exactly where window duplicates sit —
   yielded buffered envelopes unfiltered. *Fix:* all yield paths route
   through one shared `should_skip_replayed`. (b) The FIFO disarm argument
   assumed publish order = acceptance order, but `process_session_start`
   published *after* dropping the session mutex, allowing a later message's
   broadcast to precede the SessionStart's. *Fix:* publish while holding the
   mutex, matching `process_message`. (The reviewer specifically re-derived
   the FIFO argument under the new per-session locking — the ordering
   premise had changed when the global lock was removed.)

4. **D2 — SessionStart rollback after the commit point was incoherent
   (durability).** A snapshot failure *after* the durable log append
   triggered a rollback that could not un-append the entry: the client saw
   `StorageFailed`, but the session resurrected on the next restart, and a
   same-id client retry appended a second SessionStart that made the log
   unreplayable (fatal under `MACP_STRICT_RECOVERY`). *Fix:* past the
   commit point, snapshot failure is a warning, not a failure — the same
   "log is authoritative, snapshots are best-effort" doctrine the
   non-start path already followed. Rollback now exists only on the two
   pre-commit failure paths, where it is coherent. *Test:*
   `session_start_snapshot_failure_is_nonfatal_after_commit_point`.

### Advisory notes (accepted, not fixed)
- The `max_open_sessions` count iterates all sessions under the map write
  lock with `try_lock`, counting any in-flight (locked) session as open
  **for the requesting sender** — under heavy fsync load, one sender's
  in-flight sessions can transiently consume another's budget. Conservative
  direction for a rate limit; acceptable bias, now documented here.
- JWKS refresh has no single-flight (thundering herd on TTL expiry) and
  builds a client per refresh — availability polish, queued as follow-up.
- A duplicate-SessionStart ack observed during a failed start's rollback
  window can report `duplicate=true` with a non-Open state — cosmetic,
  self-corrects on retry.

### Post-review outcomes (addendum, 2026-07-05)

Dispositions of the advisory notes after the change set merged and v0.5.0
shipped: the **JWKS single-flight + client reuse** advisory was implemented
(refresh mutex + cache re-check, one fetch under 8-way concurrency, proven
by test) and released; the **`max_open_sessions` conservative-count bias**
remains accepted as documented; the **duplicate-SessionStart ack** cosmetic
is recorded in `plans/defer/follow_ons.md`. Of the "explicitly NOT claimed"
items, `MAX_SUSPEND_MS` session binding has since shipped (spec PR #46 +
runtime, v0.5.0) and the RFC-0012 §4.5 handoff timer is now fully specified
upstream (RFC-0010 §5.1) with the runtime implementation queued in
`follow_ons.md`; Redis power-loss durability and multi-node/HA remain
disclosed non-goals.

### What this verification cycle demonstrates
The document's per-task claims survived adversarial reading; the defects
lived in *interactions between* independently-correct changes (E3's hooks vs
the second transport; A6's clock vs the other timestamp field; B2's FIFO
premise vs D2's new lock scope; D2's rollback vs B-phase durability
doctrine). That is the strongest argument for keeping this two-layer review
practice — per-task verification plus a cross-cutting adversarial pass —
for future change sets of this size.
