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
- P0 DONE — 9e72405. Stubs + RECEIPT_UNAVAILABLE + SPEC_REV. Pin first set to spec main 122ad47, corrected to 6bb4426 after plan-review round 2 (lands with P1). Verify: workspace tests green; registry parity vs git-archive of 6bb4426 green.
- Plan review: round 1 REVISE (23), round 2 REVISE (5), all applied.
- P1 DONE — e73aaf6. macp_core::receipt (jcs/build/keys/verify) behind `receipts`; CLI re-exports. Verify (fresh Opus, batched with P2): PASS, 1 round.
- P2 DONE — 59530f0. First-writer-wins receipt store on file/rocksdb/redis. Verify: PASS (batched). Minor: file tmp now removed on a failed write too (fixed in P3 commit).
- P3 DONE — 670fa2f, plus a round-2 fix commit. Verify round 1 FAIL: (1) §13(6) on a non-durable backend, where eviction dropped the only copy of a receipt; (2) the eviction/issuance cache race; (3) passthrough, suspended and acceptance items 5/9 untested; (4) doc drift. All fixed: `unpersisted` map kept until the session is unreadable, eviction under the stripe, 3 new tests (14 in total), deployment doc corrected. Minor, accepted: a NotHashable session bumps the failure counter on every call.
- P4 committed d43f85c (verify pending, batched with P5). Divergence: observer and rate-limit coverage is in the P5 integration tests, because SecurityLayer has no test constructors outside macp-auth.
- P4+P5 verify round 1 FAIL: path traversal in GetDecisionReceipt's storage fallback (security). Fixed in 056dee1 plus a follow-up: `is_storage_safe_session_id` for wire ids, a narrower escape check in FileBackend so legacy `\` ids still recover, and alias snapshots treated as unknown. Round 2 PASS (security re-verify). P4 DONE, P5 DONE.
- /reconcile: 8 entries. 6 CONFIRMED, 1 CHANGED (signing requires MACP_RUNTIME_ID), 1 deferred (ext.* receipts). All settled by Opus, none needed the user. DECISIONS.md updated.
- Finalization: workspace tests, clippy -D warnings (workspace + integration_tests), both lockfiles --locked, tier1 136/136, tier1_jwt 9/9, tier2 5/5.

