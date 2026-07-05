# Build Status

**Started:** 2026-07-04 · **Principles:** Go slow, go deep, do right, build for the
long term, no shortcuts. Every change lands with its tests; no task is marked DONE
without a green build + targeted tests; behavior changes get regression tests and,
where replay is affected, a pre-fix log fixture.

**Sources:** `plans/IMPROVEMENT_PLAN.md` (master, evidence/rationale) ·
`plans/current/*.md` (execution plans).

Legend: `TODO` · `IN PROGRESS` · `BLOCKED (reason)` · `DONE (verification)` · `DEFERRED`

---

## Baseline

| Check | Status |
|---|---|
| `cargo build` (workspace) | DONE (green, 2026-07-04) |
| `cargo test --workspace` | DONE (green at baseline; 507 passed post-A4/A5) |
| Baseline commit | `732b689` (main, clean tree at session start) |

---

## Phase A — Pre-freeze (plans/current/phase-a-prefreeze.md)

| Task | Status | Notes |
|---|---|---|
| A1. `Session` builder + `#[non_exhaustive]` public types | DONE (510 tests green, clippy clean) | `Session::builder` added; `Session`, `MacpError`, `ModeResponse`, `PolicyDecision`, `PolicyError` marked `#[non_exhaustive]`; all 19 construction sites migrated (2 production: `runtime.rs`, `replay.rs`; 1 conversion: storage `PersistedSession→Session`; 16 fixtures). Bonus hardening: the five mode commitment checks inverted from fail-open `if let Deny` to fail-closed `match` (unknown policy decision ⇒ deny). |
| A2. `CommitmentContext` evaluator unification (+ §1.10 quorum fix) | DONE (516 tests green; tier-1 in closing gate) | Trait collapsed to one required `evaluate_commitment(&CommitmentContext)`; old six methods are `#[deprecated]` defaulted shims. All five modes now outcome-aware. Quorum evaluator's non-conformant participation reading of `threshold` removed — threshold is the RFC-0012 §4.2 approval bar (mirrors the mode); declines not gated. Task `require_output` and Proposal `max_rounds` no longer deny negative outcomes. |
| A3. `policy.default` echo contract (permissive empty match) | DONE (3 new tests) | Empty `commitment.policy_version` now defers to the session's bound policy; non-empty must match exactly (`mode/util.rs`). Tier-1 case pending with the tier-1 batch. |
| A4. Session-ID validation fix (36-char base64url with `-`) | DONE (2 new tests; UUID-parseable strings keep strict rules — uppercase UUIDs still rejected) | `macp-core/src/session.rs` |
| A5. Ext-mode holes | DONE (all three sub-items, 7 new tests) | (1) promote-to-`macp.mode.*` rejected, promotion validated; (2) descriptors must declare ≥1 terminal type, and only `Commitment` (passthrough-backed); (3) empty ext `mode_version` now binds descriptor version, recorded as `LogEntry.bound_mode_version` (serde-default `None`), replay uses recorded binding never live registry, legacy logs keep legacy vacuous semantics. Updated tier-1 fixture descriptor. |
| A6. Handoff implicit-accept interim fix | DONE (519 tests green; 4 new tests) | `MessageContext` + defaulted `Mode::on_message_at` kernel entry (zero churn on ~330 test call sites); handoff times implicit-accept against the acceptance clock on rev≥1 sessions; `Session.semantics_rev` + `CURRENT_SEMANTICS_REV=1` recorded on SessionStart log entry and in snapshots; legacy histories (rev 0 via serde default) replay under the envelope clock. |
| A7. Multi-round proto payloads | DONE (2026-07-05) — `macp-proto` 0.1.4 released; branch verified against the published crate, ready to merge | Spec side: PR #45 (canonical proto, package sync, all three hardcoded codegen lists), closes issue #39. Runtime side on `feat/a7-multi-round-proto`: macp-pb codegen + `multi_round_pb` module, mode accepts proto with permanent JSON-first parse order (deliberate deviation from the plan's "proto-first" — see work log), conformance loader + example client emit proto, 3 new regression tests. Verified end-to-end with a temporary path dep; branch commits against `macp-proto = "0.1.4"`. |
| A8. Roots capability decision | DONE (tier-1 test added) | Decision: disclaim honestly. `Initialize` now advertises `roots{list_roots:true, list_changed:false}` — ListRoots truthfully answers (empty set), but no change notifications are promised since no roots provider exists (RFC-0006 §3.3). Revisit at E2. |

## Phase B — Security & correctness (phase-b-security-correctness.md)

| Task | Status | Notes |
|---|---|---|
| B1. Lag surfacing + WatchSessions sync race | DONE (tier-1 gate green) | `watch_signals`/`watch_sessions` surface `Lagged` as `ResourceExhausted` (mirrors StreamSession); `watch_sessions` dedupes buffered `Created` events against the initial-sync set (IDs are create-once). Tier-1 tests added: exactly-once Created (before+after subscribe). |
| B2. Passive-subscribe sequence contract (+ compaction coupling) | DONE (521 ws tests; final gate green) | Sequence = 1-based accepted-envelope ordinal, `after_sequence` exclusive (0 = from start) per RFC-0006 §3.2 — fixes both the raw-combined-index defect and the inclusive off-by-one; subscribe-window duplicates dropped via replay message-id dedupe that disarms on first miss (FIFO); compaction checkpoints record `compacted_incoming_ordinals` so ordinals stay stable across compaction+restart, in-memory log_store now updated with disk, and resume below the compacted base returns `failed_precondition` instead of silently skipping; CLAUDE.md env table corrected. |
| B3. Backend durability parity | DONE (feature-clippy 0, 520 ws tests + 23 feature tests green) | RocksDB log appends fsync the WAL (`WriteOptions::set_sync(true)`) — acked implies durable, matching the file backend; Redis `replace_log` is atomic (MULTI/EXEC) and the backend logs a durability disclosure at connect; corrupt-entry parity: rocksdb/redis now skip+warn per entry like the file backend (new regression test); file `atomic_write` fsyncs the tmp file before rename + parent dir after. Deferred pieces: persistent file handles (needs D1 benchmarks), Redis integration tests (need C1's CI service container). |
| B4. JWT/JWKS hardening | DONE (final gate green; tier1_jwt 8/8 incl. new HS256-default-rejection test) | (1) HS256 out of the default allowlist — RS256/ES256 default, explicit `MACP_AUTH_JWT_ALGS` opt-in for HS256; (2) JWKS fetch has connect/total timeouts (3s/5s); (3) stale-cache fallback (1h grace) — endpoint outage no longer kills all JWT auth at TTL expiry; (4) `kid`-based key selection with try-all fallback; (5) `authenticate_metadata` fully async — `block_in_place`/`block_on` bridge removed (16 server call sites + tests awaited). |
| B5. Dev-mode fallback gate | DONE (tier-1 gate green) | Startup refuses to run with no configured auth unless `MACP_ALLOW_INSECURE=1`; explicit warning when opted in; false 'test-only' comment on `dev_authenticate` replaced with the truth; new tier-1 `startup_refuses_without_auth_or_insecure_flag`. C4's Dockerfile change (drop baked-in `MACP_ALLOW_INSECURE=1`) still pending — ships together as the compound break. |
| B6. WatchSignals auth + rate-limiter prune cap | DONE (tier-1 gate green) | `watch_signals` now authenticates (tier-1 test: bare call → Unauthenticated; existing broadcast test updated to authenticate). Rate limiter: per-request O(all-senders) scan replaced with amortized full sweep every 128 requests (counter in `RateBucket`) — map provably bounded (`rate_bucket_sweep_removes_stale_senders`). The false '100 entries' comment is gone with the code it described. |

## Phase C — CI foundation (phase-c-ci-foundation.md)

| Task | Status | Notes |
|---|---|---|
| C1. Feature-flag matrix | DONE (yaml valid) | New `features` CI job: clippy+tests for rocksdb/redis features, redis:7 service container with health checks, `MACP_TEST_REDIS_URL` set (redis tests now RUN in CI). |
| C2. Stable toolchain + MSRV gate | DONE | Main jobs on stable; `check` job pinned 1.89 as the MSRV gate; `audit` advisory (`continue-on-error`, out of `ci-pass`). |
| C3. Tier-1 integration tests gate PRs | DONE | New `integration-tier1` job (tier1 + tier1_jwt, serial) in the required `ci-pass` gate. |
| C4. Docker + docs hygiene | DONE | Dockerfile hardened (no baked-in dev mode, no tests COPY); `temp/` deleted; CHANGELOG/SECURITY/CONTRIBUTING added; deployment.md: real-Dockerfile guidance with explicit dev-mode opt-in, backend durability matrix (file/rocksdb=durable, redis=not power-loss-safe + single-writer), observation-surface authorization scoping documented (closes the master §1.1 'record the decision' item). |
| C5. Conformance groundwork | DONE (14 conformance tests green) | All 13 fixtures + loader atomically renamed to canonical proto names (`macp.modes.<mode>.v1.<Type>Payload`, `macp.v1.CommitmentPayload`); `tests/conformance/schema.json` (draft-2020-12, incl. Cancelled/Suspended states + already-implemented optional fields + inline `policy`); dependency-free format-guard test prevents drift back to shorthand. |

## Phase D — Hardening (phase-d-hardening.md)

| Task | Status |
|---|---|
| D1. Recovery/throughput benchmarks | DONE (criterion `benches/replay_bench.rs`) |
| D2. Per-session locking | DONE (gate green: clippy 0, tier1 71/71, jwt 8/8) |
| D3. Memory bounds | DONE (gate green) | Eviction clears registry + log cache (`remove_session_log`) + stream channel (`remove_if_unused`, receiver-safe); Cancelled sessions now evictable |
| D4. Server limits + graceful shutdown | DONE (2 tier-1 tests) | tonic concurrency/stream/timeout/keepalive limits (env-tunable); `max_decoding_message_size` tracks `MACP_MAX_PAYLOAD_BYTES` (was silently capped at tonic's 4MB before the payload check); `serve_with_shutdown` + hard drain deadline (`MACP_SHUTDOWN_DRAIN_SECS`, watch streams can't hold shutdown open); SIGINT-drain test green |
| D5. Metrics export | DONE (tier-1 endpoint test) | Rejection counters wired at the Send error path (were zero-callers); suspended/resumed added to `MetricsSnapshot`; dependency-free Prometheus text endpoint (`MACP_METRICS_ADDR`, opt-in) incl. `macp_replay_mismatches_total` |
| D6. On-disk retention/GC | DONE (regression test) | `gc_disk_sessions` — `storage.delete_session`'s first caller ever; enumerates storage (works for evicted sessions), terminal+past-retention only, clears registry/log-cache/stream-bus remnants; opt-in `MACP_SESSION_DISK_RETENTION_SECS` (0=keep forever) |
| D7. Replay consistency validation | DONE (unit test) | Warn-only `validate_replay_consistency` (state/dedup-count/participants/bound-versions) run in recovery BEFORE the snapshot is overwritten; `replay_mismatches` counter surfaced via the metrics endpoint |

## Phase E/F — Features & tooling (phase-e-features.md)

| Task | Status |
|---|---|
| E1. `MACP_POLICIES_DIR` | DONE (2 tier-1 tests) | `PolicyRegistry::load_from_dir` (deterministic order, full validation via the same register path); loaded BEFORE recovery so replayed sessions resolve file policies; RFC-0012 §9 profile: wire registry read-only (`register_policy:false` advertised, mutating RPCs → FAILED_PRECONDITION); broken dir fatal at startup |
| E2. Roots | DONE-BY-DECISION (A8) | Disclaimed honestly (`list_changed:false`); provider deferred until a consumer exists |
| E3. `PolicyEngine` trait + audit verbosity | DONE (3-hook test + unit test; 527 ws tests green) | `macp_runtime::policy_engine::PolicyEngine` — async identity-aware INGRESS gating (session start / message / session access), injectable via `MacpServer::with_policy_engine`, deny-on-error, denials surface as POLICY_DENIED. Determinism boundary documented: rejected traffic never enters history, so async engines can't diverge replay; commitment governance stays with the pure `PolicyEvaluator` (RFC-0012 §6.3). Audit: `rules.audit.level="info"` elevates per-message audit lines per bound policy. |
| E4. Conformance pack + CI oracle | LOCAL PART DONE (canonical format + schema, C5) · publishing + spec-repo CI oracle UPSTREAM (rfc-changes.md item 15) |
| E5. §3.5 consistency cleanups | DONE (participant validation per-RFC verified 2026-07-05) | Commitment epilogue extracted to `util::enforce_commitment_policy` (5 modes, ~150 lines removed); shared mode-state codec added; `extract_commitment_rules` deduped to the core impl; dead `HandoffContext` authorize branch removed; eval-time rule-parse failures now DENY loudly (were silently evaluating empty defaults). Participant validation verified against RFCs 0007–0011: Task's `initiator ∈ participants` check conflicted with RFC-0009 role-based authority — relaxed to "≥1 non-initiator assignee"; Handoff's strictness justified by the delegated model (RFC-0010 §2), now documented at the check; Decision/Proposal/Quorum conformant as-is. Remaining polish only: per-mode `encode_state` wrappers may delegate to the codec. |
| E6. Transcript visualizer + buf.build | Visualizer DONE (2 tests; renders all 13 fixtures + session logs) · buf.build UPSTREAM (spec repo owns protos) |

## RFC filings (rfc-changes.md)

| Item | Status |
|---|---|
| 1–6 blocking normative issues | DONE (filed 2026-07-05 as spec-repo issues #34–#39; 1–3 flagged freeze-blocking) |
| 7–15 corrections | DONE (filed 2026-07-05: 7–11 batched as doc-drift issue #40; 12→#41, 13→#42, 14→#43, 15→#44) |
| Spec PRs for 1, 3, 4 | TODO (per checklist: draft once maintainers ack direction) |

---

## Work log

- **2026-07-04** — BUILD_STATUS.md created; baseline build+test green.
- **2026-07-04** — **A4 done**: `validate_session_id_for_acceptance` no longer
  hard-routes 36-char strings containing `-` to the UUID branch; UUID-parseable
  strings keep strict rules (no fall-through), so uppercase/wrong-version UUIDs
  are still rejected. Tests: `base64url_36_chars_with_hyphen_accepted`,
  `uuid_shaped_but_wrong_version_does_not_fall_through`.
- **2026-07-04** — **A5 done** (master §1.8/§1.9):
  - `promote_mode` rejects `macp.mode.*` targets (reserved-namespace guard) and
    empty targets; failed promotion provably does not mutate the entry.
  - `validate_extension_descriptor` requires ≥1 terminal message type and only
    `Commitment` as terminal (passthrough-backed modes resolve on nothing else).
  - Version binding: SessionStart with empty `mode_version` on a non-strict ext
    mode now binds the registered descriptor's version; recorded as new
    `LogEntry.bound_mode_version` (`#[serde(default)]` → legacy logs deserialize
    as `None` and keep legacy empty-binding semantics on replay). Replay reads
    the recorded binding only — proven by a test whose live registry carries a
    different version. Commitment with `mode_version:""` on a bound ext session
    is now rejected (was vacuously accepted).
  - Updated: tier-1 `test_mode_registry.rs` descriptor (was registering a mode
    that could never resolve — exactly the defect class this closes).
  - Verification: workspace build + 507 tests green, fmt applied, clippy clean.
- **2026-07-04** — **A3 done** (master §2.3): empty `commitment.policy_version`
  matches the session's bound policy (client that started with "" is no longer
  forced to echo `policy.default`); non-empty values must match exactly. Chosen
  as the forward-compatible direction while the upstream echo ambiguity
  (rfc-changes.md item 3) is unresolved. 510 tests green, clippy clean.
  Tier-1 integration suite running against the rebuilt binary.
- **2026-07-04** — **A1 code complete** (master §2.1): `SessionBuilder` (three
  required args: session_id, mode, initiator_sender; documented defaults,
  `ttl_expiry: i64::MAX`), `#[non_exhaustive]` on `Session`, `MacpError`,
  `ModeResponse`, `PolicyDecision`, `PolicyError`. All construction sites
  migrated to the builder. **Fail-closed hardening**: all five modes previously
  used `if let PolicyDecision::Deny` — anything else (including any future
  variant) was silently treated as allow; now an explicit `match` where only
  `Allow` proceeds and unknown decisions deny with a logged reason.
  **Verified**: cargo fix applied, clippy 0 warnings, 510 tests green. A1 DONE.
- **2026-07-04** — **A2 started** (master §2.2): scoping agent producing the
  implementation dossier (trait surface, all call/test sites, per-mode state
  mapping, quorum §1.10 fix lines, decline-blocking checks in the four
  outcome-blind evaluators). Tier-1 integration suite still compiling in
  background — must be green on the final binary before Phase A closes.
- **2026-07-04** — **A2 implemented** (master §2.2 + §1.10): `PolicyEvaluator`
  collapsed to a single required `evaluate_commitment(&CommitmentContext)`;
  `CommitmentContext{policy, participants, outcome_positive, mode}` with
  `#[non_exhaustive] CommitmentMode` enum (Decision carries `&DecisionState`,
  others carry their existing scalars — zero state-type moves into core). Old
  six methods are `#[deprecated]` defaulted shims delegating old→new; the 96
  policy free-function tests untouched in signature. All five modes now build
  the context and pass `outcome_positive` (four previously discarded the
  payload). **Conformance fixes**: (1) quorum evaluator's participation
  reinterpretation of `threshold` removed — it is the RFC-0012 §4.2 approval
  bar, mirroring the mode's `effective_threshold` math (one test flipped
  Allow→Deny: 2 approvals vs threshold 3 was passing on "participation");
  (2) declines are no longer denied by outcome-blind checks: quorum threshold,
  task `require_output` (a TaskFail has no output), proposal `max_rounds`
  (the decline is the exit from an over-long negotiation). `DenyAllEvaluator`
  and `DefaultPolicyEvaluator` migrated. 6 new tests; 516 total green.
  Tier-1 gate running (fmt/clippy/tier-1 on final binary).
- **2026-07-04** — **A8 done**: roots capability now honest —
  `list_changed:false` in `Initialize` (`server.rs`); `ListRoots` keeps
  answering with the empty set (a valid state). New tier-1 test
  `initialize_advertises_honest_roots_capability`. Implementing a provider is
  E2, gated on an actual consumer appearing.
- **2026-07-04** — **A6 design settled** (implementation next): pass the
  runtime's acceptance timestamp into `Mode::on_message` (computed once, also
  used as the log entry's `received_at_ms` so live and replay agree); handoff
  implicit-accept switches from the initiator-forgeable `env.timestamp_unix_ms`
  to acceptance time. Replay gating via a session-level semantics marker
  recorded on the SessionStart log entry (same `#[serde(default)]` pattern as
  `bound_mode_version`): new sessions get new semantics, legacy histories
  replay under legacy semantics, pre-fix log fixture test required.
- **2026-07-04** — **A6 done** (master §2.5 interim fix): added
  `macp_core::mode::MessageContext` (`#[non_exhaustive]`, acceptance clock) and
  a defaulted `Mode::on_message_at` kernel entry point — modes that don't need
  a clock are untouched (no churn across ~330 existing `on_message` call
  sites). The runtime computes one acceptance timestamp per message, passes it
  to the mode AND records it as the log entry's `received_at_ms`; replay feeds
  the recorded value back, so live and replay observe the identical clock.
  Handoff's implicit-accept now times against that clock on rev>=1 sessions —
  the initiator-forgeable envelope timestamp no longer finalizes offers
  (proved by `implicit_accept_ignores_forged_envelope_timestamp_on_rev1`).
  Migration per the ground rule: `Session.semantics_rev`
  (`CURRENT_SEMANTICS_REV = 1`) recorded on the SessionStart log entry and in
  `PersistedSession` (serde defaults -> legacy loads as rev 0); rev-0 sessions
  keep the envelope clock (`implicit_accept_legacy_rev0_keeps_envelope_clock`,
  `replay_preserves_recorded_semantics_rev`). 519 tests green. RFC-0012 §4.5's
  runtime-timer contract remains upstream (rfc-changes.md item 2); this is the
  interim trust fix. Phase A closing gate (fmt/clippy/tests/tier-1) running.
- **2026-07-04** — Phase A closing gate: fmt applied, clippy 0 warnings,
  519 unit/integration tests green, release-path binary built. Tier-1 suite
  running (background cargo tasks get killed in this environment; rerunning
  attached). Phase A code work is complete: A1-A6, A8 done; A7 blocked on
  upstream `macp-proto` release.
- **2026-07-04** — **B1 + B6 implemented** (master §1.1/§1.3/§1.11): lag on
  `WatchSignals`/`WatchSessions` now terminates the stream with
  `ResourceExhausted` instead of silent `Ok` close; `WatchSessions`
  subscribe-before-snapshot duplicate race fixed by deduping buffered
  `Created` events against the synced set (session IDs are create-once, so
  suppressing any repeat `Created` is safe — non-Created events pass through);
  `WatchSignals` requires authentication (ambient payloads are agent data;
  RFC-0004 §4.1 note: the spec constrains producers — subscriber auth is our
  hardening posture, documented as such). Rate limiter: the per-request
  full-map stale scan (O(attacker-controllable sender cardinality)) replaced
  by an amortized full sweep every 128 requests; between sweeps a request
  touches only its own deque. New tests: sweep-bounding unit test, tier-1
  unauthenticated-WatchSignals, tier-1 watch_sessions exactly-once-Created.
  Note: WatchModeRegistry/WatchRoots stay unauthenticated as discovery
  surfaces (deliberate; recorded here per master §1.1) — the ListSessions/
  WatchSessions all-sessions visibility asymmetry remains a documented
  decision for C4's deployment.md update.
- **2026-07-04** — **B3 done** (master §1.5): RocksDB COMMIT-POINT appends now
  sync the WAL before acking (crash cannot lose acknowledged messages;
  snapshots stay async — log is source of truth); Redis `replace_log`
  DEL+RPUSH* now one MULTI/EXEC transaction (was silently truncatable
  mid-sequence) and connect logs a durability disclosure (no WAIT/AOF
  barrier; single-writer only); corrupt-entry handling unified across
  backends (skip+warn per entry — one bad record no longer drops a whole
  session on rocksdb/redis; `rocksdb_load_log_skips_corrupt_entry`); file
  backend `atomic_write` is now genuinely crash-atomic (tmp fsync before
  rename, parent-dir fsync after). A1 fallout in feature-gated code fixed:
  the rocksdb/redis test fixtures (never compiled by default CI — exactly
  the C1 gap) migrated to `Session::builder`. Feature-enabled clippy: 0.
- **2026-07-04** — **B5 implemented** (master §1.7): `SecurityLayer::has_configured_auth()`
  + startup gate in `main.rs` — no tokens/issuer configured now refuses to
  start with an actionable error unless `MACP_ALLOW_INSECURE=1` (the same
  flag that gates plaintext keeps local dev a one-variable flow); when opted
  in, a warning states that ANY bearer token is fully privileged. Integration
  harness already sets the flag (verified before changing). New tier-1 test
  spawns the binary with a scrubbed env and asserts non-zero exit + message.
  Combined tier-1 gate (Phase A + B1/B3/B5/B6 + 4 new tier-1 tests) running.
- **2026-07-04** — **B4 done** (master §1.6): all five JWT/JWKS items.
  Coordination note for `auth-service` (RS256): unaffected by the HS256
  default removal; release notes must mention `MACP_AUTH_JWT_ALGS` for any
  shared-secret deployments. `tier1_jwt` suite validates end-to-end in the
  final gate. Remaining Phase B: B2 (passive-subscribe sequence contract +
  compaction coupling) — the last and largest item, designed as one unit.
- **2026-07-04** — **Tier-1 gate GREEN**: 71 passed / 0 failed, including all
  four new tests (startup gate, WatchSignals auth, watch_sessions
  exactly-once-Created, honest roots capability). This gate covers Phase A
  (A1-A6, A8) and Phase B B1/B3/B5/B6 end-to-end through the real gRPC
  boundary. `tier1_jwt` suite running for B4's end-to-end validation.
  **Phase A closed** (A7 upstream-blocked). Starting B2 — passive-subscribe
  sequence contract (ordinal + exclusive-after + dedupe-on-drain + compaction
  coupling), the last Phase B item.
- **2026-07-04** — **B2 code complete** (master §1.2+§1.4, one design):
  ordinal contract in `log_store::get_incoming_after` (1-based accepted
  ordinal, exclusive-after, internal/checkpoint entries never consume
  ordinals — contiguous client-visible sequence); `Err(base)` when the range
  was compacted away -> `failed_precondition` at the transport; stream loop
  dedupes the subscribe-window broadcast buffer against replayed message_ids
  (drops-on-hit, disarms on first miss — safe because the receiver is FIFO
  and all in-window events precede post-snapshot events); compaction now
  records discarded ordinal count on the checkpoint (accumulating across
  repeated compactions), returns the checkpoint, and the runtime replaces the
  in-memory log alongside disk. 2 new log_store tests (ordinals + compaction
  base + below-base error). CLAUDE.md: MACP_CHECKPOINT_INTERVAL note fixed,
  MACP_AUTH_JWT_ALGS documented. Workspace verification + tier-1 rerun pending.
- **2026-07-04** — **PHASE B COMPLETE, final gate GREEN**: tier1 71/71,
  tier1_jwt 8/8. The gate caught one intended breaking change in flight — the
  JWT harness signs HS256, which the hardened default allowlist now rejects;
  harness opts in via `MACP_AUTH_JWT_ALGS=HS256` (exercising the knob) and a
  NEW test pins the security property (HS256 rejected without opt-in).
- **2026-07-04** — **Phase C: C1-C3 done, C4 mostly done**: CI now runs
  stable + MSRV check; feature-gated backends built/linted/tested with a live
  Redis service; tier-1 suite gates PRs; audit advisory-only. Dockerfile no
  longer bakes in dev mode; temp/ deleted; CHANGELOG/SECURITY/CONTRIBUTING
  added. Remaining: deployment.md updates (C4), conformance canonical names +
  schema (C5).
- **2026-07-04** — **C4 done**: deployment.md now carries the durability
  matrix, the single-writer warning, the dev-mode opt-in docs (compound break
  with B5 shipped together as planned), and the ListSessions/WatchSessions
  visibility decision in writing. Remaining Phase C: C5 (conformance
  canonical payload_type names + fixture schema.json). CHANGELOG.md seeded
  with every user-visible change from Phases A+B.
- **2026-07-04** — **C5 done — PHASE C COMPLETE**: canonical conformance
  format landed atomically (fixtures + loader + schema + format guard). The
  fixture format now matches what the spec repo's conformance pack expects
  (master §5.7 steps 1-2); steps 3-4 (publishing + spec-repo CI oracle) are
  Phase E work needing upstream coordination. **Starting Phase D** with D1
  (recovery/throughput benchmarks — the baseline for D2's locking rework).
- **2026-07-04** — **D1 done**: criterion baselines (quick mode, this machine):
  replay_from_start 100/1k/10k entries = 83µs / 810µs / 8.1ms (linear — no
  superlinear replay pathology); send single-session 1.68µs vs across-8
  1.62µs on MemoryBackend (lock invisible when storage is free); **the D2
  target number**: 8 concurrent sends to 8 different sessions on the
  fsyncing FileBackend = **95ms** (~12ms/fsync, fully serialized by the
  global registry write lock). Per-session locking should approach ~12-15ms.
  Bench notes TTL nuance (live benches need wall-clock timestamps).
- **2026-07-04** — **D2 + D3 implemented** (master §3.1/§3.2):
  `SessionRegistry` stores `Arc<Mutex<Session>>` (`SharedSession`); map lock
  only for lookup/insert/remove with a documented lock-ordering rule;
  per-session mutex held across validate+append+commit. SessionStart uses
  reserve-and-rollback (atomic dedup+max_open under brief map write; storage
  I/O unlocked; rollback poisons the placeholder non-Open before removal so
  racing waiters fail the OPEN gate — never append to an uncommitted
  session). All lifecycle paths + TTL sweep converted (sweep expires each
  session under its own mutex). Eviction clears all three unbounded maps.
  **523 tests green** incl. TOCTOU/dedup/concurrency invariants.
  **Bench**: 8 concurrent fsync-backed cross-session sends 95ms → 60ms
  (-38%). Remaining ceiling is device fsync bandwidth (2 fsyncs/message:
  durable append + snapshot), not lock architecture — snapshot
  debounce/latest-wins noted as a possible future optimization (log is
  authoritative; snapshots best-effort). Gate (fmt/clippy/tier-1) running.
- **2026-07-04** — **D4-D7 implemented**: server limits + graceful shutdown
  with hard drain deadline (SIGINT test: clean exit <1s); ingress bound now
  tracks MACP_MAX_PAYLOAD_BYTES (tonic 4MB default previously applied first);
  metrics finally exportable (opt-in Prometheus text endpoint, zero deps) with
  rejection counters recorded; disk GC gives delete_session its first caller
  (bounded disk + restart memory floor); replay/snapshot divergence checked
  at recovery, warn-only per the promoted defer plan's own mitigation. Env
  vars documented in CLAUDE.md. Full verification cycle running.
- **2026-07-04** — **PHASE D COMPLETE, gate GREEN** (tier1 73/73, jwt 8/8):
  all seven tasks done — benchmarks, per-session locking (-38% on the
  fsync-contended path, ceiling now hardware not architecture), memory bounds
  (three unbounded maps now evicted), server limits + graceful drain, metrics
  export, disk GC, replay validation. Starting Phase E with E1
  (MACP_POLICIES_DIR + RFC-0012 §9 read-only registry profile).
- **2026-07-04** — **E1 done** (2 tier-1 tests: file-loaded policy visible +
  read-only wire registry + honest capability; invalid dir fatal). **E5
  mostly done**: 5-mode commitment epilogue extraction (shared fail-closed
  gate), codec helpers, rules-extraction dedupe, dead branch removal, loud
  eval-time rule-parse denial (102 policy tests green, 219 mode tests green).
  E2 resolved by A8's disclaim decision. Full verification + tier-1 gate next.
- **2026-07-04** — **E3 done** (527 tests green, clippy 0): pluggable ingress
  `PolicyEngine` with the OPA/Cedar integration shape from the promoted defer
  plan, hooked at all three points (proven by a deny-one-sender double:
  POLICY_DENIED acks on start+message, PermissionDenied on read); policy-
  driven audit verbosity via an `audit` rules block (composes with any mode's
  schema — unknown blocks are ignored at validation). E1/E5 tier-1 gate was
  green (75/75 + 8/8) before E3 started; final Phase E gate next.
- **2026-07-04** — Maintenance: build cache had grown to ~70GB across the
  session (disk hit 100%); cleaned target dirs, 32GB free. `MacpServer`
  promoted from the binary into the library (`macp_runtime::server`) so
  embedders can actually reach `with_policy_engine` — the E3 injection API
  was otherwise unreachable outside this repo (clippy's dead-code warning
  caught a real API-placement mistake). Cold-rebuild verification running;
  the ~27 post-promotion clippy warnings turned out to be artifacts of the
  disk-full partial build — after the clean rebuild: 527 tests green,
  clippy 0. Phase E tier-1 gate running (cold rebuild).
- **2026-07-04** — **E6 visualizer done** (`macp-transcript-viz`: fixture JSON
  or session-log JSONL -> Mermaid sequenceDiagram; acceptance test renders all
  13 conformance fixtures with structural checks; corrupt log lines skipped
  like the runtime). Phase E tier-1 gate was green (75/75 + 8/8).
  **ALL LOCAL WORK ACROSS PHASES A-E IS NOW COMPLETE**: 529 workspace tests,
  clippy 0, every phase gated end-to-end. Remaining items require action in
  the spec repo / upstream: rfc-changes.md filings (items 1-15), A7
  multi-round proto (needs macp-proto release), E4 publishing + CI oracle,
  E6 buf.build. E5 residuals (participant-validation normalization,
  per-mode codec delegation) are documented polish, not defects.
- **2026-07-04** — **Adversarial implementation review of the full change
  set** (docs/change-review-phases-a-e.md): architecture claims CONFIRMED
  (D2 locking discipline, B2 ordinals, A5 replay binding, B4 async auth,
  startup ordering). **Four real defects found in the gaps and FIXED with
  regression tests**: (1) E3 engine bypass via StreamSession (envelope +
  subscribe paths now gated); (2) A6 forgery relocated to offer back-dating
  (offered_at_ms now acceptance clock on rev>=1); (3) B2 dedupe bypassed by
  drain loops + SessionStart publish outside the mutex broke the FIFO
  premise (shared should_skip_replayed on all yield paths; publish under
  the mutex); (4) D2 rollback after the commit point resurrected failed
  sessions / made retry logs unreplayable (post-commit snapshot failure is
  now non-fatal, matching the log-authoritative doctrine). 532 tests,
  clippy 0. Tier-1 gate rerunning.
- **2026-07-05** — **Post-review gate CONFIRMED GREEN on `422085b`**: workspace
  tests pass (exit 0), tier-1 75/75, tier1_jwt 8/8, tier-2 Rig tools 5/5
  (tier-3 ignored as designed). The adversarial-review fix commit is fully
  gated. Also verified: `plans/` IS git-tracked — the master plan's
  version-control warning (gitignored, sole-copy risk) is stale/resolved.
- **2026-07-05** — **RFC filings DONE** (rfc-changes.md items 1–15): filed as
  11 GitHub issues on the spec repo
  (multiagentcoordinationprotocol/multiagentcoordinationprotocol):
  #34 MAX_SUSPEND_MS binding, #35 handoff timer contract, #36 policy_version
  echo (all three flagged freeze-blocking), #37 quorum threshold schema
  defects, #38 ListSessions pagination, #39 multi-round proto + macp-proto
  release, #40 doc-drift batch (items 7–11), #41 intent undocumented,
  #42 vote cardinality, #43 passive-subscribe sequence definition (the B2
  contract proposed upstream), #44 conformance fixture ownership + schema.
  Spec PRs for 1/3/4 remain per the checklist (await maintainer ack).
- **2026-07-05** — **A7 code complete** (master §4.5). Spec repo: canonical
  `macp/modes/multi_round/v1/multi_round.proto` (`ContributePayload{string
  value=1}` — string over bytes/structured: equality-compared opaque values,
  extensible via new fields) + entrypoint proto + the three hardcoded
  codegen lists (Makefile, ci.yml, publish workflow) + raw-package sync +
  python scaffolding; buf lint / proto compile / sync check green; PR #45.
  Runtime: `multi_round_pb` generated in macp-pb, re-exported; mode accepts
  proto and legacy JSON with **JSON-first parse order, permanently** — a
  deliberate deviation from the plan's "protobuf first": pathological legacy
  JSON bytes CAN decode as valid proto (e.g. `{` opens a group a later `|`
  closes) which would silently change a replayed contribution, while a proto
  payload can never parse as a JSON object — JSON-first is the only
  replay-safe order (RFC-0003 §1). Empty payloads stay rejected (proto3
  can't encode value="" non-emptily). Conformance loader + client emit
  proto; tests: json_fallback_still_accepted, proto/json equivalence
  (no round advance on same value), empty rejected. Verified with temporary
  path dep on the spec repo; committed on `feat/a7-multi-round-proto`
  against `macp-proto = "0.1.4"` — the branch builds the moment the release
  lands (tag `proto-v0.1.4` after PR #45 merges).
- **2026-07-05** — **E5 participant-validation residual closed** (per-RFC
  verification, all six modes): no RFC specifies a numeric participant
  minimum — all count checks are runtime policy. Findings: Decision/
  Proposal/Quorum conformant (non-empty only; RFC-0007 §2 and RFC-0011 §2
  make initiator authority role-based, NOT membership-based — quorum
  explicitly separates coordinator from voter pool). **Task's
  `initiator ∈ participants` check contradicted RFC-0009** (§2/§4 authorize
  TaskRequest by initiator role; a pure external orchestrator was wrongly
  rejected) — relaxed to "≥1 participant other than the initiator" (the
  orchestrated model's real invariant: at least one eligible assignee);
  new test `session_start_accepts_external_orchestrator`. Handoff keeps
  both strict checks — intrinsic to the delegated model (owner IS the
  initiator and a transfer party, RFC-0010 §2/§3) — now documented at the
  check. CHANGELOG entries added.
- **2026-07-05** — **A7 UNBLOCKED — macp-proto 0.1.4 released**: PR #45
  merged (`04f8332`), publish workflow run 28747377932 dispatched with
  version 0.1.4, all 8 package jobs green (crates.io shows 0.1.4 as
  newest, PyPI 0.1.4 live, Go module tagged `packages/proto-go/v0.1.4`).
  Runtime `feat/a7-multi-round-proto` rebuilt against the PUBLISHED crate
  (not the path dep): build + 22 multi_round tests + 14 conformance tests
  green; Cargo.lock pinned to registry 0.1.4 and pushed (`0aa37a9`).
  Naming note: the release was tagged `v0.1.4`, but the workflow's tag
  trigger and the previous release use the `proto-v*` convention
  (`proto-v0.1.3`) — harmless here because workflow_dispatch supplied the
  version explicitly, but future releases should tag `proto-vX.Y.Z` (or
  dispatch consistently) so tag-triggered publishes work and spec-version
  `v*` tags stay unambiguous.
- **2026-07-05** — **JWKS follow-up (adversarial-review advisory)**: refresh
  is now single-flight (refresh mutex + cache re-check; 8 concurrent
  refreshes provably coalesce into 1 fetch —
  `concurrent_jwks_refresh_is_single_flight` with a real local listener)
  and the reqwest client is built once and reused (was per-fetch). 44 auth
  tests green.
