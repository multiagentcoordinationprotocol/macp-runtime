# Changelog

All notable changes to `macp-runtime` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the shared
workspace version in the root `Cargo.toml`.

## [Unreleased]

### Security
- **Dev-mode auth is now opt-in**: with no authentication configured
  (`MACP_AUTH_TOKENS_*` / `MACP_AUTH_ISSUER` unset) the runtime refuses to
  start unless `MACP_ALLOW_INSECURE=1`. Previously it silently ran an
  any-bearer-token-is-admin fallback. The published Docker image no longer
  bakes in `MACP_ALLOW_INSECURE=1`; pass it explicitly for local development.
- **HS256 removed from the default JWT algorithm allowlist** (now
  RS256/ES256). Shared-secret deployments must opt in via
  `MACP_AUTH_JWT_ALGS=HS256`.
- `WatchSignals` now requires authentication.
- Handoff implicit-accept timeouts are measured against the runtime's
  acceptance clock on new sessions — a forged (post-dated) envelope timestamp
  can no longer finalize an offer the target never accepted. Existing session
  histories keep their original semantics on replay.
- JWKS fetches have connect/total timeouts, a stale-cache grace window
  (endpoint outages no longer disable all JWT auth at TTL expiry), and
  `kid`-based key selection.

### Fixed
- Passive-subscribe sequence contract (RFC-MACP-0006 §3.2): `after_sequence`
  is now the 1-based accepted-envelope ordinal, exclusive (`0` = from start).
  Previously it was compared inclusively against a raw log index that shifted
  with internal entries. Envelopes accepted during the subscribe window are
  no longer delivered twice; ordinals stay stable across log compaction and
  restart, and resuming below a compacted range returns `FAILED_PRECONDITION`
  instead of silently skipping history.
- `WatchSignals`/`WatchSessions` surface consumer lag as `RESOURCE_EXHAUSTED`
  instead of silently closing; `WatchSessions` no longer duplicates `Created`
  events for sessions present in the initial sync.
- RocksDB log appends are fsynced before acknowledgement (acked implies
  durable, matching the file backend); Redis `replace_log` is atomic; a
  corrupt log entry no longer fails a whole session load on RocksDB/Redis
  (skip + warn, matching the file backend); file snapshot writes fsync before
  rename.
- Quorum policy evaluation conforms to RFC-MACP-0012 §4.2: `threshold` is an
  approval-count bar (as the mode reads it), no longer reinterpreted as a
  participation quorum. Legitimate negative (decline) commitments are no
  longer denied by outcome-blind checks in Quorum/Task/Proposal policy rules.
- A commitment with empty `policy_version` matches the session's bound policy
  (clients that started with an empty policy_version no longer have to echo
  `policy.default`).
- Extension-mode hardening: `PromoteMode` can no longer re-key a mode into
  the reserved `macp.mode.*` namespace; descriptors must declare `Commitment`
  as a terminal type; an ext session started without `mode_version` binds the
  registered descriptor's version (commitments no longer match `""`
  vacuously).
- 36-character base64url session IDs containing `-` are accepted (previously
  mis-routed to UUID validation and rejected).
- Rate-limiter stale-sender cleanup is amortized (full sweep every 128
  requests) instead of scanning all senders on every request.
- Task mode accepts an external orchestrator: `SessionStart` no longer
  requires the initiator in `participants` (RFC-MACP-0009 authorizes
  `TaskRequest` by initiator role, not membership). The pool must still
  contain at least one eligible assignee other than the initiator. Handoff
  keeps requiring initiator membership — intrinsic to the delegated model
  (RFC-MACP-0010 §2), now documented at the check.

### Changed
- `PolicyEvaluator` is now a single-method trait:
  `evaluate_commitment(&CommitmentContext)`. The per-mode methods remain as
  deprecated shims for one release. All five standard modes pass
  `outcome_positive` (previously only Decision did).
- `Session` and other public core types are `#[non_exhaustive]`; construct
  sessions via `Session::builder`.
- CI: main jobs run on stable (1.89 retained as the MSRV check); the
  feature-gated RocksDB/Redis backends are built, linted, and tested (Redis
  against a live service container); the tier-1 gRPC integration suite gates
  pull requests; `cargo audit` is advisory rather than a required gate.

## [0.4.0] — baseline

Workspace split (7 crates), suspend/resume + cancelled lifecycle, policy
registry with schema v2 negative-outcome support, passive subscribe, JWT
bearer auth, RocksDB/Redis backends, checkpoint/compaction replay.
