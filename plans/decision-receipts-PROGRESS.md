# decision-receipts — PROGRESS

Plan: `plans/decision-receipts.md`. Branch: `feat/decision-receipts`. It is rebased on main, and its first commit is the macp-proto 0.1.13 bump taken from bot PR #261.

## Repo map
- `src/server.rs`:
  - `:295-331` `authenticate_session_access`
  - `:859-892` `status_from_error`/`coded_status`
  - `:1044` `impl MacpRuntimeService`
  - `:1080-1113` `initialize` capabilities
  - `:1165-1187` `get_session`
  - `:1391-1416` `get_manifest` (no auth)
  - `:1957-1970` `what_if_replay` rate limit
  - `:2244-2270` test helpers `make_server`/`do_send`
- `src/runtime.rs`:
  - `:975` `process_message` (session lock `:987`, append `:1053`, `step::commit` `:1062`, Resolved `:1089-1097`, save `:1100`, compaction `:1101-1104`, publish `:1108`)
  - `:315` `now_ms`
  - `:1749` `gc_disk_sessions`
  - `:2534-2560` failing-storage mock
- `src/main.rs`:
  - `:20-127` `validate_env_config`
  - `:229-243` startup abort
  - `:312-402` recovery loop
  - `:475-488` TLS file read
  - `:613` Prometheus render
- `src/replay.rs:35-82`: checkpoint replay that restores `semantics_rev`.
- `src/metrics.rs`: global counter pattern (`replay_mismatches` `:49,144-150`).
- `crates/macp-core/src/session.rs`:
  - `:187` `resolution: Option<Vec<u8>>`
  - `:518-526` `apply_mode_response`
  - `:225` `semantics_rev`
  - `:150` `CURRENT_SEMANTICS_REV = 4`
- `crates/macp-core/src/error.rs`:
  - `:6` `#[non_exhaustive] MacpError`
  - `:70-73` `HistoryUnavailable` template
  - `:179-224` registry
  - `:263-300` coverage test
- `crates/macp-core/src/commitment_hash.rs:69,193`: `commitment_hash`, `commitment_hash_of_wire`.
- `crates/macp-modes/src/mode/*.rs`: `resolution` set at decision `:304`, proposal `:412`, task `:432`, handoff `:682`, quorum `:509` (verbatim), multi_round `:234-245` (JSON), passthrough `:37-43` (JSON summary).
- `crates/macp-storage/src/`:
  - `storage/mod.rs:31-49`: the `StorageBackend` trait
  - `storage/file.rs:34-48`: `atomic_write`
  - `storage/recovery.rs:26-48`: tmp sweep
  - `storage/rocksdb.rs:18-30`: CF open
  - `storage/redis_backend.rs:97-104`: delete pipeline
  - `storage/memory.rs`: no-op
  - `compaction.rs:13-50`
  - `registry.rs:34-80`: `PersistedSession`
- `crates/macp-cli/src/`:
  - `jcs.rs` (canonicalize)
  - `receipt.rs` (`verify`, `add_jwks`, `TrustedKeys`, private `project`/`payload_of`)
  - `main.rs:45-58` `Cmd::Verify`, `:139` `Require`, `:248-308` `verify()`, `:310` `add_key`, `:83-93` the replay target/token pattern, `:387` `runtime()`
  - `replay.rs:186-190`: the bearer pattern
- `crates/macp-cli/tests/receipt_vectors.rs` (12 vectors and stack tests); `tests/vectors/receipt/` and `SOURCE.md` (vendored at the spec rev, checked by the conformance-oracle `check_dir`).
- `crates/macp-conform/src/client.rs:16`: `connect(url)`.
- `.github/workflows/ci.yml`: `:283-320` deps-isolation, `:328-358` lockstep, `:434` features job.
- `integration_tests/src/server_manager.rs:60`: `start_with_env`. Template: `tests/tier1_protocol/test_what_if_replay.rs:99-142`.
- Spec, at `../multiagentcoordinationprotocol`:
  - `rfcs/RFC-MACP-0013-commitment-hash.md` §13 (:195-213), App. A (:223-272)
  - `schemas/json/macp-decision-receipt.schema.json`, `macp-receipt-keys.schema.json`
  - `schemas/conformance/receipt/receipt_0NN_*.json`
  - `scripts/gen-receipt-vectors.py`
  - `registries/error-codes.md:34`
  - `examples/discovery/receipt_keys.json`

## Phase log
