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
