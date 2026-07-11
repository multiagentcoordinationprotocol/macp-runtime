# Changelog

All notable changes to `macp-runtime` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/); versions follow the shared
workspace version in the root `Cargo.toml`.

## [Unreleased]

## [0.6.1](https://github.com/multiagentcoordinationprotocol/macp-runtime/compare/macp-runtime-v0.5.0...macp-runtime-v0.6.1) - 2026-07-11

### Fixed

- *(release-plz)* stop ignoring the tracked plans/ docs
- green the new CI gates (otel build, rustdoc, coverage)
- derive advertised runtime version from the crate version

### Other

- *(release)* 0.6.0 ([#94](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/94))
- *(dependabot)* weekly -> monthly to cut PR volume ([#90](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/90))
- *(dependabot)* group updates to cut PR volume ([#88](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/88))
- *(deps)* bump rust in the docker group across 1 directory ([#70](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/70))
- *(deps)* bump redis from 0.27.6 to 1.2.4 ([#76](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/76))
- *(deps)* bump reqwest from 0.12.28 to 0.13.4 ([#78](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/78))
- *(dependabot)* ignore breaking major bumps that need migration ([#79](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/79))
- SHA-pin third-party GitHub Actions ([#80](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/80))
- *(deps)* bump docker/login-action from 3 to 4 ([#73](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/73))
- *(deps)* bump arduino/setup-protoc ([#72](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/72))
- *(deps)* bump docker/setup-qemu-action from 3 to 4 ([#71](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/71))
- *(dependabot)* add docker ecosystem for base-image updates ([#69](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/69))
- SHA-pin peter-evans/repository-dispatch (v4.0.1) ([#68](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/68))
- retire WEBSITE_SYNC_TOKEN PAT in notify-website (use macp-deps-bot App token) ([#66](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/66))
- add auto-merge + event-driven bump-proto (cargo) callers ([#64](https://github.com/multiagentcoordinationprotocol/macp-runtime/pull/64))
- automate releases with release-plz (lockstep via version_group)
- refresh for the CI/test overhaul; document Suspend/Resume; fix stale versions
- prune redundant tests, de-flake the harness, fill coverage gaps
- *(release)* semver-checks + changelog gate + GitHub Release; weekly audit; multi-arch image
- pin toolchain to current stable everywhere, harden workflows
- *(deps)* bump crossbeam-epoch 0.9.20, anyhow 1.0.103 (RUSTSEC-2026-0204, RUSTSEC-2026-0190)
- ignore .macp-data/ (local runtime persistence; only its .DS_Store was ignored before)

_Nothing yet._

## [0.6.0] — 2026-07-10

Maintenance release; realigns the runtime with the SDK version line (the
TypeScript SDK advanced to 0.6.0). No changes to the wire protocol, gRPC
surface, or mode semantics — a `Send`/`StreamSession` client built against
0.5.0 interoperates unchanged.

### Added
- Negative-outcome conformance fixtures for the proposal, task, handoff, and
  quorum modes (`task.failed`, `proposal.rejected`, `handoff.declined`,
  `quorum.rejected`), exercising the `TaskFail`, quorum `Reject`/`Abstain`, and
  `HandoffDecline` message types end-to-end.

### Changed
- Releases are now automated with release-plz; all seven crates are pinned to a
  single lockstep version via `version_group`, enforced by a CI guard.

### Fixed
- Green the new CI gates (otel build, rustdoc, coverage) and de-flake the test
  harness; corrected the release tooling's handling of the tracked `plans/`
  docs.

## [0.5.0] — 2026-07-05

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

### Added
- The maximum-suspension cap is session-bound: `SessionStartPayload.max_suspend_ms`
  (macp-proto ≥ 0.1.5) binds a per-session cap at start; `0`/absent selects the
  runtime default (7 days). The **resolved** cap is recorded on the session and
  its SessionStart log entry, and replay uses the recorded value — never live
  configuration (RFC-MACP-0001 §7.5, RFC-MACP-0003 §2). Legacy histories carry
  no recorded cap and keep default-cap semantics. Negative values are rejected
  at SessionStart.

### Changed
- `ext.multi_round.v1` `Contribute` payloads use the canonical protobuf
  encoding (`macp.modes.multi_round.v1.ContributePayload`) — the last
  advertised mode off the canonical wire format. Legacy JSON
  (`{"value":"..."}`) payloads remain accepted (tried first, permanently, so
  pre-proto histories replay byte-identically). Requires `macp-proto` ≥ 0.1.4.
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
