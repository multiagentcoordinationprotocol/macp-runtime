# macp-runtime Improvement Plan

**Date:** 2026-07-03 · **Baseline:** v0.4.0 (`732b689`) · **Status:** **EXECUTED — released as v0.5.0 on 2026-07-05** (all seven crates on crates.io; see `plans/BUILD_STATUS.md` for the task-by-task record and `docs/change-review-phases-a-e.md` for the engineering review). Remaining work: actionable follow-ons in `plans/defer/follow_ons.md`; hard-blocked items in `plans/defer/README.md` (§9 unchanged). This document is retained as the rationale/evidence source of record.

This plan is the result of a full audit of the runtime against the normative spec
(`../multiagentcoordinationprotocol/`, RFCs 0001–0012), covering the kernel/transport
(`src/`), the mode/policy crates, the storage/auth crates, tests/CI/docs, and the
existing planning corpus (`plans/current/`, `plans/defer/`).

**How this relates to `plans/`:** On 2026-07-04 this master plan was decomposed into
per-phase execution plans under `plans/current/` (`README.md` index,
`rfc-changes.md`, `phase-a-prefreeze.md` … `phase-e-features.md`). Work from those
files; this document remains the rationale/evidence source of record. On 2026-07-03 the
deferred-work corpus (`plans/defer/`, last reviewed 2026-04-07) was **merged into this
plan**: the actionable items (multi-round proto payloads, integration-test gaps,
replay-consistency validation, conformance-pack publishing, recovery benchmarking,
pluggable policy-engine trait, policy-driven audit logging, lightweight dev tooling)
are promoted into the phased roadmap below (§4, §5, §8), and only genuinely blocked
items remain deferred (§9). The absorbed defer files were deleted; `plans/defer/`
retains only the still-blocked plans and a slim index. Note that the old defer docs
predate the workspace split — their `src/mode/*`, `src/policy/*` paths are stale;
paths in this document are current.

**Version-control note (resolved 2026-07-05):** `plans/` is gitignored but the
planning corpus was force-added (`git add -f plans/`) and is committed — this
file, `plans/current/`, `plans/defer/`, and `BUILD_STATUS.md` are all tracked.
The original warning (sole uncommitted copy of the absorbed defer content) no
longer applies.

**Framing constraint:** v0.4.0 is a *unary-first freeze candidate*. Anything that is a
breaking API or wire-adjacent change must land **before** the freeze; that drives the
priority ordering below.

**Review log:** two independent adversarial verification passes were run on this
document (2026-07-03). Pass 1 spot-checked ~25 file:line claims (all confirmed; one
normative citation corrected) and forced replay-migration analysis into §1.9/§2.5,
the combined §1.2+§1.4 sequence contract, and the §1.10 RFC adjudication. Pass 2
verified all pass-1-introduced claims (all confirmed), audited the `plans/defer`
merge, added the §3.2 stream-bus leak, refiled §1.12 to P1, and flagged the
version-control risk above. Claims about sibling repos (control-plane, ui-console)
are from their READMEs and were not independently re-verified.

---

## 1. MUST DO — correctness & security fixes (P0)

These are verified defects, not hypotheticals. Each has a file:line anchor.

### 1.1 Authenticate `WatchSignals`
`server.rs:1055` — `watch_signals` never calls `authenticate_metadata`. Any
unauthenticated caller can subscribe to the entire ambient signal plane, including
`SignalPayload.data`. (Precision on the normative basis: RFC-MACP-0004 §4.1 governs
unauthenticated signal **producers**, not subscriber auth — the spec does not
strictly mandate authenticating watchers. This is nonetheless the right hardening
call under RFC-0004 §1's confidentiality/resource-exhaustion posture: signal payloads
are agent-generated data and the stream is an unmetered resource.) Fix: require auth
like `watch_sessions`/`list_sessions`. Decide explicitly (and document) whether
`WatchModeRegistry` (`server.rs:1000`) and `WatchRoots` (`server.rs:1030`) stay
unauthenticated as discovery surfaces.

Related scoping decision to record while here: `ListSessions`/`WatchSessions` return
**all** sessions' metadata (intent, participants) to any authenticated identity,
while `GetSession` enforces per-session membership via `authenticate_session_access`.
RFC-0006 sanctions the shape, but the asymmetry should be a documented decision
(observer-role only? participant-scoped filtering?), not an accident.

### 1.2 Design the passive-subscribe sequence contract (race + ordinal + off-by-one + compaction)
Three defects in the RFC-MACP-0006 §3.2 normative path, plus a coupling to
compaction (§1.4). These must be fixed as **one design item** — a sequence contract —
not as independent patches. (The `macp-control-plane` sibling repo builds its core
read loop on this path — claim from its README/invariants, not re-verified here.)

- **Duplicate delivery race** — `process_subscribe_frame` (`server.rs:348`) subscribes
  to the live broadcast (`server.rs:385`) *before* reading log history
  (`server.rs:396`) without a consistent view; publishes happen under the registry
  write lock (`runtime.rs:487,593`). An envelope accepted in that window is delivered
  twice (once in replay, once from the buffered receiver). **Primary fix: dedupe the
  buffered receiver against the replay boundary** (drop drained events at or below
  the last replayed ordinal). Do *not* fix this by taking the global registry lock
  across subscribe+read — that hard-codes the very lock §3.1 removes and would be
  redone in Phase D.
- **Non-resumable sequence numbers** — `get_incoming_after` (`log_store.rs:66`)
  compares `after_sequence` against the **raw combined log index** (Incoming +
  Internal + Checkpoint entries, `log_store.rs:79`), so client-visible sequences are
  non-contiguous and shift meaning as internal entries interleave.
- **Off-by-one vs the RFC** — RFC-0006 §3.2 specifies replay "starting from
  `after_sequence + 1`" (exclusive-after; `0` = from the start), but the code filters
  `idx >= after_sequence` (inclusive-from, `log_store.rs:79`). A spec-conformant
  client resuming with its last-seen sequence gets that envelope re-delivered.

**Contract to implement:** the per-session sequence is the **1-based ordinal of
accepted Incoming envelopes**, `after_sequence` is exclusive (`0` = from start), and
— since `Envelope` carries no sequence field on the wire — clients derive it by
counting delivered envelopes, so the ordinal must be stable across restarts and
compaction. That last requirement binds §1.4: the compaction checkpoint must record
the ordinal count of discarded entries (or retain the Incoming entries), otherwise
resume-after-restart on a compacted session is undefined. Cover with tier-1 tests
that interleave suspend/resume/checkpoint entries and resume across a
compaction+restart.

Phasing note: this changes observable behavior on a frozen proto field
(`after_sequence`), which by this plan's own rule would put it in Phase A. It sits in
Phase B deliberately: the current behavior is a spec-conformance *bug* (fixing it
converges on the contract RFC-0006 already binds clients to), and rushing the design
without the §1.4 compaction decision would produce a second incompatible sequence
definition. If the freeze is imminent, at minimum land the contract *decision*
(ordinal, exclusive-after) before freezing, even if the implementation follows.

### 1.3 Surface broadcast lag instead of silently closing streams; fix the `WatchSessions` sync race
`server.rs:1060` (`watch_signals`) and `server.rs:1109` (`watch_sessions`) use
`while let Ok(env) = rx.recv().await` — a `Lagged` error ends the stream with `Ok(())`
and no status. A slow consumer is silently disconnected and cannot distinguish "no
traffic" from "dropped". Fix: match on `RecvError::Lagged` and terminate with
`ResourceExhausted`, as the `StreamSession` path already does (`server.rs:528`).

Also in `watch_sessions`: the same subscribe-before-snapshot race class as §1.2 —
it subscribes to the lifecycle bus (`server.rs:1094`) *before* reading the initial
session list (`server.rs:1098`), so a lifecycle event in the window is emitted twice
(once as initial-sync state, once as a live event). Apply the same dedupe-on-drain
approach — but note the dedupe key differs from §1.2's ordinal: the initial sync
emits only `Created`-shaped events and session IDs are create-once, so dedupe
buffered **`Created`** events by `session_id` against the snapshot; buffered
non-`Created` events (Resolved/Suspended/…) are not duplicates and must pass through.

### 1.4 Fix compaction memory/disk desync (affects every terminal session by default)
`maybe_compact_log` → `replace_log` rewrites **storage only** (`runtime.rs:831-845`,
`storage/compaction.rs:36`); the in-memory `log_store` keeps full history. Note the
severity: compaction runs on **every** session reaching a terminal state
(`runtime.rs:586-589,931`) — it is *not* gated by `MACP_CHECKPOINT_INTERVAL` (that
gates mid-session checkpoints only; CLAUDE.md's env table is misleading on this —
fix in §6.6). Effects: (a) memory and disk diverge for every resolved session;
(b) after a restart, passive-subscribe history for a compacted session is **empty**
(only the checkpoint survives). Fix: update `log_store` in the same operation, and
settle the post-compaction history contract **jointly with the §1.2 sequence
contract** — either the checkpoint records the discarded-Incoming ordinal count (and
compacted sessions reject history replay with a clear error) or Incoming entries are
retained in the checkpoint. Silence is the only wrong option.

### 1.5 Backend durability honesty (RocksDB, Redis)
The runtime's core invariant — "log append failures are fatal; ack implies durable" —
only holds on the file backend (`storage/file.rs:104-118` does `sync_data`).

- **RocksDB** (`storage/rocksdb.rs:172-182`): default `WriteOptions` (sync=false); a
  crash can lose acked entries. Fix: use `WriteOptions::set_sync(true)` for log
  appends (or a configurable durability knob defaulting to sync).
- **Redis** (`storage/redis_backend.rs:104-111`): `rpush` acks in-memory; no `WAIT`,
  no AOF guarantee, and `replace_log` (`:128-144`) is a non-atomic DEL + N RPUSH.
  Fix: document Redis as non-durable/cache-tier in `deployment.md`, make `replace_log`
  atomic (MULTI/EXEC or Lua), and consider requiring an explicit
  `MACP_REDIS_ACKNOWLEDGE_NON_DURABLE=1` opt-in.
- **Corrupt-entry parity**: one bad log entry fails the *whole session load* on
  RocksDB/Redis (`rocksdb.rs:197`, `redis_backend.rs:121`) but is skipped per-line on
  file (`file.rs:131-140`). Pick one behavior (recommend: per-entry skip + warning,
  fatal under `MACP_STRICT_RECOVERY`) and apply to all backends.
- **File backend nits**: fsync the tmp file before rename in `atomic_write`
  (`file.rs:34-38`) and fsync the parent directory; keep a persistent file handle per
  active session log instead of open/fsync/close per append (`file.rs:110-117`).

### 1.6 JWT/JWKS hardening
`crates/macp-auth/src/auth/resolvers/jwt_bearer.rs`:

- **Drop HS256 from the default algorithm allowlist** (`security.rs:154-159`). If an
  operator's JWKS ever contains an `oct` key, symmetric tokens become accepted.
  RS256/ES256 default; HS256 only by explicit config. Coordinate with `auth-service`
  (mints RS256 — unaffected).
- **JWKS fetch has no HTTP timeout** (`jwt_bearer.rs:122`, bare `reqwest::get`) — a
  hanging endpoint blocks auth indefinitely. Add a client with connect/total timeouts.
- **No stale-cache fallback** (`jwt_bearer.rs:107-118`) — when the TTL expires and the
  JWKS endpoint is down, *all* JWT auth fails. Serve stale keys with a warning while
  refresh fails (bounded stale window).
- **Use `kid` for key selection** instead of trying every key (`jwt_bearer.rs:219-240`).
- **Replace `block_in_place` + `block_on`** in `authenticate_metadata`
  (`security.rs:244-252`) with a properly async path — it blocks a worker thread for
  the entire JWKS fetch and panics on a current-thread runtime.

### 1.7 Gate dev-mode auth fallback on an explicit flag
`security.rs:89-103, 266-267` — with no tokens and no issuer configured, *any* bearer
token authenticates as a fully-privileged identity (`can_start_sessions`,
`can_manage_mode_registry`). This is independent of `MACP_ALLOW_INSECURE`. An operator
who forgets auth env vars silently runs an any-token-is-admin server (the code
comment at `security.rs:90` claiming this path is "used ONLY in tests" is false).
Fix: refuse to start without configured auth unless `MACP_ALLOW_INSECURE=1` (reusing
the existing flag keeps the local-dev flow one variable). Update README/CLAUDE.md dev
instructions. **Compound operational break, intentional:** this plus §6.1 (removing
`ENV MACP_ALLOW_INSECURE=1` from the Docker image) means a bare `docker run` of the
published image fails at startup instead of silently running open — ship both
together with quickstart docs showing the explicit dev flags, and call it out in the
release notes.

### 1.8 Close the `PromoteMode` namespace/validation holes
`mode_registry.rs`:

- `register_extension` forbids `macp.mode.*` (`:472`) but
  `promote_mode(mode, Some("macp.mode.x.v1"))` re-keys straight into the reserved
  namespace (`:568`) and flips `strict_session_start=true` (`:589`) — a
  passthrough-backed, schema-less mode can masquerade as standards-track. Fix: apply
  the same namespace guard to the promotion target.
- `validate_extension_descriptor` (`:468`) does not require any terminal message type;
  a mode with empty `terminal_message_types` (or `Commitment` missing from
  `message_types`) can never resolve — sessions can only expire. Fix: require at least
  one terminal type present in `message_types`.

### 1.9 Enforce version binding for extension modes (with replay migration)
`util.rs:24` + `runtime.rs:337-344` — ext modes may bind `mode_version=""` at
SessionStart, making the commitment version check (`commitment.mode_version ==
session.mode_version`) vacuously true for `""`. The freeze invariant "CommitmentPayload
version fields must match session-bound versions" is only real for standards-track
modes. Fix: when the SessionStart payload's version is empty, bind the registered
descriptor's `mode_version` (descriptors already require non-empty `mode_version`,
`mode_registry.rs:468`) — **and persist the bound value in the session/log so replay
uses the recorded binding, never re-derives it from the live registry** (ext
descriptors are dynamic; re-deriving on replay would diverge if the registration
changed).

**Migration constraint (this is not a one-liner):** legacy persisted ext sessions
whose accepted Commitments carry `mode_version:""` must still replay — a strict
equality check against a newly-derived binding would fail them, violating the
replay invariant (CLAUDE.md §5, RFC-0003 §1). Apply the new binding only to sessions
whose recorded SessionStart bound a non-empty version; histories with an
empty-version binding keep the legacy (vacuous) check. Add a replay test with a
pre-fix log fixture.

### 1.10 Resolve the Quorum threshold double-meaning
The same policy `threshold.value` gates **two different quantities**: the mode treats
it as required approvals (`quorum.rs:66,99`), the evaluator as a participation quorum
over approve+reject voters (`evaluator.rs:606`). A config satisfying one gate can be
denied by the other. RFC-0012 §4.2 adjudicates this: `threshold` "overrides the
`required_approvals` from ApprovalRequest" — the mode's reading is correct and **the
evaluator's participation check is non-conformant**. Fix: drop the evaluator's
participation interpretation and align it with the mode (a separate participation-
quorum rule field, if wanted, is an RFC-0012 schema addition — file upstream first,
§7 item 12). Add a conformance vector. This interacts with §2.2 (outcome-aware
evaluation): today a *legitimate decline* (threshold mathematically unreachable) can
be blocked by the outcome-blind evaluator.

Related unit defect found during review: the canonical schema types `percentage`
thresholds as an **integer fraction (0–1)** while `quorum.rs:74` divides by 100
(expects `60`-style values) — a third meaning for the same field, and an upstream
schema defect (integer-typed fraction), §7 item 12.

### 1.11 Fix the rate limiter's O(all-senders) scan
`security.rs:295-332` — `check_bucket` prunes the entire sender map on every request;
the comment claims a 100-entry cap that the code does not implement (`:305-317`).
Sender cardinality is attacker-controllable (JWT `sub`, dev tokens). Fix: implement
the cap the comment promises, or move pruning to a periodic task; fix the comment.

### 1.12 Record rejection metrics — *refiled as P1, execute with §4.1*
`metrics.rs:69,111` — `record_message_rejected` / `record_commitment_rejected` have
**zero callers**; the counters are permanently 0. Wire them into the `Send` error path
(`server.rs:725`) and the mode-rejection paths. Also include the collected-but-dropped
`sessions_suspended`/`sessions_resumed` in `MetricsSnapshot` (`metrics.rs:141,170`).
This is an observability gap, not a correctness/security defect — nothing misbehaves,
and the counters are invisible until §4.1 exports them — so it is **not P0**; the
number is kept only to preserve cross-references. Do it as the first step of §4.1
(Phase D).

---

## 2. MUST DO — pre-freeze API shape (P0, breaking-change window)

These are cheap now and expensive forever after the freeze.

### 2.1 Make public types evolvable
- `Session` (`macp-core/src/session.rs:37`) has ~25 `pub` fields, no builder, no
  `#[non_exhaustive]` — adding any field post-freeze breaks every constructor
  (there are ~10 full-literal copies in mode tests alone). Add a builder (or
  `Session::new(required…)` + setters), mark `#[non_exhaustive]`, migrate callers.
- Mark `MacpError` (`error.rs:4`), `ModeResponse` (`mode.rs:10`),
  `PolicyDecision`/`PolicyError` (`policy/mod.rs:25,31`), and the mode phase enums
  `#[non_exhaustive]`.

### 2.2 Unify the `PolicyEvaluator` trait around a commitment context
`policy/mod.rs:102` — the trait is five positional-primitive methods
(`evaluate_quorum_commitment(usize, usize, usize, usize)` is transposition-prone), and
only Decision has an outcome-aware variant (`evaluate_decision_commitment_outcome`,
added as a defaulted duplicate). Freezing this locks the other four modes out of
outcome-awareness (negative/decline commitments — the direction RFC-0012
schema_version 2 just took) without a breaking change. Replace with a single
`evaluate_commitment(ctx: &CommitmentContext) -> Result<PolicyDecision, PolicyError>`
where `CommitmentContext` carries mode id, accumulated state, and `outcome_positive`.
Keep the old methods as deprecated shims for one release if external consumers exist.

### 2.3 Decide the `policy.default` echo contract
`runtime.rs:388-408` rewrites empty `policy_version` → `"policy.default"` on the
session; `util.rs:30` then requires the Commitment to echo it. So a client that sent
`""` at SessionStart **must** echo `"policy.default"` in its Commitment or be rejected
`InvalidPayload`. Mode unit tests hide this (they build sessions with
`policy_version:""` directly). Decide: either accept empty `commitment.policy_version`
as matching the default, or make SDKs echo the resolved value (they can read it from
`GetSession`). Either way: add a tier-1 test for the empty-at-start case and file the
upstream ambiguity (§7 item 10).

### 2.4 Session-ID validation fix
`macp-core/src/session.rs:236` — a 36-char base64url token containing `-` is routed to
the UUID branch and rejected, though it is valid per the base64url rule. Fix the
branch condition (attempt UUID parse; fall through to base64url on failure). Trivial,
but it is acceptance-affecting wire behavior — do it before freeze.

### 2.5 Decide the Handoff implicit-accept trust model
`handoff.rs:229-243` auto-accepts using the **Commitment envelope's**
`timestamp_unix_ms` — initiator-forgeable: a future timestamp finalizes an offer the
target never accepted. This was chosen for replay determinism, but it converts a
policy timer into an initiator capability.

The real contract is RFC-0012 §4.5: a runtime **timer emits a synthetic accept into
history**, which the runtime does not implement — file the timing/authority/
determinism questions upstream (§7 item 10) and treat anything else as interim.

**Interim mitigation** — use the runtime's acceptance timestamp of the Commitment
instead of the client-supplied one. This is *not* a local one-liner:
- `Mode::on_message(&self, session, env)` has no acceptance-time parameter, and
  replay deliberately reconstructs envelopes with the original client
  `timestamp_unix_ms` (`replay.rs:108-114`). Passing acceptance time means a Mode
  trait signature change (breaking — co-sequence with the §2.1/§2.2 trait work) plus
  plumbing the log entry's `received_at_ms` through replay.
- Legacy histories where an offer was implicitly accepted under the old semantics
  could replay to a different outcome under the new clock — same migration rule as
  §1.9: switch semantics on new sessions only (gate on a recorded marker), keep old
  logs on old semantics, and add a pre-fix log fixture test.

---

## 3. SHOULD CHANGE — architecture & scalability (P1)

### 3.1 Break the global write lock across storage I/O
`runtime.rs:359-486` and `:508-599` hold the single `registry.sessions.write()` across
`create_session_storage` / `append_log_entry` / `save_session` awaits (and
`cleanup_expired_sessions`, `runtime.rs:904-949`, holds it across storage appends for
every expired session in one sweep). The entire
runtime processes **one session-scoped message at a time**, and one slow backend write
stalls every session. It is also load-bearing for correctness today (it is what makes
RocksDB's non-atomic `next_seq` read-then-write safe, `rocksdb.rs:43-76`).
Fix: per-session locking — `DashMap<SessionId, Arc<Mutex<SessionSlot>>>` or an
actor-per-session model. RFC-0001 §8.1 only requires serialization *within* a session.
This is the single biggest throughput improvement available. Prereqs: make RocksDB
`next_seq` atomic per session (it is per-session-keyed, so the per-session lock
suffices), and audit `max_open_sessions` (keep its check-and-insert atomic).

### 3.2 Bound in-memory growth
- `log_store` cache (`log_store.rs:30-31`) retains every session's full log (including
  raw payloads) for the process lifetime; `evict_stale_sessions` (`runtime.rs:953-976`)
  evicts the registry but never `log_store.logs`. Evict both together.
- **`SessionStreamBus` channels are never removed** (`stream_bus.rs:27-36`): every
  session ever streamed or passively subscribed leaves a permanent
  `broadcast::Sender` map entry, and a lagging receiver pins up to 256 buffered
  envelopes (payloads up to 1 MB each). There is no removal API at all — add one and
  call it from the same eviction path (and on session terminal state once no
  receivers remain).
- `seen_message_ids` grows one entry per accepted message forever and is re-serialized
  on every `save_session`. Acceptable per-session; document the implication for
  long-lived sessions, and consider a windowed dedup set post-v1 (needs spec care —
  dedup is normative).
- Registry memory floor grows monotonically across restarts because expired sessions
  are reloaded from disk forever — fixed properly by §5.3 (on-disk retention/GC).

### 3.3 Server resource limits & graceful shutdown
- `main.rs:283` sets no tonic limits — add `concurrency_limit_per_connection`,
  `max_concurrent_streams`, request timeouts, and TCP keepalive. Combined with §3.1,
  this closes the unbounded-queuing DoS surface (RFC-0004 §7).
- `main.rs:332` — ctrl-c drops the server future, killing in-flight RPCs. Use tonic's
  `serve_with_shutdown`, drain streams with a timeout, then snapshot sessions.
- `MACP_MAX_PAYLOAD_BYTES` only bounds the inner payload after tonic has decoded up to
  its own 4 MB default (`server.rs:76-78`). Set tonic's `max_decoding_message_size`
  from the same config so the ingress bound is real.

### 3.4 Paginate the unbounded list surfaces
`server.rs:1072` (`ListSessions`) clones every live session into one response;
`watch_sessions` initial sync (`server.rs:1098`) has the same shape. Add page
token/limit to `ListSessions` (needs a proto change → coordinate upstream, §7) or at
minimum a server-side cap with a documented default.

### 3.5 Consistency cleanups (small, from the mode audit)
- Extract the copy-pasted commitment epilogue (validate → ready-guard → policy eval →
  `PersistAndResolve`) from the 5 standards modes into `util::finalize_commitment`
  (anchors: `decision.rs:231`, `proposal.rs:353`, `task.rs:326`, `handoff.rs:223`,
  `quorum.rs:218`).
- Deduplicate `encode_state`/`decode_state` (6 copies) into a generic codec in `util.rs`;
  deduplicate `extract_commitment_rules` (`policy/mod.rs:89` vs `util.rs:125`).
- Normalize SessionStart participant validation: Task/Handoff require ≥2 participants
  and initiator∈participants; Decision/Proposal/Quorum/Multi-round only require
  non-empty. Verify each against its RFC and align or comment why they differ.
- Remove the dead `HandoffContext` authorize branch (`handoff.rs:84`).
- Malformed policy rules currently degrade to defaults via `unwrap_or_default()`
  (`evaluator.rs:87`, `quorum.rs:69`) — make parse failures at evaluation time a
  `PolicyError`, not silent defaults (they were validated at registration, so this
  should be unreachable — make it loud if it isn't).
- Passthrough replaces `mode_state` wholesale per message (`passthrough.rs:46`) —
  either accumulate (append entries) or document replace-semantics in the ext-mode docs.
- Delete the dead persistence path in `registry.rs:179-219` (`persist_map` /
  `PersistedSession` — never wired; `main.rs:179` constructs without a path) and the
  unused `recovery::recover_session` (`storage/recovery.rs:6-24`), or wire them; two
  parallel persistence mechanisms with one dead is a maintenance trap.
- Either wire the extension provider registry (`runtime.rs:45` `#[allow(dead_code)]`,
  `on_session_start`/`on_session_terminal` never invoked — RFC-0001 §7.4.2 describes
  these hooks) or remove it until there is a consumer.

---

## 4. FEATURES TO ADD

### 4.1 Metrics export (highest-value feature; currently zero observability)
`RuntimeMetrics::snapshot()` has no callers — every counter is write-only. Add a
Prometheus `/metrics` HTTP endpoint (feature-gated alongside `otel`), including the
mode counters, rejection counters (§1.12), and new storage metrics (append latency,
fsync failures, log sizes, compaction counts, recovery skips) and auth metrics
(auth failures, rate-limit hits, JWKS refresh failures). The `macp-ui-console`
sibling repo surfaces "runtime health" via the control-plane and currently has
nothing to read (per its README — not re-verified in this audit).

### 4.2 `MACP_POLICIES_DIR` (file-loaded policies)
Documented in CLAUDE.md as "(future)" and specified in RFC-0012 §9 /
`registries/policies.md` (`register_policy=false, list_policies=true` profile). Load
policy JSON at startup, validate against mode schemas, pre-register. Natural companion:
ship the RFC's recommended `policy.majority` / `policy.supermajority` /
`policy.unanimous` as optional built-ins once upstream reserves them.

### 4.3 On-disk retention/GC
`storage.delete_session` is never called anywhere (verified) — disk grows without
bound; `MACP_SESSION_RETENTION_SECS` only evicts memory. Add
`MACP_SESSION_DISK_RETENTION_SECS`: terminal sessions past retention get archived
(export JSONL) or deleted, per config. This also fixes the §3.2 restart memory floor.

### 4.4 Roots: implement or explicitly disclaim
`ListRoots` always returns empty (`server.rs:973`); `WatchRoots` emits one event then
`pending()` forever (`server.rs:1042`). Either implement a minimal roots provider
(static config file) or advertise `roots.list_changed=false` / omit the capability in
`Initialize` so clients don't wait on a stub. Decision criterion: if no ecosystem
consumer (SDKs, control-plane) reads roots today, disclaim — implementing a provider
without a consumer is speculative. Note the disclaim option changes capability
advertisement in `Initialize`, which is wire-adjacent — make the *decision* in
Phase A even if a provider implementation waits until Phase E. Acceptance: either a
config-backed provider with a tier-1 test, or capabilities that no longer promise
roots and a test asserting that. Same decision for `WatchModeRegistry`'s payload-less
"changed" pings (`server.rs:1000`) — RFC-0006 §3.3 allows ping-style, but document it.

### 4.5 Multi-round protobuf payloads (promoted from defer Tier 1, ~2 days)
Still not done (`src/bin/multi_round_client.rs:42,58,78` send raw JSON
`{"value":"option_a"}`); multi-round is the only mode off the canonical proto
encoding. Do it inside the freeze window since it changes wire payloads.

Plan (updated from the original defer doc, which predates the workspace split and
assumed protos live in this repo — they now come from the upstream `macp-proto`
crate):
1. **Proto definition upstream**: add
   `macp/modes/multi_round/v1/multi_round.proto` (`ContributePayload { string value = 1 }`)
   to the spec repo's proto package; publish a new `macp-proto` version; bump it in
   the root `Cargo.toml` `[workspace.dependencies]`. `ResolutionPayload` and
   `MultiRoundState` stay internal JSON — only the wire payload needs proto.
2. **Codegen**: generate the message in `crates/macp-pb` (prost-only) alongside the
   other mode payloads.
3. **Mode update** (`crates/macp-modes/src/mode/multi_round.rs`): parse protobuf
   first, fall back to JSON for one release cycle (old logs must still replay — the
   fallback is a replay-compatibility requirement, not just client compat).
4. **Fixtures + client**: update `tests/conformance_loader.rs` payload encoding and
   `src/bin/multi_round_client.rs`.
5. Remove the JSON fallback one release after; keep replay fallback until a log
   migration exists.

Open decision carried from the defer doc: whether `value` should be `string` or
`bytes`/structured — decide before publishing the proto (wire-frozen after).

### 4.6 Pluggable policy-engine trait + policy-driven audit logging (promoted from defer Tier 2)
The built-in evaluator is done; what remains (trait sketch inlined from the deleted
`policy_engine.md`):

(a) a higher-level, identity-aware `PolicyEngine` trait so external engines
(OPA, Cedar) can be plugged in:

```rust
#[async_trait]
pub trait PolicyEngine: Send + Sync {
    async fn evaluate_session_start(&self, identity: &AuthIdentity, mode: &str,
        payload: &SessionStartPayload) -> PolicyDecision;
    async fn evaluate_message(&self, identity: &AuthIdentity, session: &Session,
        env: &Envelope) -> PolicyDecision;
    async fn evaluate_session_access(&self, identity: &AuthIdentity,
        session: &Session) -> PolicyDecision;
}
```

Design decisions to settle before implementing: error semantics (deny-on-error vs
fail-open — must be deny), determinism boundary (RFC-0012 §6.3 requires commitment
evaluation be a pure function — an *async external* engine can only govern
non-replayed decisions like session-access, or must be snapshot-recorded into
history; this constraint is why the trait is separate from `PolicyEvaluator`), and
where it composes with §2.2's `CommitmentContext`. Acceptance: a test double engine
that denies one designated sender proves all three hook points fire.

(b) per-policy/per-mode audit-log verbosity instead of global tracing: a
`audit` rules block (verbosity level per mode/policy) consumed at the tracing
call sites in the kernel. Acceptance: two sessions under different policies emit
different audit detail.

Sequencing: **after** §2.2 — designing the pluggable trait around the current five
positional methods would freeze the wrong shape. Tenant isolation (the third item in
that defer doc) stays deferred until a multi-tenant deployment exists (§9), because
it requires tenant identity in `AuthIdentity` and RFC-0004 §11 authz-layer scoping.

### 4.7 Lightweight developer tooling (promoted subset from defer Tier 3)
From the deleted `developer_tools.md`, promote only the two cheap, low-risk items:
- **Transcript visualizer** (~3 days): a `macp-transcript-viz` CLI (new bin target)
  that takes a conformance fixture JSON *or* a session log (JSONL) and emits a
  Mermaid `sequenceDiagram` (participants = senders; one arrow per accepted message;
  a note on resolution). No coupling to code structure. Acceptance: rendering every
  fixture in `tests/conformance/` produces valid Mermaid (lint in CI is optional).
- **Schema publishing**: publish the protobuf schemas to buf.build. This is
  spec-repo work (protos live there and it already has `buf.yaml`/`buf` config) —
  the runtime-side action is to file/coordinate it upstream and, once published,
  reference the BSR module in docs.
The fixture generator, state-diagram generator, and `macp-mode-derive` proc-macro
stay deferred (§9): they are code-structure-coupled or high-effort, and there is no
community mode-author audience yet.

---

## 5. TEST & CI IMPROVEMENTS

### 5.1 CI feature-flag matrix (biggest CI gap)
`rocksdb-backend`, `redis-backend`, and `otel` are never compiled, linted, or tested in
CI — clippy has never seen that code. RocksDB's unit tests are self-contained
(tempdir) and would run today; Redis tests silently self-skip without
`MACP_TEST_REDIS_URL` (`redis_backend.rs:197-222`), so a broken backend cannot fail CI.
Add: a matrix job building/testing `--features rocksdb-backend` and
`--features redis-backend` with a `redis:7` service container (set
`MACP_TEST_REDIS_URL`), plus `--all-features` clippy. Make the Redis tests *fail* (not
skip) when the env var is set but unreachable.

### 5.2 Test on stable, keep 1.89 as explicit MSRV check
CI currently tests **only** on the pinned 1.89.0 — inverted from the usual pattern.
Run the main test job on stable; keep a separate `cargo check` job on 1.89 as the MSRV
gate. Also: move `cargo audit` out of the required `ci-pass` gate (external-feed
flakiness) into a scheduled job + PR warning; consider `cargo-deny` for licenses/bans.

### 5.3 Gate PRs on tier-1 integration tests
The entire real-gRPC-boundary suite (`integration_tests/`, 82 tests) is
`workflow_dispatch`-only. Tier 1 is hermetic (spawns the built binary, serial). Add it
to the PR path (it needs `cargo build` first; budget ~5-10 min). Tier 2 stays manual,
tier 3 stays ignored/manual.

### 5.4 New tests this plan requires
Each P0 fix above lands with a regression test; notable ones:
passive-subscribe race (accept during subscribe window → no duplicates),
`after_sequence` resume across interleaved internal entries, compacted-session
restart history behavior, RocksDB sync-write crash test, ext-mode promote-to-reserved
rejection, quorum threshold semantics vector (add to `tests/conformance/`), the
`policy.default` echo case, WatchSignals auth, lag → `ResourceExhausted`.

**Plus the five items promoted from `test_gaps.md`** (~2.5 days; the deleted defer
doc's sketches are inlined here — this section is now the canonical scope):
1. **TTL expiry integration tests** — through the full runtime stack: start a session
   with a short TTL, sleep past it, assert the next session-scoped message is
   rejected `TtlExpired` and `GetSession` reports `Expired`. Use 50ms TTL + 100ms
   sleep (not 1ms/5ms) to avoid CI flakiness under load.
2. **WatchModeRegistry / WatchRoots streaming tests** — open each stream, assert the
   first event arrives (registry: `registry == "modes"`; roots:
   `observed_at_unix_ms > 0`); skip "stays open but idle" timeout assertions (slow,
   low value).
3. **Signal-no-mutation test** — open a session, send a Signal (empty
   `session_id`/`mode` per envelope rules), assert the ack is non-duplicate, then
   re-fetch the session and assert state, dedup set, and history are untouched
   (freeze invariant §7 of CLAUDE.md).
4. **FileBackend atomicity** — walk the data dir after a full session lifecycle and
   assert no `*.tmp` files remain (extend to cover the §1.5 fsync-before-rename fix).
5. **Concurrent SessionStart stress** — spawn ~20 concurrent starts against
   `max_open_sessions=5`, assert exactly ≤5 accepted (guards the TOCTOU fix at
   `runtime.rs:370-386`; re-verify after the §3.1 per-session-locking change).

### 5.5 Replay consistency validation (promoted from defer Tier 1, ~2 days)
Warn-only comparison of the replayed session against the `session.json` snapshot
during startup recovery: state, participants, mode/configuration versions, dedup
count. Emit `tracing::warn!` per divergence and a `replay_mismatches` counter in
`RuntimeMetrics` (surfaced by §4.1's exporter — sequence this after §4.1 so the
counter is actually visible, and after §1.4 so compaction-induced divergence doesn't
produce false positives). The original deferral reason (false-positive noise from
best-effort snapshots) is addressed by warning only on state/dedup-count mismatches,
per the defer doc's own mitigation.

### 5.6 Recovery benchmarking (promoted from defer Tier 2)
Replay is unprofiled at scale. Add criterion benches: replay time vs log size
(100/1K/10K entries), checkpoint-based vs full replay, memory during replay of large
sessions. Do this **before** §3.1 lands so the locking rework has a perf baseline,
and use it to validate §1.5's sync-write costs on RocksDB.

### 5.7 Conformance alignment + conformance pack (promoted from defer Tier 1)
`tests/conformance/` fixtures are local and use Rust-internal `payload_type` names
(`"decision.Proposal"`), while the spec repo ships canonical fixtures at
`schemas/conformance/*.json` with a linter. Absorbing `conformance_pack.md` (with its
staleness corrected — its Phases 1.2/1.3, resolution and mode-state validation, are
**already implemented** in `tests/conformance_loader.rs:31-38`):

1. **Canonical `payload_type` names** (~2 days): rename Rust-internal prefixes to
   fully-qualified proto names (`decision.Proposal` →
   `macp.modes.decision.v1.ProposalPayload`, `Commitment` →
   `macp.v1.CommitmentPayload`); update the `encode_payload()` match arms in
   `conformance_loader.rs` atomically with the fixtures (CI breaks otherwise).
2. **Fixture JSON schema** (~1 day): `tests/conformance/schema.json`
   (draft-2020-12). Shape (reconstructed from the deleted defer doc — this is now
   the canonical draft): required top-level fields `mode` (pattern
   `^macp\.mode\.[a-z_]+\.v\d+$`), `initiator`, `participants` (minItems 1),
   `mode_version`, `configuration_version`, `ttl_ms` (integer ≥1), `messages`,
   `expected_final_state` (enum `Open|Resolved|Expired`); optional `policy_version`;
   each message requires `sender`, `message_type`, `payload_type`, `payload`
   (object), `expect` (enum `accept|reject`). Extend beyond the original draft with
   the already-implemented optional fields (`expected_resolution`,
   `expected_mode_state`, `expect_resolution_present`,
   `verify_replay_equivalence`) and the terminal states the draft predates
   (`Cancelled`, `Suspended`).
3. **Publish the pack** (~2 days): package `fixtures/` per mode + schema + pass/fail
   rules as a distributable `conformance-pack/` directory, coordinated with the spec
   repo's `schemas/conformance/` so there is exactly **one** canonical fixture source
   (recommend: spec repo owns fixtures, runtime consumes them; that also gives the
   CI oracle below).
4. **CI oracle**: a job that runs the runtime against the spec repo's fixtures, so
   runtime and spec cannot drift silently.

Cross-runtime validation (a second implementation passing the pack) remains deferred
— it requires a second implementation to exist (§9).

---

## 6. DOCS & HYGIENE

1. **Dockerfile**: remove `ENV MACP_ALLOW_INSECURE=1` from the published image (make
   the compose/dev docs set it instead); drop the unnecessary `COPY tests/ tests/`
   (cache-busting, unused by `cargo build --release`).
2. **`docs/deployment.md`**: replace the stale sample Dockerfile (`:97-111` says
   rust:1.85, `/data`, root) with a pointer to the real one; add a **backend
   durability matrix** (file=fsync'd, rocksdb=configurable after §1.5, redis=non-durable)
   and a metrics/health scraping section once §4.1 lands; document the `otel` feature.
3. **Add `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`** (issue templates and PR
   template already exist). SECURITY.md matters for a runtime advertising an auth
   boundary.
4. **Delete `temp/`** (stale 480 KB source zip, a `zip-source.sh` whose version-grep
   is broken for the workspace layout, two outdated architecture docs). If the zip
   script is still needed, fix it to read `[workspace.package]`.
5. **`plans/defer/` cleanup** — done 2026-07-03 as part of this plan: absorbed plans
   deleted, README rewritten to index only still-blocked items (§9), stale
   pre-workspace paths corrected on absorption.
6. **CLAUDE.md touch-ups**: after §1.7, update the dev-auth instructions; add the
   suspend/resume states to the freeze-profile invariants list (they exist in code and
   RFC but not in the invariant bullets); correct the `MACP_CHECKPOINT_INTERVAL` env
   table entry — it gates mid-session checkpoints only, while terminal-session
   compaction is unconditional (see §1.4).

---

## 7. UPSTREAM — issues to file against the spec repo

Found during the audit; the runtime cannot fix these unilaterally. File as issues in
`../multiagentcoordinationprotocol/`:

1. **`MAX_SUSPEND_MS` unspecified** — it is a deterministic replay input (RFC-0003 §2)
   but never bound at SessionStart, so two runtimes with different caps produce
   different terminal states on identical history, breaking the cross-implementation
   replay guarantee (RFC-0003 §1/§8). Should be session-bound like `ttl_ms`.
2. **Mode RFCs 0007–0011 reference the removed `context` field** — `SessionStartPayload`
   replaced `bytes context` with `context_id`+`extensions`; Handoff's context-frozen
   determinism class leans on the removed field.
3. **RFC-0006 §3.6 `SessionLifecycleEvent` text is stale** — lists CREATED/RESOLVED/
   EXPIRED; proto has SUSPENDED/RESUMED/CANCELLED too.
4. **`policy.proto` comment says schema_version "currently 1"** vs RFC-0012 defining {1,2}.
5. **Spec repo CLAUDE.md/README describe the pre-suspension state machine** and a
   `SessionEnd` message that exists in no proto/RFC.
6. **`rfcs/RFC-MACP-0001.md` stub index omits RFC-0012.**
7. **`SessionStartPayload.intent` (field 1) is undocumented** in RFC-0001 §7.1.
8. **Decision vote-cardinality wording** ("or more permissive") permits multiple votes
   with no defined tally semantics — conflicts with the semantic-deterministic claim.
9. **Empty-`policy_version` replay equality** — must the runtime persist the rewritten
   `policy.default`, and what must the Commitment echo? (Runtime-side decision in §2.3.)
10. **Handoff `implicit_accept_timeout_ms` timer** — RFC-0012 §4.5 describes a runtime
    timer emitting a synthetic accept; no timing/authority/determinism contract is
    specified (runtime-side decision in §2.5).
11. **`ListSessions` pagination** — no page token in the proto; needed for §3.4.
12. **Quorum `threshold` schema defects** — (a) the `percentage` type is declared as
    an integer fraction (0–1), which can only express 0% or 100%; implementations
    (including this runtime, `quorum.rs:74`) treat it as a 0–100 integer percentage —
    the schema type is wrong; (b) if a *participation quorum* (distinct from the
    approval threshold that RFC-0012 §4.2 defines) is desired, it needs its own rule
    field (context: §1.10).

---

## 8. SEQUENCING

**Phase A — pre-freeze (blocks the freeze; wire/API-affecting):**
§2.1–§2.5 (API shape, policy-echo, session-ID, handoff trust model), §1.8–§1.10
(ext-mode holes, quorum threshold), §4.5 (multi-round proto), and the upstream filings
(§7) so spec decisions land before the runtime freezes against them.

**Phase B — security & correctness (next release):**
§1.1–§1.7, §1.11, plus their regression tests (§5.4, including the five promoted
`test_gaps.md` tests). §1.12 refiled to Phase D (with §4.1). Design §1.2 and §1.4
together (one sequence contract). Note: the §5.4 tests covering Phase A items
(promote-to-reserved, quorum vector, `policy.default` echo, session-ID, ext-version
replay fixtures) land with Phase A, not here. Effort honesty: Phase B is **3–4 weeks
of work** (§1.5 spans three backends; §1.6 includes an async-auth refactor) — plan
it as such, not as a sprint alongside Phase A.

**Phase C — CI foundation (parallel with B; no code risk):**
§5.1–§5.3, §5.7 steps 1–2 (canonical fixture names + schema), §6 hygiene items.

**Phase D — production hardening:**
§5.6 (recovery benchmarks first — perf baseline), then §3.1 (per-session locking —
largest single change, do it alone), §3.2–§3.4, §4.1 (metrics, starting with §1.12's
rejection counters), §4.3 (disk GC), §5.5 (replay validation — after §4.1 exposes
its counter and §1.4 removes compaction false positives).

**Phase E — features:**
§4.2 (policies dir), §4.4 (roots decision), §4.6 (policy-engine trait + audit
logging, after §2.2), §5.7 steps 3–4 (publish the conformance pack + CI oracle),
§3.5 cleanups as ongoing debt paydown.

**Phase F — ecosystem/tooling (opportunistic):**
§4.7 (transcript visualizer, buf.build schema publishing).

---

## 9. STILL DEFERRED (blocked, not merely unprioritized)

After the 2026-07-03 merge, only items with a hard external blocker remain deferred.
Each keeps (or gets) a one-line entry in `plans/defer/README.md`; the two with
substantial design content keep their plan files:

| Item | Blocker | Plan file kept |
|---|---|---|
| Session composition (parent/child, causal links) | Needs RFC proto changes (`parent_session_id`, `correlation_id`); spec-first per its own doc | `session_composition.md` |
| Workflow primitives (triggers, guards, retry, cross-session deadlines) | Blocked on session composition; needs ≥3 real workflow patterns from users | `workflow_primitives.md` |
| Cross-runtime conformance validation | Requires a second MACP implementation to exist | — (covered by §5.7) |
| Tenant isolation | Requires a multi-tenant deployment + tenant identity in `AuthIdentity` (RFC-0004 §11) | — (noted in §4.6) |
| SQLite / PostgreSQL backends | No demand; file/RocksDB/Redis cover current deployment patterns | — |
| Fixture generator, state-diagram generator, `macp-mode-derive` proc-macro | Code-structure-coupled or high-effort; no community mode-author audience yet | — (subset promoted in §4.7) |
| New experimental modes; WASM/shared-lib mode plugins | Marked obsolete in the defer index (no demand; `ModeFactory` suffices) — unchanged | — |

Multi-node/HA remains explicitly out of scope: the runtime is a single-writer kernel
by design (all authority in one process's memory). If HA demand appears it needs its
own design doc — two runtimes sharing a Redis today would corrupt each other, which
is why §1.5 documents Redis as single-writer-only.
