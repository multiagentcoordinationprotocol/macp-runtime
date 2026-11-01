# Migrating between releases

This file is for **embedders**: systems that use `macp-core`, `macp-modes`,
`macp-policy` and `macp-pb` as libraries, drive `macp_modes::step` and
`macp_modes::replay` themselves, and keep their own history (see
[docs/embedding.md](docs/embedding.md)). Operators of the `macp-runtime` server
should read the "Upgrading to ..." sections of
[docs/deployment.md](docs/deployment.md) instead. `CHANGELOG.md` is generated
from commit subjects and does not carry this detail.

Every release that may break an embeddable crate has an entry here, under a
`## <version>` heading. Before 1.0 that means every minor release
([docs/stability.md](docs/stability.md)). CI requires the entry: a PR that
introduces a breaking API change must add it, and a release PR with a breaking
bump fails without it. Patch releases have no entry.

## 0.13.0 — OpenTelemetry 0.33 and tower-http 0.7 under `otel`

The embeddable crates (`macp-core`, `macp-modes`, `macp-policy`, `macp-pb`) have
no API change. Lockstep versioning moves them to 0.13.0 anyway: the minor bump
comes from `macp-runtime` alone.

- With the `otel` feature, public `macp_runtime::telemetry` signatures now use
  `opentelemetry` / `opentelemetry_sdk` 0.33 and `tower-http` 0.7 types. That
  covers `build_tracer_provider`, `build_meter_provider`, `otel_layer`,
  `Instruments::new`, `ProcessSpan::link_anchor` and the gRPC `ServerLayer`
  family. Callers of these must use the same majors. Without `otel`, nothing
  changes.
- `opentelemetry-otlp` 0.33 retries OTLP/gRPC exports by default, so an
  unreachable collector now gets retried before an export is dropped. Shutdown
  stays bounded at 5 seconds ([docs/deployment.md](docs/deployment.md)).

## 0.12.0 — gRPC compacted resume, spec re-pin, SIGTERM drain, HTTP/JSON binding

### gRPC wire (`macp-runtime` server)

- A `StreamSession` subscribe whose `after_sequence` is below the compacted
  base now **ends the stream** with gRPC `FAILED_PRECONDITION`,
  `macp-error-code: HISTORY_UNAVAILABLE` and `macp-error-details-bin` = the
  UTF-8 JSON text `{"lowest_available_sequence":N}` (RFC-MACP-0006 §3.2). It
  was an inline `INVALID_ENVELOPE` frame with the stream left open. The status
  message keeps the `session history before ordinal {N} was compacted; ...`
  sentence. `MacpError::HistoryUnavailable` is unchanged (still a unit variant);
  the details are built by the server.
- `macp_runtime::policy_engine::require_allow` stamps `macp-error-code:
  FORBIDDEN` on the `PERMISSION_DENIED` it returns. The gRPC code is unchanged.
- `WatchPolicies` now **requires a credential**, like `ListPolicies` and
  `GetPolicy`, whose descriptors it streams (#274). An unauthenticated call is
  refused with `UNAUTHENTICATED` before the stream opens; it previously
  streamed the full policy registry to anyone. Send the same `authorization`
  header you send to `ListPolicies`. `WatchModeRegistry` and `WatchRoots` stay
  open, like `ListModes`/`ListRoots`.

### `macp-runtime` binary: SIGTERM drains

- The binary now drains on SIGTERM as well as SIGINT. SIGTERM used to kill it
  at once; `docker stop` and Kubernetes now give in-flight requests up to
  `MACP_SHUTDOWN_DRAIN_SECS` (default 10) and the runtime persists sessions
  before exiting 0. This includes gRPC-only deployments. Set
  `terminationGracePeriodSeconds` (or `docker stop -t`) above
  `MACP_SHUTDOWN_DRAIN_SECS`, or the platform's SIGKILL ends the drain early.
  A second signal during the drain exits at once (130 or 143). The
  `MACP_METRICS_ADDR` port closes as soon as the first signal arrives. No
  library API changed; the shutdown signal is private to the binary
  (`src/shutdown.rs`). See [docs/deployment.md](docs/deployment.md#graceful-shutdown).

### `macp-runtime`: HTTP/JSON binding (`http-listener` feature, opt-in)

- New non-default Cargo feature `http-listener` on the root `macp-runtime`
  crate: the RFC-MACP-0006 §4 HTTP/JSON binding, a second listener over the
  same `MacpServer`. The embeddable crates (`macp-core`, `macp-modes`,
  `macp-policy`, `macp-pb`) gain no dependency and no API; a default build of
  `macp-runtime` is unchanged.
- At runtime the listener starts only when `MACP_HTTP_BIND_ADDR` is set.
  Unset, it opens no socket and `GetManifest` is byte-identical to a build
  without the feature. A binary **without** the feature now refuses to start
  when `MACP_HTTP_BIND_ADDR` is set, instead of ignoring it.
- Published images after 0.11.0 are built with `otel,http-listener` (the
  `Dockerfile` default changed from empty to `otel,http-listener`; pass
  `--build-arg CARGO_FEATURES=` for the plain binary) and `EXPOSE 8080`.
- `macp_runtime::http` is public so its documentation is published; its
  items are not a stable library API (`MacpServer::with_http_endpoint` is
  `#[doc(hidden)]`). For SDKs the wire contract is the binding itself:
  [docs/API.md](docs/API.md#httpjson-binding). Operators: the variables,
  TLS and reverse-proxy notes are in
  [docs/deployment.md](docs/deployment.md#httpjson-listener).
- `macp-conform` gains an `http` feature and `--transport http`, which drives
  the conformance fixtures through the HTTP binding
  ([crates/macp-conform/README.md](crates/macp-conform/README.md)).

### Fixtures and vectors

- `SPEC_REV` moved to spec `99f3cfa`. `tests/parity/contract.json` lists
  `RECEIPT_UNAVAILABLE` as permanent. The HTTP/JSON binding's examples, error
  schema and negative fixtures are vendored under `tests/http/`.

## 0.11.0 — decision receipts

### `macp-pb` / macp-proto 0.1.13

- macp-proto 0.1.13 adds `Capabilities.decision_receipt`
  (`DecisionReceiptCapability`) and the `GetDecisionReceipt` / `GetReceiptKeys`
  messages. `Capabilities` is a plain prost struct, so a struct literal must
  now set `decision_receipt` (or use `..Default::default()`).

### `macp_core::error`

- New `MacpError::ReceiptUnavailable` (`RECEIPT_UNAVAILABLE`, gRPC
  `FAILED_PRECONDITION`, HTTP 409), in `ERROR_CODE_REGISTRY`. The enum is
  `#[non_exhaustive]`, so a wildcard arm already covers it.

### `macp_core::receipt` (new, `receipts` feature)

- Receipt building, verification and keys moved out of `macp-cli` into
  `macp_core::receipt`, behind the non-default `receipts` feature (it pulls in
  `base64` and `ed25519-dalek`; without it `macp-core` gains no dependency).
  `build(&ReceiptInputs, Option<&ReceiptSigner>)` is pure: same inputs and key,
  same bytes. `ReceiptSigner` loads a PKCS#8 PEM or a private OKP JWK;
  `KeySet` is the published JWKS and refuses private members and duplicate
  `kid`s. Errors are `KeyError` and `BuildError`, both `#[non_exhaustive]`.
- `Integrity`, `AttestationOutcome`, `Verdict` and `Verification` are now
  `#[non_exhaustive]`. Match them with a wildcard arm, and read
  `Verification` fields rather than building one.
- `add_jwks` returns `Result<usize, KeyError>` instead of `Result<usize,
  String>`. Use `.to_string()` (or `{}`) where a `String` was expected.

### `macp-cli`

- `macp_cli::receipt` and `macp_cli::jcs` are re-exports of
  `macp_core::receipt` and `macp_core::receipt::jcs`, so 0.10 paths keep
  resolving. The changes above apply through them. `macp verify` gains
  `--keys-from <target>` and `--token`.

### `macp_storage::StorageBackend`

- Three provided methods, so an existing implementation still compiles:
  `load_receipt` (default `Ok(None)`), `store_receipt_if_absent` (default
  `Unsupported`) and `receipts_durable` (default `false`). On a backend left
  at the defaults the runtime keeps receipts in memory only and warns at
  startup when it signs. To persist them, implement both methods first writer
  wins, never replace a stored receipt, and delete it in `delete_session`.
  The outcome type `ReceiptWrite` is `#[non_exhaustive]`.
- New `is_storage_safe_session_id(&str)`: whether a wire id may key storage
  (one plain path component, at most 255 bytes, no `\`). The runtime checks it
  before any storage read keyed by a request. `FileBackend` itself now
  refuses, with `InvalidInput`, an id that could leave `sessions/`: empty,
  `.`, `..`, or containing `/` or NUL. A third-party backend that keys paths
  by session id should refuse the same.

### `macp-runtime` (as a library)

- `Runtime::with_receipt_config(ReceiptConfig)` and
  `Runtime::with_compact_terminal_logs(bool)` are builders; the default is
  unsigned receipts with issuer `runtime:macp-runtime`. `ReceiptConfig::from_env`
  refuses a signing key without `MACP_RUNTIME_ID`. `decision_receipt`
  and `session_for_read` are new. Session recovery from storage moved into
  `macp_runtime::recovery::recover_sessions`, returning a `#[non_exhaustive]`
  `RecoveryReport`.

### Fixtures and vectors

- `SPEC_REV` moved to spec `6bb4426`. No vendored fixture changed.

## 0.10.0 — what-if replay

Additive only: nothing an embedder calls changes.

### `macp_modes::replay` / `macp_modes::what_if`

- New: `macp_modes::what_if::replay_under_policy(start, events, mode, opts,
  candidate) -> Result<WhatIfReport, MacpError>`. It replays a recorded session
  under a candidate `PolicyDefinition` and reports the would-be outcome and its
  difference from the record. `replay`, `replay_start` and `apply_event` are
  unchanged, so what a record rebuilds to is unchanged. See
  [docs/embedding.md](docs/embedding.md#what-if-replay).

### `macp_policy::PolicyRegistry::validate_candidate` / `WhatIfReplay`

- New: `PolicyRegistry::validate_candidate(&PolicyDefinition) -> Result<(),
  String>`. It applies `register`'s admission to a definition without
  registering it, and skips the `policy_id` checks. Use it on a what-if
  candidate before `replay_under_policy`.
- New RPC `WhatIfReplay` (macp-proto 0.1.12), advertised as
  `PolicyRegistryCapability.what_if_replay`. See
  [docs/API.md](docs/API.md#whatifreplay).
- New env `MACP_COMPACT_TERMINAL_LOGS` (default on). Set it to `0` to keep
  ended sessions' history, which `WhatIfReplay` needs.
- New `MacpError::HistoryUnavailable` (`HISTORY_UNAVAILABLE`, gRPC
  `FAILED_PRECONDITION`, HTTP 409), in `ERROR_CODE_REGISTRY`. The enum is
  `#[non_exhaustive]`, so a wildcard arm already covers it.

### Fixtures and vectors

- `SPEC_REV` moved to spec `8d20dc5`. `tests/parity/contract.json` lists
  `HISTORY_UNAVAILABLE` as permanent. Re-vendor it if you check the contract.

### `macp-cli` (new crate) / `macp_conform::cli`

- New crate `macp-cli`, binary `macp`: `up`/`down`/`status` for the self-host
  stack, `verify` for decision receipts (RFC-MACP-0013 Appendix A.4), and
  `conform`, `session tail`, which follows a session's accepted envelopes,
  and `replay`, which runs what-if replay over a data directory, a log file
  or a running runtime.
  See [crates/macp-cli/README.md](crates/macp-cli/README.md).
- New: `macp_conform::cli::main(prog, args) -> ExitCode` (feature `grpc`), the
  `macp-conform` binary's entry point as a library function. The
  `macp-conform` binary itself is unchanged.
- New: `macp_conform::client` (feature `grpc`), the generated `macp.v1`
  client and its `connect(url)`, which `GrpcTarget` and `macp` share.
- New in `macp-runtime`: `runtime::what_if_over_log(..)`, the what-if replay
  of a log without a running runtime (`Runtime::what_if_replay` now calls
  it), `runtime::recorded_policy(..)`, and `server::what_if::response(run)`,
  which builds the `WhatIfReplayResponse`. `WhatIfRun` and `WhatIfError` are
  public.

## 0.9.0 — library acceptance, step and replay

0.9.0 makes the libraries a complete runtime: `SessionStart` acceptance and
replay moved from the server into `macp-modes`, and the server now calls the
same functions. It also carries the Seam-driven validation changes S1, S4, S5,
S6, S7, S12 and S13 (macp-runtime #228, #229).

### Replay and `semantics_rev`

- **`CURRENT_SEMANTICS_REV` is 4** (was 3). New sessions get three
  tightenings, each refused with `INVALID_ENVELOPE`:
  - Decision `Vote.vote` and `Evaluation.recommendation` must be UPPER_CASE.
    `Objection.severity` must be lowercase and non-empty; an empty severity no
    longer defaults to `"medium"` (S7).
  - Handoff: no new offer to a target that already declined (rule 3a, S5).
  - Task: a `TaskReject` before assignment is the sender's personal decline
    (rule 2a, S6).

  Histories recorded at revisions 0-3 replay under their own rules. Record the
  revision with each session and pass it back through
  `RecordedStart::with_semantics_rev`. If you refuse logs written by a newer
  release, raise your known revision to 4 only after upgrading to 0.9.0. See
  [docs/semantics-revisions.md](docs/semantics-revisions.md).
- **Quorum rule 6 as written (S4), ungated.** A policy `n_of_m` threshold
  above the participant count no longer refuses the `ApprovalRequest`, and the
  session can be declined before any ballot. This reverses #145. It only
  accepts more, so it applies at every revision. A history that 0.8 refused
  to replay for this reason now replays.
- **Replay bindings match live acceptance.** Replay bases the TTL deadline on
  the `SessionStart` envelope's `timestamp_unix_ms`, as acceptance does
  (RFC-MACP-0003 §2). When the payload named no policy, it binds the resolved
  default policy's id. Both were server-replay divergences, fixed in 0.9.0
  only. If you rebuilt sessions through the server's `replay_session`, a
  rebuilt session's `ttl_expiry` and `policy_version` can differ from 0.8.x
  (see [Upgrading to 0.9.0](docs/deployment.md#upgrading-to-090), item 5).
- **New: `macp_modes::replay`.** `replay`, `replay_start` and `apply_event`
  rebuild a session from a `RecordedStart` plus `ReplayEvent`s, with no
  storage. If you had your own replay loop, move to it. It is the code the
  server runs.

### `macp_modes::step`

- `validate_message(session, env, mode)` gains `now_ms`:
  `validate_message(session, env, mode, now_ms)`. Mode dispatch runs at that
  acceptance clock through `Mode::on_message_at`. Never pass the envelope's
  timestamp.
- `StepOutcome` is `#[non_exhaustive]`, and so is its `Accepted` variant.
  `Accepted` gains `synthesized: Vec<Envelope>`: runtime-originated envelopes
  (the handoff implicit accept) that entered history **before** this message.
  A durable embedder appends them first, then the message. Match with
  `StepOutcome::Accepted { state, .. }` and a `_` arm.
- `check_preconditions` also checks the envelope shape
  (`check_envelope_shape`: `macp_version`, non-empty `message_type`,
  `message_id` and `sender`) and that `env.session_id` matches the session. It
  reports `Precheck::Expired` for a `Suspended` session past its suspension cap
  too, through `Session::deadline_passed`. On expiry `step` calls
  `Session::expire()`.
- New building blocks: `admit`, `dispatch`, `advance`, `next_synthetic` with
  `commit_synthetic`, and `MAX_SYNTHETIC_PER_STEP`. A durable embedder uses
  the split form so it can append before it commits; see
  [Durable embedders](docs/embedding.md#durable-embedders).
- New `SessionStart` path: `accept_session_start`, or `validate_session_start`
  then `ValidatedStart::accept`, configured by `StartOptions`. Use it instead
  of building a `Session` yourself: it applies the RFC-MACP-0001 §7.6
  obligations, including policy binding and the session-id rule.
  [docs/embedding.md](docs/embedding.md) maps each obligation to a call.

### `macp-core`

- `MacpError::PolicyDenied { reasons }` holds `Vec<DenyReason>` (was
  `Vec<String>`). Each reason has a stable `code`, a `message` and the
  `participants` it concerns (S13). `MacpError::deny_reasons()` reads them.
- New `MacpError` variants `RegistryReadOnly`, `InvalidRequest`,
  `StreamLagged` and `AgentNotFound`. The enum is `#[non_exhaustive]`, so a
  wildcard arm already covers them. `transport_status()` maps every error to a
  gRPC status.
- Session ids are capped at `session::MAX_SESSION_ID_BYTES` (128) by
  `validate_session_id_for_acceptance`. A custom
  `StartOptions::with_session_id_validator` must not cap below 128.
- New: `commitment_hash::verify_commitment_hash(wire, claimed) ->
  VerifyOutcome`, `commitment_hash_of_wire` and `is_canonical_commitment_hash`
  (RFC-MACP-0013 verification). Verify from the wire bytes, not a decoded
  payload: decoding drops unknown fields.
- New: `session::resolve_max_suspend_ms`, `policy::RuleExtension`.

### `macp-policy`

`PolicyRegistry::register` refuses more. A definition an 0.8 registry admitted
can now fail with `INVALID_POLICY_DEFINITION`:

- an unknown key in a rule object. Rule objects are closed, and the error names
  the key's path. Keys matching `^[_$]` are annotations and stay ignored (S1);
- a top-level `x-<reverse-domain>` vendor key with no `RuleExtension`
  registered for it through `PolicyRegistry::with_rule_extensions` (S1);
- a `schema_version` outside `macp_policy::SUPPORTED_SCHEMA_VERSIONS`
  (`{1, 2, 3}`). 0.8 admitted it and then denied every commitment (S2);
- a rule value outside its schema's domain: `veto_threshold < 1`,
  `minimum_confidence` outside `[0, 1]`, an unknown `commitment.authority`,
  `acceptance.criterion` or `abstention.interpretation`.

A stored, already-bound definition is never revalidated (RFC-MACP-0012 §8), so
replay is unaffected. Re-register your policy files against 0.9.0 before you
upgrade.

### Fixtures and vectors

- `tests/conformance/` follows the spec's newer fixture format
  (`schema.json`), and `SPEC_REV` moved to spec `59ae57d`. New or changed
  fixtures: `decision_reject_paths.json` (case-exact enums),
  `handoff_reject_paths.json` (rule 3a), `task_reject_paths.json` (rule 2a),
  `handoff_implicit_accept.json`, `quorum_unreachable_override.json`,
  `quorum_percentage_override_reachable.json` and
  `session_start_reject_paths.json`. Re-vendor them if you run the corpus.
- `tests/conformance/cmt-hash/` gains verify vectors `cmt_hash_006` to
  `cmt_hash_010` (`wire_hex`, `claimed_hash`, `expected_outcome`). Compute
  vectors 001-005 are unchanged.

### Spec changelog entries with replay exposure

| RFC entry | Change | Replay exposure | 0.9.0 handling |
|-----------|--------|-----------------|----------------|
| RFC-MACP-0007 1.4.0-draft | Case-exact Decision enums, no default severity (S7) | Yes | gated at revision 4 |
| RFC-MACP-0010 1.2.0-draft | Rule 3a: no re-offer to a declined target (S5) | Yes, change (1) | gated at revision 4 |
| RFC-MACP-0009 1.1.0-draft | Rule 2a: pre-assignment personal decline (S6) | Yes, for the tightening | gated at revision 4 |
| RFC-MACP-0004 1.2.0-draft | `session_id` length floor of 128 bytes | Neutral (admission only) | cap at `SessionStart` only |
| RFC-MACP-0012 1.7.0-draft | Vendor `x-` keys, descriptor-schema admission gate, structured `POLICY_DENIED` (S1, S2, S13) | None (admission only) | `register` checks only |
| RFC-MACP-0013 1.1.0-draft | Verification outcomes, vectors 006-010 (S14) | None | new verify API |

RFC-MACP-0001 1.3.0-draft (§7.6 acceptance obligations) is what
[docs/embedding.md](docs/embedding.md) answers. It moves no rule a history
depends on.

---

## Template for a new entry

Copy this above the newest entry. The heading must be exactly `## <version>`,
optionally followed by ` — <title>`, because CI matches it. Leave out sections
with nothing in them.

```markdown
## 0.X.0 — <short title>

<One paragraph: what the release is for, with PR numbers.>

### Replay and `semantics_rev`

- `CURRENT_SEMANTICS_REV` N-1 → N: <each tightening and its gate>. Histories
  at earlier revisions replay unchanged. Add the row to
  docs/semantics-revisions.md.
- <Any change to what `replay` rebuilds, and why it is not a silent change.>

### `macp_modes::step` / `macp_modes::replay`

- <Old signature> → <new signature>. <What the caller changes.>

### `macp-core` / `macp-policy` / `macp-pb`

- <Each break, with the replacement.>

### Fixtures and vectors

- <Conformance fixtures, cmt-hash vectors and the parity contract changed or
  added, and the SPEC_REV they come from.>

### Spec changelog entries with replay exposure

| RFC entry | Change | Replay exposure | handling |
|-----------|--------|-----------------|----------|
```
