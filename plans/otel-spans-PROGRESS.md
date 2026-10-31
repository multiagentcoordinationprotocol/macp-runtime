# otel-spans — PROGRESS

Plan: `plans/otel-spans.md`. Branch: `feat/otel-spans`, based on main 611d001 (0.11.0). Spec: semconv at
`0f77753` (`macp-semconv/0.1`). Spec `origin/main` was `554ee9e` (parent `6c6e3a2`) at
revision time; otel files unchanged since `0f77753`. Spec PR #239 (`99f3cfa`) is the source of the `_OTHER` rule only;
the pin stays `0f77753` (99f3cfa also changes the parity contract, out of scope).

## Repo map
- `Cargo.toml`:
  - `:78` the `otel` feature;
  - `:107-118` optional otel deps (opentelemetry 0.32, sdk 0.32.1 in the lock, otlp `grpc-tonic`+`trace` only,
    tracing-opentelemetry 0.33);
  - `:13` crate `exclude` (scripts/ and plans/);
  - no direct `tower` dependency (add an optional `tower` with `util` under `otel` for `option_layer`).
- `src/main.rs`:
  - `:18-127` `validate_env_config` (fatal on failure, so the OTEL protocol/endpoint checks go in
    `telemetry::init` instead, non-fatal);
  - `:190-229` tracing init. The otel arm defaults the endpoint to localhost, sets no resource and no propagator,
    and makes `EnvFilter` global;
  - `:413-418` tonic `Server::builder`, `:475-482` `add_service` and serve (the SERVER span layer goes here);
  - `:500-522` background cleanup loop (expire sweep);
  - `:528-570` hand-rolled Prometheus `/metrics` (append the semconv families at `:546-556`);
  - `:607-612` unbounded provider shutdown.
- `src/server.rs`:
  - `:114-152` `validate_envelope_shape`;
  - `:195-215` `make_error_ack` (`e.to_string()`, which must never reach telemetry);
  - `:257-293` `apply_authenticated_sender` / `authenticate_send_request` (the authenticated sender to record);
  - `:399-516` `process_stream_request`; not-open refusal `:467-476` (`coded_status(InvalidEnvelope,
    "SESSION_NOT_OPEN")`); `:510-514` discards `ProcessResult` (`duplicate` lost);
  - `:885-900` `coded_status` (sets `macp-error-code` from the carried `MacpError`);
  - `:607` `build_stream_session_stream` (`async_stream`, polled outside the handler span);
  - `:1022-1034` `StreamFailure` (`Terminal` loses the `MacpError`);
  - `:1134-1178` `send`, where legacy rejection counters live at `:1163-1168`;
  - `:1204`/`:1272`/`:1340` cancel/suspend/resume RPCs;
  - `:1503-1516` `stream_session`;
  - `:2311-2340` test helpers `make_server`/`do_send`.
- `src/runtime.rs`:
  - `:20-23` `ProcessResult` (public and exhaustive, do not add fields);
  - `:962`, `:1091` synthetic implicit accept calls `record_message_accepted` (no `process` span);
  - `:99-120` `Runtime` fields;
  - `:269-310` `with_registries`;
  - `:313` `with_clock`;
  - `:562-591` `maybe_expire_session` (expire `:581`);
  - `:593-603` `process` dispatch;
  - `:605-835` `process_session_start`: `accept` `:697`, commit point `:758-779`, metric `:802`, guard drop `:815`;
  - `:1002-1140` `process_message`: lock `:1014`, `Precheck::Expired` `:1036-1045`, resolved `:1116-1123`;
  - `:1146-1172` `process_signal`;
  - `:1174-1185` `get_session_checked` (it expires sessions, read RPCs included);
  - cancel `:1335`, metric `:1390`, reason log `:1391`;
  - suspend `:1407`, metric `:1447`, log `:1448`;
  - resume `:1464`, metric `:1543`, log `:1544`, force-expire `:1576`;
  - `:1709-1758` `cleanup_expired_sessions`, expire `:1741`;
  - `:1885` `gc_disk_sessions`;
  - `:1944` `evict_stale_sessions`.
- `src/metrics.rs`:
  - `:43` `MAX_MODE_CARDINALITY`;
  - `:68-126` per-mode recorders;
  - `:241-258` legacy `macp_*_total{mode=}` rendering;
  - `:261` `#[cfg(test)] mod tests`; `:285` `assert_valid_prometheus_line` (private to that module, so the P3
    Prometheus test is a unit test there).
- `src/replay.rs:35-82`: checkpoint replay restores `PersistedSession` from the log. This is why the anchor must not
  live there.
- `src/lib.rs`: module list. Add `pub mod telemetry`.
- `crates/macp-storage/src/registry.rs:14-80`: `PersistedSession` (`#[non_exhaustive]`, also the checkpoint body).
- `crates/macp-storage/src/storage/`:
  - `mod.rs:45-89`: the `StorageBackend` trait and the receipts defaulted-method precedent;
  - `mod.rs:109-150`: `receipt_contract` tests;
  - `file.rs:35,58-67,70,203-210`: `receipt.json`, layout, `atomic_write`, `delete_session`;
  - `rocksdb.rs:10-31`: CFs and `create_missing_column_families` (P5 adds **no** CF);
  - `rocksdb.rs` receipts CF is point-ops only: `delete_session` `:137`, receipts delete `:150-155`,
    `load_receipt` get `:170-178`, `store_receipt_if_absent` put `:180-198`; iterators only on logs/sessions CFs
    (`:70`, `:159`, `:210`, `:247`, `:282`). Anchor key `{id}\x00anchor` goes here;
  - `mod.rs:28-33` `is_storage_safe_session_id` (refuses `\0`);
  - `file.rs:130-143` `load_all_sessions`, `:212-226` `list_session_ids` (directories only, so 0.11 ignores
    `anchor.json`);
  - `redis_backend.rs:42,101-111`: receipt key and delete pipeline (0.11's pipeline does not delete `:anchor:`,
    hence the EX on the anchor key when disk retention > 0);
  - `memory.rs`: no-op.
- `crates/macp-core/src/`:
  - `error.rs:85-110` registry codes (`StorageFailed` → `INTERNAL_ERROR` `:100`);
  - `commitment_hash.rs:193` `commitment_hash_of_wire`;
  - `policy/mod.rs:26-32` `PolicyDefinition.policy_id`;
  - `session.rs:179-181` `Session` (`#[non_exhaustive]`); ~`:387` Open expiry is `now_ms > ttl_expiry`.
- `crates/macp-modes/src/mode_registry.rs:155` `ModeRegistration::descriptor()` (clones; `message_types` for the
  `_OTHER` label set), `:371` `get_mode`; `src/mode/mod.rs:177` `standard_mode_names()` (commitment-hash gate).
- `crates/macp-modes/`: `step.rs` and `replay.rs`, the Seam entry points. They get no telemetry changes. The crate
  already has `tracing` at `Cargo.toml:19`, and `macp-core` has no tracing dependency.
- `.github/workflows/ci.yml`:
  - `:39` `SPEC_REV`;
  - `:283-330` deps-isolation;
  - `:447-493` features job (otel is clippy only, `:491-493`);
  - `:637-669` `check_dir`;
  - `:872-890` docker-build gate.
- `.github/workflows/spec-drift.yml:62-65`: parses `SPEC_REV`.
- `.github/workflows/docker.yml:293`: multi-arch build, no features.
- `Dockerfile`: `cargo build --release`, no build-arg. Uses `rust:1.98` against the `1.96.1` toolchain pin (drift,
  out of scope).
- `tests/conformance/SOURCE.md`: the vendoring template for `tests/otel/SOURCE.md`. `tests/parity/SOURCE.md` and
  `tests/vectors/receipt/SOURCE.md` also state the pin.
- `tests/policies_dry_run.rs`: the `CARGO_BIN_EXE` real-binary test pattern.
- `tests/reflection.rs`: the feature-gated test pattern (`#![cfg(feature = ...)]`).
- `integration_tests/src/server_manager.rs:55-60`: `start`/`start_with_env`. Not needed for otel, because the binary
  tests live in root `tests/`.
- `docs/deployment.md`:
  - `:269` env table, `:300` `MACP_METRICS_ADDR`;
  - `:340` durability matrix;
  - `:456-472` Monitoring;
  - `:474` Container deployment, `:535` published image tags.
- `docs/embedding.md`, `docs/stability.md`, `docs/testing.md:142`, `MIGRATING.md` (embedders only), `CLAUDE.md`
  env and feature tables.
- Spec (`../multiagentcoordinationprotocol`, always read with `git show <rev>:`):
  - `docs/opentelemetry.md`;
  - `schemas/otel/macp-semconv.json`;
  - `rfcs/RFC-MACP-0006-transport-bindings.md` §2.1 (:64-77);
  - `docs/security.md` "Trace Propagation" (:287-294);
  - `scripts/check-sdk-sync.py:28` (the `tests/otel` layout).
- Crate APIs (in `~/.cargo/registry`):
  - `tracing-opentelemetry-0.33.0/src/layer.rs:30-33` special fields;
  - `span_ext.rs:60,98` `set_parent`/`add_link`;
  - `opentelemetry-otlp-0.32.0` features `metrics`, `tls-ring`, `tls-roots`, and env precedence (`lib.rs:315-316`);
    `tonic_types::transport::{ClientTlsConfig, Certificate}` re-export `lib.rs:856`; https channel uses a bare
    `ClientTlsConfig::new()` at `exporter/tonic/mod.rs:323-324`; `with_tls_config` `:763`; no
    `OTEL_EXPORTER_OTLP_CERTIFICATE` support anywhere;
  - `tonic-0.14.6/src/transport/channel/tls.rs:119` `with_enabled_roots`; `channel/service/tls.rs:86` native roots
    load only when enabled;
  - `macp-storage 0.11.0` is published on crates.io (rollback-test dev-dependency candidate);
  - `opentelemetry_sdk-0.32.1` reads `OTEL_TRACES_SAMPLER` (`trace/config.rs`) and resource env
    (`resource/env.rs`).

## Checkpoints

PR strategy: one PR from feat/otel-spans, one commit per phase (like #264). Everything stays inert without both the feature and an endpoint variable.
Risk tiers: P0 simple; P1-P6 complex; P7 simple. Each phase is implemented by a fresh Opus executor subagent, to keep the main thread lean, and verified by a separate fresh Opus verifier.

- 2026-10-31 P0 DONE. PASS in round 1 (Opus verifier, simple phase gated alone because P1 is complex). Files: ci.yml (SPEC_REV 0f77753, check_dir tests/otel, otel test step), spec-drift.yml (watches schemas/otel), tests/otel/{macp-semconv.json,SOURCE.md}, tests/support/otel_semconv.rs, tests/otel_semconv.rs (6 tests), the three SOURCE.md files, docs/testing.md, DECISIONS.md (re-pin entry). Tests: otel_semconv 6/6, --features otel 314/314, fmt and clippy clean on both builds. Next: P1.
- 2026-10-31 P1 DONE. PASS in round 2 (Opus verifier, complex phase gated alone).
  - Round 1: PASS with gaps: shutdown not actually bounded (tokio's blocking pool joins on drop), no failure-path binary tests, private CA failing on hosts without native roots, a vacuous wrong-protocol assertion, endpoint userinfo in logs, no test constructor for an active Telemetry.
  - Round 2: all closed.
  - Files: Cargo.toml/.lock, src/{lib,main,runtime,server}.rs, src/telemetry/{mod,init}.rs, tests/otel_bootstrap.rs, tests/support/fake_collector.rs, tests/golden/policies_dry_run.txt, docs/deployment.md, ASSUMPTIONS.md.
  - Tests: workspace 1100, otel 343, fmt and clippy clean on both builds.
  - Carry to P3: re-assert the wrong-protocol test's "collector receives nothing" with real spans, and add a meter provider to active_for_tests.
  - Next: P2.
- 2026-10-31 P2 DONE. PASS in round 1 (Opus verifier, complex phase gated alone).
  - Before commit, applied the verifier's notes: corrected the timeout limitation (CANCELLED, headers only); added tests for an error code in trailers, status 0 at stream end, and no span events; made the new internals pub(crate).
  - Files: src/telemetry/{mod,grpc}.rs, src/main.rs, tests/otel_grpc.rs, docs/deployment.md, ASSUMPTIONS.md.
  - Tests: workspace 1100, otel 356 (otel_grpc 8).
  - Also deflaked tests/receipt_issuance.rs eviction: a pre-existing same-millisecond race, committed separately.
  - Next: P3.
- 2026-10-31 P3 DONE. PASS in round 1 (Opus verifier, complex phase gated alone).
  - Before commit, applied the verifier's minor notes: replaced the global label-cache mutex with a read-mostly snapshot (new concurrency test), added `error.type=_OTHER` on dropped spans, fixed an ASSUMPTIONS citation, and made the unreachable Inline arm a `debug_assert!`.
  - Carried P1 items are done: the wrong-protocol negative uses real spans, and active_for_tests takes a meter provider.
  - Files: src/telemetry/{process,labels,instruments,mod,init}.rs, src/{metrics,runtime,server,main}.rs, tests/{otel_process,otel_bootstrap,otel_grpc}.rs, docs/deployment.md, ASSUMPTIONS.md.
  - Tests: workspace 1115, otel 383.
  - Next: P4.

