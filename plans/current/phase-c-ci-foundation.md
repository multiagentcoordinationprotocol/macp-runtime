# Phase C — CI Foundation & Hygiene

**Source:** master plan §5.1–§5.3, §5.7 (steps 1–2), §6 · **Effort:** ~1 week ·
**Runs in parallel with anything** — no runtime code risk.

## Tasks

### C1. Feature-flag matrix (master §5.1)
- `ci.yml`: add matrix legs for `--features rocksdb-backend` and
  `--features redis-backend` (with a `redis:7` service container +
  `MACP_TEST_REDIS_URL`), plus one `--all-features` clippy leg.
- Make Redis tests **fail** (not skip) when `MACP_TEST_REDIS_URL` is set but
  unreachable; keep silent-skip only when unset.
- **Exit:** a compile error in `rocksdb.rs` or a behavioral regression in
  `redis_backend.rs` turns CI red.

### C2. Toolchain policy (master §5.2)
- Main test job on `stable`; separate `cargo check` job pinned 1.89.0 as the MSRV
  gate.
- Move `cargo audit` out of the required `ci-pass` gate → scheduled job + PR
  warning annotation. Consider `cargo-deny` for license/ban policy.

### C3. Gate PRs on tier-1 integration tests (master §5.3)
- New required job: build release binary, run
  `integration_tests` tier-1 (`--test-threads=1`, hermetic). Budget ~5–10 min.
- Tier 2 stays manual; tier 3 stays `#[ignore]`/manual.

### C4. Docker + docs hygiene (master §6)
1. Dockerfile: drop `ENV MACP_ALLOW_INSECURE=1` (**ships with Phase B's B5** —
   compound break) and the unnecessary `COPY tests/ tests/`.
2. `docs/deployment.md`: replace the stale sample Dockerfile; add the backend
   durability matrix (file=fsync'd; rocksdb=configurable after B3;
   redis=non-durable, single-writer); document the `otel` feature.
3. Add `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`.
4. Delete `temp/` (fix `zip-source.sh`'s `[workspace.package]` version parsing
   first if the zip flow is still wanted; otherwise delete it too).
5. CLAUDE.md: dev-auth instructions (post-B5), suspend/resume states in the
   freeze-invariant list, `MACP_CHECKPOINT_INTERVAL` correction (post-B2).

### C5. Conformance groundwork (master §5.7 steps 1–2)
1. Canonical `payload_type` names in `tests/conformance/*.json` +
   `conformance_loader.rs` `encode_payload()` — one atomic PR (CI breaks
   otherwise). Mapping: `decision.Proposal` →
   `macp.modes.decision.v1.ProposalPayload`, …, `Commitment` →
   `macp.v1.CommitmentPayload`.
2. `tests/conformance/schema.json` (draft-2020-12) per the inlined draft in
   master §5.7 (including `Cancelled`/`Suspended` final states and the
   already-implemented optional fields); validate all fixtures against it in CI.

## Exit criteria
- CI red on: feature-gated code breakage, stable-toolchain regression, tier-1
  integration failure, fixture-schema violation.
- Repo carries CHANGELOG/CONTRIBUTING/SECURITY; `temp/` gone.
