# Decision receipts: issue and serve (Wave 5 R1, RFC-MACP-0013 §13)

## Context

Wave 5 R1 makes the runtime issue a *decision receipt* for every session that resolves through a
`CommitmentPayload`, and serve it over two RPCs that macp-proto 0.1.13 adds:
`GetDecisionReceipt{session_id} → {receipt_json}` and `GetReceiptKeys{} → {jwks_json}`. The
runtime also advertises `Capabilities.decision_receipt{get_decision_receipt, signed}` (field 8),
and `macp verify --keys-from <target>` fetches the key set over gRPC.

**Sources, in order of authority:**
- **Normative:** `../multiagentcoordinationprotocol/rfcs/RFC-MACP-0013-commitment-hash.md`
  §13 (lines 195-213) and App. A (223-272), at proto-v0.1.13 = spec `6bb4426`; RFC-0006 §3.9.
- **Brief:** `../multiagentcoordinationprotocol/plans/cross-repo/macp-runtime-wave5.md` §R1.
  Its Seam constraints S-a..S-e are binding.
- Where the spec is stricter than the brief, the spec wins.

**Current state on `feat/decision-receipts`** (all verified):

*Build.* The branch carries the macp-proto 0.1.13 bump (bot PR #261), and the root crate does not
compile. There are exactly two errors:
- `src/server.rs:1044`: E0046, `get_decision_receipt`/`get_receipt_keys` are missing.
- `src/server.rs:1080`: E0063, `decision_receipt` is missing from the `Capabilities` literal.

*Existing receipt code.*
- Receipt **verification** lives in `crates/macp-cli`:
  - `src/jcs.rs` is a hand-rolled RFC 8785 canonicalizer.
  - `src/receipt.rs` has `verify`, `add_jwks`, `TrustedKeys`, and a private `project` and `payload_of`.
  - The crate has a hard `ed25519-dalek = "3"` dependency.
- **No receipt-building code exists anywhere.**
- `macp-cli` depends on `macp-runtime`, so the shared code must move down a layer.

*`Session.resolution`* (`crates/macp-core/src/session.rs:187`, `Option<Vec<u8>>`) is set only by
`apply_mode_response` (`session.rs:518-526`), which copies the bytes unchanged.
- The five standard modes store `env.payload.clone()` verbatim: decision `:304`, proposal `:412`,
  task `:432`, handoff `:682`, quorum `:509`.
- `multi_round` stores a JSON `ResolutionPayload` (`multi_round.rs:234-245`).
- `passthrough` stores the JSON summary `{"action","commitment_id"}` (`passthrough.rs:37-43`).
  Every dynamically registered extension mode runs as passthrough (`mode_registry.rs:516-534`).
- Nothing re-encodes a resolution. So S-d already holds and this plan only pins it with a test,
  and S-e is respected by leaving passthrough alone.

*Persistence.*
- `PersistedSession` is rebuilt from `Session` on every save, recovery included (`main.rs:357`).
  A receipt field on it would be wiped.
- Compaction (`crates/macp-storage/src/storage/compaction.rs:13-50`) replaces only the log. The
  checkpoint's `raw_payload` holds the full `PersistedSession`, including the real
  `semantics_rev`; the `0` at `:38` is only the LogEntry field. `src/replay.rs:35-82` restores it.
- `StorageBackend` (`storage/mod.rs:31-49`) has no compare-and-set primitive.

*Session lookup.*
- `get_session_checked` reads only the in-memory registry (`runtime.rs:1147-1148`).
- `evict_stale_sessions` drops terminal sessions `MACP_SESSION_RETENTION_SECS` (default 3600)
  after `started_at` (`main.rs:546-572`).
- Today, then, every session-scoped RPC returns `SESSION_NOT_FOUND` for an evicted session until
  the next restart.

*Server and error codes.*
- `get_session` (`server.rs:1165-1187`) uses `authenticate_session_access` (`:295-331`).
  `what_if_replay` adds `enforce_rate_limit` (`:1957-1970`). `get_manifest` (`:1391-1416`) does
  no auth call, and `AgentManifest` has no capabilities.
- `MacpError` is `#[non_exhaustive]` (`error.rs:6`), so a new variant is additive.
- `RECEIPT_UNAVAILABLE` (409/`FAILED_PRECONDITION`) is in the spec registry at `6bb4426` but not
  at this repo's pinned `SPEC_REV` `8d20dc5` (`ci.yml:39`). `registry_matches_the_spec_table`
  (`error.rs:428-432`) asserts parity in both directions, so the row and the pin bump must land
  together.

*Version.* The proto bump adds a field to macp-pb's constructible `Capabilities`, so the release is
**0.11.0**. It needs the `semver-breaking` PR label (`scripts/embedder_contract.py:136-150`) and a
`MIGRATING.md` `## 0.11.0` section.

**What the spec overrides or corrects in the brief:**
1. **Issuer.** `runtime:<…or hostname>` would break req 9: a container hostname changes on every
   restart.
2. **Eager issuance.** "Issue at commit" is dropped in favour of lazy issuance, which req 7 already
   describes.
3. **Unavailable cases.** `RECEIPT_UNAVAILABLE` covers every non-RESOLVED state and any failure to
   project, not only OPEN.
4. **Storage failure.** It is `INTERNAL_ERROR`.
5. **JWKS private members.** The JWKS must refuse all eight (`d p q dp dq qi oth k`).
6. **receipt_010 match.** "Byte-for-byte" applies to the JCS body, the JWS string and the hash.
   The vector file itself is written `indent=2`, not as JCS.

## Phases

All phases land in macp-runtime. The plan aims at one PR: P0 is the prerequisite for everything,
and nothing is releasable until the RPCs are real. `/implement` may decide otherwise.

### P0 — macp-proto 0.1.13 compiles; error code; spec pin
- **Status:** DONE (9e72405; pin corrected to `6bb4426` in the P1 commit)
- **Delivers:**
  - The workspace builds and passes tests on macp-proto 0.1.13.
  - Both new RPCs return `UNIMPLEMENTED`, and `Capabilities.decision_receipt` is `None`. This is
    the spec's not-implemented state.
  - `MacpError::ReceiptUnavailable` is added with registry row `RECEIPT_UNAVAILABLE`, in the same
    commit as the `SPEC_REV` bump.
  - `integration_tests/Cargo.lock` is regenerated.
  - `MIGRATING.md` gets its `## 0.11.0` heading, filled in during P5.
  - The PR carries the `semver-breaking` label.
- **Depends on:** —
- **Files:**
  - `src/server.rs`
  - `crates/macp-core/src/error.rs`: the variant, an `error_code` arm, a registry row next to
    `HistoryUnavailable` (`:70-73`), and a coverage-test line
  - `.github/workflows/ci.yml:39` (`SPEC_REV`)
  - the vendored `SOURCE.md` files that record the rev (`tests/conformance/`, `tests/parity/`,
    `tests/vectors/receipt/`)
  - `integration_tests/Cargo.lock`
  - `MIGRATING.md`
- **Approach:**
  - Add stub methods to `impl MacpRuntimeService for MacpServer` (`server.rs:1044`), and set
    `decision_receipt: None` in `initialize` (`:1080`).
  - Bump `SPEC_REV` to exactly `6bb4426` (proto-v0.1.13). Not spec `main`: that adds normative
    behaviour this runtime does not implement yet (RFC-0006 HTTP binding, RFC-0001 §10), and
    spec-drift.yml:358 requires implementing before bumping.
  - Run `git diff 8d20dc5 <rev> -- <vendored source paths>`. If any vendored file changed,
    refresh the copy byte-identically in this phase.
- **Edge cases:** the vendored directories may have drifted between revs; the diff check above
  catches that.
- **Acceptance:**
  - `cargo build --workspace` and `cargo test --workspace` pass.
  - The conformance-oracle `check_dir` passes against the new rev. Run locally on a
    `git archive` export of that rev, per CLAUDE.md.
  - `registry_matches_the_spec_table` passes against the new rev.
  - `cargo metadata --locked --manifest-path integration_tests/Cargo.toml` succeeds.
  - A server test asserts both RPCs return `Code::Unimplemented` and `decision_receipt == None`.
- **Tests:** `receipt_rpcs_unimplemented_before_issuance` (replaced in P4); the existing error
  registry tests.

### P1 — `macp_core::receipt` behind the non-default `receipts` feature
- **Status:** TODO
- **Delivers:** a single shared implementation of JCS, the §3 projection, receipt building,
  Ed25519 JWS sign and verify, key loading and JWKS rendering, in `macp-core` behind `receipts`.
  The CLI uses it.
- **Depends on:** P0, for the macp-cli half only. The macp-core half compiles and tests on its own.
- **Files:**
  - `crates/macp-core/Cargo.toml`:
    - `receipts = ["dep:ed25519-dalek","dep:base64"]`, with ed25519-dalek feature `pem`
      (`zeroize` is already a default feature).
    - `[[test]] name="receipt_vectors" required-features=["receipts"]`.
    - `[package.metadata.docs.rs] features=["receipts"]`.
  - Root `Cargo.toml`: add `ed25519-dalek` to `[workspace.dependencies]`. `base64 = "0.23"`
    already exists at `:70`.
  - New `crates/macp-core/src/receipt/{mod,jcs,build,verify,keys}.rs`.
  - `crates/macp-core/src/lib.rs`.
  - `crates/macp-core/tests/receipt_vectors.rs`, which moves the vector tests out of
    `crates/macp-cli/tests/receipt_vectors.rs`. The stack tests stay in the CLI.
  - `crates/macp-core/tests/fixtures/` (an `openssl genpkey -algorithm ed25519` PEM and JWK
    fixtures).
  - `crates/macp-cli/{Cargo.toml,src/lib.rs,src/main.rs}`; delete `src/jcs.rs` and
    `src/receipt.rs` (re-exported instead).
  - `tests/vectors/receipt/SOURCE.md` (the test path).
  - `.github/workflows/ci.yml`:
    - deps-isolation (`:283-320`): assert no `ed25519` in `cargo tree -p macp-core -e normal`
      or `-p macp-modes -e normal`;
    - features job (`:434`): add `-p macp-core --features receipts` to clippy and test.
  - `docs/stability.md`.
  - `integration_tests/Cargo.lock`. The `pem` feature pulls pkcs8 0.11, der 0.8, spki 0.8 and
    pem-rfc7468 1.0, all new to the lock, so also run `cargo audit`.
- **Approach:**
  - **Why `macp-core` and not a new crate:**
    - Seam already pins it.
    - It owns `commitment_hash` and the pb types.
    - It pulls no tonic, tokio or storage.
    - It needs no release-plz, publish or lockstep changes.
  - **Rejected:** a `macp-receipt` crate (more release plumbing, another Seam pin), and
    `macp-conform` (a tool crate outside the stability contract).
  - **The whole module, JCS included, is behind `receipts`.** The default embeddable API surface
    is then unchanged (S-a), and no ed25519 in the default tree holds trivially.
  - **API.** Records are `#[non_exhaustive]` and are built through constructors.
    - `receipt::jcs::canonicalize(&Value) -> Result<Vec<u8>, NotCanonicalizable>`, moved
      verbatim. It handles integers, which `semantics_rev.rev` needs.
    - `receipt::project(&CommitmentPayload) -> Value`, the §3 projection, now public.
    - `receipt::ReceiptInputs::new(session_id, mode, wire, issuer, issued_at)` with
      `.semantics_rev(implementation, rev)`.
    - `receipt::build(&ReceiptInputs, Option<&ReceiptSigner>) -> Result<Vec<u8>, BuildError>`:
      1. Decode and hash through `commitment_hash::commitment_hash_of_wire`. An unknown field or
         an undecodable payload becomes `BuildError::NotHashable`.
      2. Project.
      3. Set `commitment_wire_b64` as standard **padded** base64 of the exact input bytes. This
         matches the generator at `gen-receipt-vectors.py:181`.
      4. Assemble the body with no `transcript`.
      5. JCS the body and sign it as compact JWS with a detached payload, signing input
         `b64url(header) + "." + b64url(JCS body)`.
      6. Emit JCS of the full receipt with `attestations` set: `[]` without a signer, otherwise
         one `{"type":"jws","jws":…}`. This output is deterministic. It is the issuer's layout
         choice, and those bytes become permanent per session once served.
    - **Header bytes.** They are exactly `{"alg":"EdDSA","kid":<kid as JSON string>}`, written
      in that key order with no other member.
    - `receipt::ReceiptSigner`:
      - `from_pkcs8_pem(pem, kid)`.
      - `from_jwk(&Value, configured_kid: Option<&str>)`. The JWK must be OKP/Ed25519 with `d`,
        and its own `kid` wins. A JWK without `kid` needs the configured one. Two kids that
        differ are an error, and `x`, if present, must match `d`.
      - `kid()` and `public_jwk()`, which yields `{kty:"OKP",crv:"Ed25519",kid,x,alg:"EdDSA",
        use:"sig"}`.
      - A `Debug` impl that redacts the key, which lives in `SigningKey` (zeroize).
    - `receipt::KeySet`:
      - `retired_from_jwks(&Value)` refuses:
        - any of `d p q dp dq qi oth k`;
        - a non-OKP/Ed25519 key;
        - a missing `kid` or `x`;
        - a duplicate kid.
      - `.with_active(&ReceiptSigner)` refuses a retired entry with the active kid and a
        different `x`, and drops an exact duplicate.
      - `.to_jwks_json()`.
    - **Moved verification.** `verify`, `add_jwks`, `TrustedKeys`, `Verification`, `Integrity`,
      `AttestationOutcome`, `Verdict`, `Verdict::as_str` and `FORMAT` move over. Because they
      join macp-core's semver promise and 0.11.0 is breaking anyway:
      - `Verification` and the enums become `#[non_exhaustive]`.
      - `add_jwks` returns a typed `KeyError` instead of `String`. `macp-cli/src/main.rs:300-310`
        only reads `.verdict` and formats the error.
  - **macp-cli** keeps `pub mod jcs` and `pub mod receipt` as re-exports
    (`pub use macp_core::receipt::jcs;`, `pub mod receipt { pub use macp_core::receipt::*; }`),
    enables `macp-core/receipts`, and drops its direct ed25519-dalek dependency.
  - **`issued_at`** is formatted by the caller, so the builder stays pure and deterministic.
- **Edge cases:**
  - An unknown payload field gives `NotHashable`.
  - An empty-but-present `supersedes` projects as `{"commitment_hash":"","session_id":""}`.
  - A `kid` with `"` or `\` is escaped by serde.
  - A PEM that isn't Ed25519 is refused.
  - A JWK `x` that doesn't match `d` is refused.
  - Errors never contain key bytes.
- **Acceptance:**
  1. CI asserts that neither `cargo tree -p macp-core -e normal` nor
     `cargo tree -p macp-modes -e normal` contains `ed25519`.
  2. `cargo test -p macp-core` (no features) and `cargo test -p macp-modes` pass.
  3. `cargo test -p macp-core --features receipts` passes all 12 vendored vectors via `verify`.
  4. **receipt_010 issuance test.** Build from the vector's inputs:
     - wire = base64-decode of its `commitment_wire_b64`;
     - issuer `runtime:example`, `issued_at` `2026-10-24T12:00:00Z`;
     - `semantics_rev` (`macp-runtime`, 4);
     - signer seed = SHA-256(`macp-receipt example key`), kid `example-1`.

     Then assert that the JCS body equals JCS(vector minus `attestations`), that the `jws` string
     equals the vector's, and that `verify` with the vector's `trusted_keys` gives `attested`.
  5. Header bytes for kid `example-1` b64url-encode to
     `eyJhbGciOiJFZERTQSIsImtpZCI6ImV4YW1wbGUtMSJ9`.
  6. `KeySet` and signer tests:
     - each private member is refused, including every
       `schemas/json/tests/invalid-receipt-keys/*.json` case from the spec at the P0 rev,
       vendored as fixtures;
     - a duplicate kid is refused;
     - a kid collision with a different `x` is refused;
     - an empty set renders `{"keys":[]}`;
     - PEM and JWK, each with and without a kid, are loaded or refused as specified.
  7. `cargo test -p macp-cli` passes unchanged (`verify` CLI tests and stack tests).
- **Docs:**
  - `docs/stability.md`: the `receipts` feature on an embeddable crate.
  - `tests/vectors/receipt/SOURCE.md`.

### P2 — Receipt storage, first-writer-wins
- **Status:** TODO
- **Delivers:** each backend persists a receipt beside its session, write-once, and deletes it
  with the session.
- **Depends on:** —. It stores opaque bytes.
- **Files:** `crates/macp-storage/src/storage/{mod,file,memory,rocksdb,redis_backend}.rs` and
  their test modules.
- **Approach:**
  - **New `StorageBackend` methods.** Both have defaults, so third-party implementors still
    compile.
    - `load_receipt(&self, id) -> io::Result<Option<Vec<u8>>>`. Defaults to `Ok(None)`.
    - `store_receipt_if_absent(&self, id, bytes) -> io::Result<ReceiptWrite>`, with
      `#[non_exhaustive] enum ReceiptWrite { Stored, Existing(Vec<u8>) }`. Defaults to
      `Err(ErrorKind::Unsupported)`.
  - **File backend.** The receipt lives at `sessions/<id>/receipt.json`.
    - **Write:** write `receipt.json.<uuid>.tmp`, fsync it, then
      `tokio::fs::hard_link(tmp, receipt.json)`, which is atomic and only one writer can win. On
      `AlreadyExists`, read the file and return `Existing`. In every case remove the tmp and
      fsync the directory.
    - **Rejected:** `create_new`+write leaves a torn file on crash, and rename overwrites.
    - `remove_dir_all` in `delete_session` removes the file.
    - Compaction never touches it.
    - The startup sweep (`storage/recovery.rs:26-48`, run at `main.rs:268`) already matches
      `extension == "tmp"`, so orphaned tmps are cleaned.
    - **Hard-link probe.** `FileBackend::new` is sync, so it probes with `std::fs::hard_link`
      on two uuid-named files in `base_dir` (never under `sessions/`, which `list_session_ids`
      would read as a session) and removes both best-effort; the result is a `hard_links: bool`
      field. Without hard links it logs a `warn!` and the receipt methods return `Unsupported`.
      Any other link error at write time, including `ENOENT` for a GC'd session dir, is `Err`.
  - **RocksDB.**
    - Add a `receipts` column family in `open` (`rocksdb.rs:18-30`).
    - Do get-then-put under a `tokio::Mutex<()>` held **inside** the backend, so first-writer-wins
      is the backend's own guarantee and doesn't depend on the caller.
    - `delete_session` deletes the key.
  - **Redis.**
    - Key `{prefix}:receipt:{id}`, using the same configurable prefix as `{prefix}:session:{id}`
      (`redis_backend.rs:38-48`).
    - Write with `SET NX`, then `GET` on a miss.
    - Add the key to the `delete_session` pipeline (`:97-104`).
    - This is the only true cross-replica first-writer-wins (req 6).
  - **Memory backend:** defaults, because it persists nothing.
- **Edge cases:**
  - **Concurrent writers:** exactly one `Stored`.
  - **Crash before the link:** nothing is stored, and the next fetch re-issues.
  - **Crash after the link:** the orphaned tmp is swept.
  - **Unreadable `receipt.json`:** `Err`.
  - **Redis down:** `Err`.
- **Acceptance (file backend; the same cases for RocksDB and Redis behind their features and
  `MACP_TEST_REDIS_URL`, per `ci.yml:492-503`):**
  1. Two stores with different bytes return `Stored`, then `Existing(first)`.
  2. 16 concurrent stores of distinct bytes return exactly one `Stored`, and every `Existing`
     carries the winner's bytes.
  3. `delete_session` removes the receipt.
  4. `replace_log` leaves the receipt untouched.
  5. A backend using the defaults returns `None` and `Unsupported`.

### P3 — Runtime issuance: config, eligibility, lazy issue-and-persist
- **Status:** TODO
- **Delivers:** `Runtime::decision_receipt(session_id) -> Result<Arc<Vec<u8>>, MacpError>`,
  together with its configuration, the evicted-session read path, metrics, and a recovery loop
  extracted into the library.
- **Depends on:** P0, P1, P2.
- **Files:**
  - root `Cargo.toml` (`macp-core` with `receipts` for the runtime)
  - new `src/receipts.rs`, `src/runtime.rs`, `src/main.rs`, `src/metrics.rs`, `src/lib.rs`
  - new `src/recovery.rs` (the extracted loop)
  - `tests/receipt_issuance.rs`
  - `integration_tests/tests/tier1_protocol/test_startup_gate.rs`
- **Approach:**
  - **Configuration** is validated once in `validate_env_config` (`main.rs:20-127`), the same way
    the TLS paths are. A bad value aborts startup and names the variable, and never echoes key
    bytes.
    - `MACP_RECEIPT_SIGNING_KEY_PATH`: a file holding a PKCS#8 PEM or a private OKP JWK.
    - `MACP_RECEIPT_SIGNING_KEY`: the same content inline. This follows the precedent of
      `MACP_AUTH_TOKENS_JSON` and `MACP_AUTH_JWKS_JSON`, and is meant for platform secrets.
      Setting both this and the `_PATH` form is an error.
    - `MACP_RECEIPT_SIGNING_KID`: required for a PEM. For a JWK, it must agree with the JWK's own
      `kid` if both are present.
    - `MACP_RECEIPT_PUBLIC_KEYS_PATH`: retired public keys, loaded through
      `KeySet::retired_from_jwks` and then `.with_active`.
    - `MACP_RUNTIME_ID`: the issuer becomes `runtime:<id>`.
      - If unset, it defaults to `runtime:macp-runtime`. That is stable (req 9) but not
        per-deployment, so startup logs a `warn!` when a signing key is configured without it.
      - There is no hostname fallback.
  - **Eligibility** is a single function:
    - `state != Resolved` → `ReceiptUnavailable`.
    - The mode is not one of the five standard-track modes, named in an explicit constant list
      → `ReceiptUnavailable`. That covers multi_round, passthrough and `ext.*` (S-e). This rule
      is explicit, not "try decoding".
    - `resolution` is `None` → `ReceiptUnavailable`.
    - Otherwise the source is the `resolution` bytes, which are the verbatim wire (S-d).
  - **Building.** Use `semantics_rev = ("macp-runtime", session.semantics_rev)`, taken from
    session state. That is correct after compaction, because the checkpoint restores it.
    - `issued_at` is `now_ms()` (from the injectable `Clock`), formatted as whole-second UTC
      RFC 3339 with chrono, which the root crate already has.
    - `NotHashable` → `ReceiptUnavailable`.
    - With a key configured, a signing error is `ReceiptUnavailable`, and nothing is persisted.
      An unsigned receipt is never persisted while a key is configured, so `signed=true` holds
      for every receipt the runtime issues.
  - **Lazy-only.**
    - Receipts are issued on the first `decision_receipt` call. Nothing in `process_message`
      changes.
    - **Rejected:** eager spawned issuance after commit. It races `gc_disk_sessions`
      (`runtime.rs:1749`), leaving orphan receipt keys on Redis and RocksDB. It also makes tests
      nondeterministic, and it buys only an earlier `issued_at`, which A.6 calls informational.
    - Lazy is exactly what req 7 describes ("issued only by a later call"), and it keeps issuance
      off the Commitment path entirely.
  - **Evicted sessions.** `Runtime::session_for_read(id)` tries the registry first, then
    `storage.load_session` (already returns `Session`; conversion at `registry.rs:125`). It does
    **not** re-insert the session into the registry. The stored snapshot is saved on resolve
    before compaction, so it is accurate under either compaction setting. Caveats: the resolve
    save is best-effort (`runtime.rs:525-533`), so a failed save leaves the OPEN snapshot and the
    call gives `RECEIPT_UNAVAILABLE` until a restart replays the log (safe direction); and
    `MemoryBackend::load_session` is always `None`, so an evicted memory-only session is
    `SESSION_NOT_FOUND`. Receipts for sessions that came from storage are **not** cached (keeps
    the cache bounded by the registry).
  - **GC vs. issuance.** `gc_disk_sessions` takes the same stripe lock around `delete_session`,
    and issuance re-checks `load_session` under the lock before storing, so no orphan receipt
    key survives GC. The receipt RPC's access check (P4) and the issuance source both use it. The
    general `get_session` is not changed.
  - **Serving.** Hold a fixed array of 64 `tokio::Mutex<()>`, striped by a hash of `session_id`.
    That keeps memory bounded with no pruning. Then:
    1. Return the in-memory cache entry if there is one (`RwLock<HashMap<String,
       Arc<Vec<u8>>>>`, holding only winner or loaded bytes).
    2. Otherwise, return `storage.load_receipt` if it has one, and cache it.
    3. Otherwise, check eligibility, build, and call `store_receipt_if_absent`.
       - `Stored` → serve ours. `Existing(b)` → serve `b`. A losing signed candidate is never
         served (req 6).
       - `Unsupported` → cache only.
    4. Any storage `Err` → `StorageFailed` (`INTERNAL_ERROR`). Count it, and cache nothing.
  - **Non-durable warning.** A defaulted trait method `fn receipts_durable(&self) -> bool
    { false }` (file: its `hard_links` probe result; RocksDB and Redis: `true`). If a signing key
    is configured and the backend is not durable, startup logs one `warn!`. This covers the memory backend, `MACP_MEMORY_ONLY`,
    a hard-link-less volume, and third-party default backends. In those cases a restart can serve
    a different attested receipt.
  - **Cache bounds.** Cache entries are removed in `evict_stale_sessions` (the bytes stay in
    storage) and on GC delete.
  - **Recovery** moves into the library: `main.rs:312-402` becomes
    `pub async fn recover_sessions(…) -> io::Result<RecoveryReport{recovered, replay_mismatches}>`
    in `src/recovery.rs`, and `main.rs` calls it. The mismatch metric hand-off
    (`main.rs:411-419`, after the `Runtime` is built) and the `memory_only` guard stay in main. Recovery
    never calls issuance, which is structural (S-a, req 7). It also lets the tests do real
    restarts.
  - **Metrics.** Per-instance `RuntimeMetrics` fields (`src/metrics.rs:45-49`, following
    `replay_mismatches`), rendered at `main.rs:613`:
    - `macp_receipts_issued_total{signed}`, which counts persisted wins only;
    - `macp_receipt_issue_failures_total`.
  - **Logging.**
    - `info!` on issuance, with `session_id` and `signed` only, never commitment content or
      `reason` (S-c).
    - `warn!` on failure, with `session_id` and the error kind.
    - These logs live in the runtime binary, which S-b allows at info.
- **Edge cases:**
  - **Concurrent first fetches:** the stripe lock plus CAS give one set of bytes.
  - **Evicted session:** loaded from storage.
  - **GC-deleted session:** `SESSION_NOT_FOUND`.
  - **Storage failure:** `INTERNAL_ERROR`, retried on the next call.
  - **Expired, cancelled or suspended session:** `ReceiptUnavailable`.
- **Acceptance:**
  1. A Decision session is resolved with a test signer, and its receipt verifies `attested`.
     `semantics_rev.rev == session.semantics_rev`, and `commitment_wire_b64` decodes to exactly
     `session.resolution`.
  2. Two calls are byte-identical. Rebuilding on the same `FileBackend` via `recover_sessions`
     also gives byte-identical bytes, with compaction both on and off.
  3. 32 concurrent first calls give one distinct value, and that value equals the stored file.
  4. With no signer, `attestations` is `[]` and the receipt verifies `integrity_only`.
  5. Each of these gives `ReceiptUnavailable`:
     - an OPEN, EXPIRED or CANCELLED session;
     - a multi_round or passthrough session resolved by a Commitment;
     - a standard-mode session whose resolution carries an unknown field (crafted test session).
  6. A storage mock that fails only receipt writes (the `runtime.rs:2534-2560` pattern) gives
     `StorageFailed`, and the counter increments. After the mock heals, the next call succeeds.
     The diff adds no receipt code to `process_message`.
  7. Evict with retention 0, then fetch: the bytes equal a pre-eviction fetch, and a first-ever
     fetch after eviction also succeeds.
  8. **Replay equivalence.** `replay_session` over the log gives an equal session (compared as
     `serde_json::to_value(PersistedSession::from(&s))`, since neither type derives
     `PartialEq`), whether or not receipts were issued and with or without a signer.
     `recover_sessions` never writes `receipt.json`.
  9. **S-d pin.** For each of the five modes, `resolution` equals the Commitment envelope's
     payload bytes. Passthrough's resolution is still the JSON summary.
  10. **Startup aborts** (`test_startup_gate.rs`, using `run_expecting_startup_abort` at `:262`).
      Each of these exits non-zero, names the variable, and puts no key bytes in the output:
      - a malformed key file or value;
      - both the PATH and inline forms set;
      - a PEM without a kid;
      - conflicting kids;
      - retired keys with a private member;
      - a kid collision.
- **Docs:** `docs/deployment.md` gets the env vars, `openssl genpkey -algorithm ed25519`, and the
  stability notes for `MACP_RUNTIME_ID`. The CLAUDE.md env table is gitignored here, so update it
  locally only.

### P4 — gRPC surface
- **Status:** TODO
- **Delivers:** real `GetDecisionReceipt` and `GetReceiptKeys`, plus
  `Initialize.capabilities.decision_receipt = {get_decision_receipt: true, signed: <active key
  configured>}`. `GetManifest` is unchanged (`AgentManifest` has no capabilities,
  `server.rs:1406-1415`).
- **Depends on:** P3.
- **Files:** `src/server.rs`.
- **Approach:**
  - **`get_decision_receipt`:**
    - Split `authorize_session_read(&identity, &Session) -> Result<(), Status>` (membership +
      policy engine) out of `authenticate_session_access` (`server.rs:295-331`), which becomes
      authenticate + `get_session_checked` + authorize, so `get_session` and `what_if_replay`
      are unchanged. The receipt RPC is authenticate + `session_for_read` + authorize, then the
      rate limit, in `what_if_replay`'s order (`:1978-1984`).
    - There is no extra `session_id` validation, because `get_session` has none either; an empty
      id gives `SESSION_NOT_FOUND`.
    - Then `enforce_rate_limit(&identity.sender, false)`, as `what_if_replay` does.
    - Then `runtime.decision_receipt`, mapped through `status_from_error`.
    - Return `receipt_json = String::from_utf8(bytes)`. Invalid UTF-8 gives `INTERNAL_ERROR`.
  - **`get_receipt_keys`:**
    - Transport auth only, like `get_manifest`.
    - Returns the startup `KeySet` JSON, precomputed once.
  - **`signed`** means an active signer is configured. Retired keys are published even when no
    signer is active (Open questions 4).
- **Acceptance (`make_server` tests):**
  1. `initialize` gives `{true,false}` with no key and `{true,true}` with a key.
  2. For a resolved Decision session, the initiator, a participant and an observer all get
     identical `receipt_json`.
  3. A non-participant gets `PERMISSION_DENIED` with `FORBIDDEN`, and an unknown session gets
     `NOT_FOUND` with `SESSION_NOT_FOUND`.
  4. An OPEN session gets `FAILED_PRECONDITION` with `RECEIPT_UNAVAILABLE`.
  5. `get_receipt_keys` returns `{"keys":[]}` with no keys, the active JWK when one is configured,
     and active plus retired keys after rotation. No response contains `"d"`.
  6. A rate-limited caller gets `RESOURCE_EXHAUSTED`.
  7. A policy-engine denial gives exactly what `get_session` gives for the same denial
     (`src/policy_engine.rs:57-67`), asserted by calling both.
  8. An evicted session's receipt is served (gRPC-level twin of P3 acceptance 7).

### P5 — `macp verify --keys-from`, gRPC integration tests, docs
- **Status:** TODO
- **Delivers:**
  - The CLI fetches the JWKS over gRPC.
  - The brief's criteria are proven at the real gRPC boundary.
  - Docs and MIGRATING are done.
  - The self-host stack accepts an optional key.
- **Depends on:** P4.
- **Files:**
  - `crates/macp-cli/src/main.rs`, `crates/macp-cli/README.md`
  - `integration_tests/tests/tier1_protocol/test_decision_receipts.rs` and its `mod` line
  - `deploy/docker-compose.yml`, `deploy/.env.example`, and their byte-identical copies in
    `crates/macp-cli/deploy/`
  - `docs/deployment.md`, `docs/API.md`, `MIGRATING.md`, and `README.md` if it lists the RPCs
- **Approach:**
  - **`--keys-from`:**
    - `Cmd::Verify` gains `--keys-from <TARGET>` and `--token` (env `MACP_TOKEN`, default as in
      `replay`).
    - It connects with `macp_conform::client::connect`, calls `get_receipt_keys`, and merges the
      result into `--key` through `add_jwks`.
    - Exit codes don't change. An RPC or connection failure exits 2, naming the status (via
      `tail::status_message`). A key set with no Ed25519 keys also exits 2, as `--key` does.
  - **Compose:** passes `MACP_RECEIPT_SIGNING_KEY`, `MACP_RECEIPT_SIGNING_KID` and
    `MACP_RUNTIME_ID` through from `.env`, defaulting to empty.
    - The runtime must treat an empty value as unset. That needs a P3 test, because compose
      passes empty strings.
    - Run self-host CI.
- **Acceptance (real binary, `ServerManager::start_with_env`), mapped to the brief's criteria:**
  1. With a key, a Decision receipt fetched over gRPC, written to a file and checked with
     `macp verify --keys-from` exits 0 and reports `attested` (brief 1, runtime half).
  2. Repeat fetches are byte-identical, including after a binary restart on the same data dir.
     8 concurrent first fetches give one value (brief 2).
  3. With no key:
     - `signed=false`;
     - `attestations` is `[]`;
     - `macp verify --require integrity` exits 0 and reports `integrity_only`;
     - `GetReceiptKeys` returns `{"keys":[]}` (brief 3).
  4. An OPEN session gives `RECEIPT_UNAVAILABLE`, an unknown session gives `SESSION_NOT_FOUND`,
     and a non-participant gives `FORBIDDEN` (brief 4).
  5. Rotation (brief 6):
     - issue under key A;
     - restart with key B active and A's public JWK in `MACP_RECEIPT_PUBLIC_KEYS_PATH`;
     - the key set lists both;
     - the old receipt is unchanged and verifies `attested` via `--keys-from`;
     - a new session is signed with B.
  6. `--keys-from` against a runtime without receipts exits 2 and names `UNIMPLEMENTED`. With
     `--key` and `--keys-from` together, the key sets merge.

  Brief criteria 2b and 5 are covered in P1 (acceptance 1–2) and P3 (acceptance 8).
- **Docs:**
  - `docs/deployment.md` gets a "Decision receipts" section:
    - the env vars, generating a key, and rotation;
    - `MACP_RUNTIME_ID` stability;
    - the non-durable-storage caveat;
    - the privacy note: a receipt reveals the whole commitment, including `reason` (req 11, A.6).
  - `docs/API.md`: the RPCs and the capability.
  - `MIGRATING.md` 0.11.0:
    - the proto bump;
    - the `StorageBackend` methods, which have defaults;
    - the `macp-core` `receipts` feature;
    - the `macp-cli` re-exports and the typed `add_jwks` error;
    - `#[non_exhaustive]` on the verification types.
  - The CLI README.

### P6 — Hosted key and live verification (post-release)
- **Status:** TODO
- **human-run:** no. Session standing rules allow Railway changes, and no human-held credential is
  involved.
- **BLOCKED-ON:** the 0.11.0 release, which needs the user's explicit approval in-session.
- **Delivers:** the hosted runtime signs receipts, and a hosted receipt verifies `attested`.
- **Approach:**
  1. Move hosted's image to `:0.11`. It auto-updates within `0.10.x` today.
  2. Generate an Ed25519 key in the scratchpad.
  3. Set `MACP_RECEIPT_SIGNING_KEY` (secret), `MACP_RECEIPT_SIGNING_KID=hosted-1` and
     `MACP_RUNTIME_ID=macp-hosted` on Railway, then redeploy.
  4. Delete the local key file. The key is never printed or committed.
  5. Resolve a session, fetch its receipt, and run `macp verify --keys-from`.
- **Rollback:** unset the variables and redeploy, keeping the public key in
  `MACP_RECEIPT_PUBLIC_KEYS_PATH` so receipts already served stay verifiable.
- **Acceptance:** hosted `signed=true`, the hosted receipt verifies `attested`, and the hosted
  JWKS has no private members.

## Long-term posture
- **One-way doors:**
  - (a) The `macp_core::receipt` public API under `receipts`, which falls under the embedder
    semver promise. Records are `#[non_exhaustive]` and built through constructors.
  - (b) The served receipt bytes are permanent per session. The layout (JCS of the full receipt)
    is fixed now and pinned by tests.
  - (c) The `issuer` string. Changing it affects future receipts only.
  - (d) The new `StorageBackend` methods, which are additive with defaults.
- **Debt accepted:**
  - There is no unattested → attested upgrade (the spec's MAY). Sessions resolved before a key was
    configured stay integrity-only. One line in DEFERRED.md.
  - Cross-replica first-writer-wins is real only on Redis. File and RocksDB are single-process
    by construction, which matches today's deployments.
- **Not in scope, reported:** macp-proto is a caret dependency (root `Cargo.toml:67`). Every new
  0.1.x proto breaks a fresh resolve of already-published runtimes (issue #253), and 0.1.13 now
  does this to 0.10.1. That fix is its own change.

## Enterprise concerns
- **Security:**
  - Keys are loaded once and kept in a zeroizing `SigningKey`. They are never logged,
    `Debug`-printed or echoed.
  - The JWKS is public parts only, and retired input is refused if it carries private members.
  - Access is checked exactly as for `GetSession`, and the RPC is rate-limited.
  - A FORBIDDEN caller is refused before any issuance work is done.
- **Reliability:**
  - Issuance is off the Commitment path, so it cannot fail, delay or alter one.
  - A storage failure is `INTERNAL_ERROR` on that read only.
  - The durability caveat for non-durable storage is surfaced at startup.
- **Observability:** the two counters, the issuance and failure logs (ids only), and a startup log
  line saying whether signing is on and under which kid (the kid is public).
- **Rollback:**
  - Unsetting the key gives `signed=false`; stored attested receipts are still served.
  - A 0.10.x binary ignores `receipt.json`, the `receipts` column family, and the `…:receipt:*`
    keys.

## Open questions (decided; `/implement` logs these as UNCONFIRMED)
1. **Issuer:** `runtime:<MACP_RUNTIME_ID>`, defaulting to `runtime:macp-runtime`, with a warning
   when signing without the variable.
   - Hostname rejected: unstable (req 9).
   - A generated id persisted in the data dir rejected: doesn't fit memory-only.
   - Reversible, since it only affects future receipts.
2. **Extension modes:** `ext.*` sessions resolved by a `CommitmentPayload` still get
   `RECEIPT_UNAVAILABLE`, following the brief and S-e under the spec's catch-all. A separate
   verbatim field could lift this later.
3. **No unattested → attested replacement:** the spec's MAY. Reversible.
4. **`{"keys":[]}` only when there are no keys at all:** retired keys stay published when no
   signer is active. This contradicts the literal sentence in req 8 in favour of its
   key-retention rule, so it has been **raised with the spec session**. Implemented as stated
   unless they answer otherwise before P4.
5. **`commitment_wire_b64` and `issued_at` formats:** standard padded base64, as the generator
   does; `issued_at` in whole-second UTC.
6. **Inline `MACP_RECEIPT_SIGNING_KEY`:** an additive extension of the brief, for platform
   secrets.
7. **Lazy-only issuance:** replaces the brief's "issue at commit", for the reasons in P3.
8. **Version:** 0.11.0, forced by the macp-pb struct field.

## Plan review
- **Round 1 (fresh Opus, checked against code and spec): REVISE.** All 23 items were applied:
  - The `SPEC_REV` bump is paired with the error row in P0, because the registry test checks
    parity in both directions.
  - Evicted sessions are read through `session_for_read`, and the recovery loop moves into the
    library.
  - Eager issuance was replaced by lazy-only issuance (GC race, test determinism).
  - **Locking and storage:**
    - a 64-way striped issuance lock;
    - a mutex inside the RocksDB backend;
    - `tokio::fs::hard_link` plus a hard-link probe;
    - a non-durable-storage warning and a bounded cache.
  - **Removed:** the GetManifest capability, the `INVALID_REQUEST` branch, and `dep:chrono`.
  - **Added:**
    - a policy-engine parity test;
    - `#[non_exhaustive]` on the moved types and a typed `add_jwks` error;
    - `[[test]] required-features` and docs.rs features;
    - the invalid-receipt-keys fixtures.
  - **Corrected:**
    - the Redis key prefix, the compaction path and the generator line;
    - where the metrics live, and comparing sessions via serde values;
    - the location of the startup-gate tests;
    - the semver-breaking label and MIGRATING heading in P0;
    - lockfile churn from the `pem` feature.
  - The key-set ambiguity was escalated to the spec session.
- **Round 2 (fresh Opus, changed parts only): REVISE**, 5 edits, all applied: pin exactly
  `6bb4426`; `load_session` shape and the stale-snapshot/memory-only caveats; recovery return
  type and what stays in main; sync hard-link probe in `base_dir`; `receipts_durable()` trait
  method, no caching of storage-loaded receipts, GC takes the stripe lock, and the
  `authorize_session_read` split. The invalid-receipt-keys fixtures carry a top-level
  `_invalid_because`, which the P1 harness ignores. Structure unchanged; plan accepted.
