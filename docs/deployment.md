# Deployment Guide

This guide covers everything you need to run the MACP Runtime in production: configuration, storage backends, crash recovery, monitoring, and container deployment. For protocol-level deployment topologies and security requirements, see the [protocol deployment](https://www.multiagentcoordinationprotocol.io/docs/deployment) and [protocol security](https://www.multiagentcoordinationprotocol.io/docs/security) documentation.

## Production checklist

Before exposing the runtime to production traffic, ensure these four items are configured:

1. **TLS certificates** -- Set `MACP_TLS_CERT_PATH` and `MACP_TLS_KEY_PATH` to valid PEM files. The runtime refuses to start without TLS unless `MACP_ALLOW_INSECURE=1` is set.

2. **Authentication** -- Configure at least one of the resolvers. For opaque bearer tokens, create a `tokens.json` mapping tokens to agent identities and set `MACP_AUTH_TOKENS_FILE`. For JWT bearer tokens, set `MACP_AUTH_ISSUER` together with a JWKS source (`MACP_AUTH_JWKS_JSON` inline or `MACP_AUTH_JWKS_URL` fetched + cached). Both can be configured at once -- JWT-shaped tokens are routed to the JWT resolver and opaque tokens to the static resolver. See the [Getting Started guide](getting-started.md) for the token format and JWT claim layout.

3. **Data directory** -- Ensure `MACP_DATA_DIR` points to a directory with write permissions. This is where session logs and snapshots are stored.

4. **Bind address** -- Set `MACP_BIND_ADDR` to the desired listen address. The default `127.0.0.1:50051` only accepts local connections.

## Upgrading into registration-time policy validation

This release tightens what the governance policy registry accepts, what the Quorum mode will bind, and how the Decision evaluator reads a `weighted` round. Six changes are operationally visible, and **item 6 is the only one that can change how an already-stored session replays** -- read it first if you have any persisted Decision session bound to a `weighted` policy. Read this section before upgrading any deployment that sets `MACP_POLICIES_DIR`, or that has persisted sessions bound to a policy with a Quorum `threshold` or a `weighted` `voting.algorithm`. `CHANGELOG.md` is generated from commit subjects and does not carry this detail.

### 1. An invalid policy file now refuses startup

Both routes into the registry -- the `RegisterPolicy` RPC and the `MACP_POLICIES_DIR` preload -- gained value-domain and conditional checks; the enforced set is listed in [Policy](policy.md#what-registration-checks). A file an earlier release accepted may now be out of domain: a fractional Quorum `threshold.value`, `threshold.type: "weighted"`, an unknown `voting.algorithm` or `voting.quorum.type`, a `weighted` algorithm with an empty `weights` map, a `supermajority` `threshold` at or below `0.5`, or a wildcard (`"*"`) policy carrying a Quorum `threshold` that was previously validated against the Decision schema alone and therefore never checked. Loading stops at the first rejection and **startup aborts** -- the preload error is propagated, not logged and skipped.

That is deliberate fail-closed behaviour, and it is why `MACP_POLICIES_DRY_RUN=1` exists. **Run the dry run with the new binary before you upgrade:**

```bash
MACP_POLICIES_DRY_RUN=1 MACP_POLICIES_DIR=/etc/macp/policies macp-runtime
```

It reports every `*.json` file by name as `OK <path> (<policy_id>)` or `REJECTED <path>: <reason>`, prints a checked/rejected count, and exits `0` if the directory would load or `1` if anything in it would be rejected. It binds no port, opens no storage, and replays nothing, so it needs neither TLS nor `MACP_ALLOW_INSECURE=1`. Its output is written to stdout/stderr directly rather than through `tracing`, so no `RUST_LOG` filter can suppress the report. Two things to know: the startup environment-configuration check still runs ahead of it, so an unrelated malformed variable aborts before the report is produced; and a readable directory containing no `*.json` exits `0` with an explicit `WARNING`, because a mis-pointed `MACP_POLICIES_DIR` otherwise looks identical to a clean run.

### 2. Do not unblock startup by deleting the rejected file

When a policy file blocks startup, the natural fix is to delete it. **Correct the file instead.** Deleting it does let the runtime boot, but persisted sessions bound to that `policy_version` are then replayed with the policy unresolved: replay resolves the version best-effort and leaves `policy_definition` empty when it cannot, and commitment enforcement treats an absent policy definition as "no policy to enforce" and returns early. Every in-flight session governed by the deleted policy therefore loses its governance **silently** -- commitments the policy would have denied are accepted, with no error and no log line tying it back to the deletion. The same applies to `UnregisterPolicy` on a policy that live sessions are still bound to.

Note the interaction with the next item: because an unresolved policy makes the Quorum mode fall back to the `ApprovalRequest`'s own `required_approvals` -- a value the mode already constrains to `1..=participants` -- deleting the policy also makes the replay failure below disappear. The two symptoms clear together, and the reason they clear is that the governance bar is no longer being applied.

### 3. A persisted Quorum session with an out-of-domain policy threshold no longer replays

RFC-MACP-0011 §6 makes a policy `threshold` *replace* the `ApprovalRequest`'s `required_approvals`, but nothing previously held the replacement to the same `1..=participants` domain the runtime enforces on the field it replaces. The Quorum mode now refuses an `ApprovalRequest` whose effective threshold falls outside that domain, and replay dispatches the same code -- so such a session fails to replay. It is skipped with a warning, or is fatal at startup under `MACP_STRICT_RECOVERY=1`.

Detect it from the logs. The mode emits, at `WARN`:

```
quorum policy threshold is outside 1..=participants; refusing the ApprovalRequest
  session_id=... policy_id=... effective_threshold=... participants=...
```

naming the session, the bound policy, the computed threshold and the declared participant count -- everything needed to identify which policy to correct. Recovery follows it with `failed to replay session; skipping` carrying the same `session_id`.

**What it takes to reach this.** Not a legacy *policy* -- an **`ApprovalRequest` this runtime accepted before the guard above existed**. Registration is no substitute for the guard, because registration has no participant count to bound the threshold against: `{"type": "n_of_m", "value": 66}` passes every check in [Policy](policy.md#what-registration-checks) under the **new** binary and still trips the guard on a three-participant session. Two classes of threshold reach it, and they are not equally benign:

- **An out-of-domain numeric threshold** -- `n_of_m` or `count` above the declared participant count. The positive outcome was unreachable from the first message, and before this release `commitment_ready` carried no `counted > 0` guard, so `approvals + remaining < required` held with no ballot cast and the coordinator could seal a binding `quorum.rejected` with **zero** approvals (issue #145). That is the condition RFC-MACP-0011 §4a reads as grounds for a decline, reached without a vote. Such a session really was broken: the only outcome it could ever have sealed was a decline nobody cast a ballot for.
- **`threshold.type: "weighted"`, or any unrecognised type.** This class **was working, and it stops replaying.** The old shared fallback arm read *any* unrecognised type as a raw approval count (`_ => rules.threshold.value as u32`), so `{"type": "weighted", "value": 2}` on three participants was a perfectly satisfiable bar of two approvals, and sessions under it sealed legitimate *positive* commitments. The type now resolves to `Unsatisfiable`, the `ApprovalRequest` is refused, and the session no longer loads. Do not read the warning as a report of a session that was already dead.

The second class survives the upgrade through a **checkpoint**, not through the registry. A checkpoint serializes the resolved `policy_definition` inline and `try_replay_from_checkpoint` restores it verbatim without consulting the registry, so an old `weighted` definition is still live even though neither `RegisterPolicy` nor the `MACP_POLICIES_DIR` preload would accept it again. A session with no checkpoint re-resolves its `policy_version` against the live registry during full replay, and there a `weighted` policy file aborts startup at item 1 before recovery ever runs.

**Recovery, for both classes: correct the threshold, do not delete the policy.** Restate a `weighted` or unrecognised type as `n_of_m`, keeping the same `value`. `QuorumThreshold::effective` treats `n_of_m` as a raw approval count, which is exactly what the old fallback arm did, so the bar that session enforced is preserved. (If the old `value` was fractional, registration now refuses it; the old arm truncated, so its floor is the faithful integer.) For a numeric threshold above the participant count, bring it into `1..=participants` -- no value reproduces that session's old behaviour, because its old behaviour *was* the zero-approval decline. Then restart: the append-only log is untouched, so the session was not loaded rather than lost, and it replays normally. Deleting the policy also clears the warning, but for the reason item 2 gives -- the governance bar stops being applied at all.

### 4. A negative weighted total now fails the Decision round

A `weighted` round whose cast weights sum below zero fails the round instead of computing a ratio over a negative denominator. In the approve direction this is a tightening: a round that previously reported `Passed` through an inverted `ratio >= threshold` comparison is now denied. In the decline direction it is **not** a tightening -- on that same round a negative commitment moves from denied to allowed, because a decline over `Passed` was refused while a decline over `Failed` is permitted once the universal reject-floor is satisfied. The case is reachable only from a directly-constructed `PolicyDefinition`, since registration already refuses negative weights. A weighted total of exactly zero is unchanged.

### 5. `voting.threshold: 0.0` and zero `voting.weights` entries are no longer accepted

Spec #99 moved two Decision bounds in `decision-rules.schema.json` from inclusive to exclusive at zero -- `voting.threshold` to `exclusiveMinimum: 0`, and `voting.weights.additionalProperties` to `exclusiveMinimum: 0` with `minProperties: 1` on the map. This runtime mirrors both, and adds the schema's `majority` arm: a `majority` `threshold` below `0.5` is refused, where `supermajority` continues to require one strictly above `0.5`. The asymmetry is deliberate -- the reserved `policy.std.majority` profile sets exactly `0.5`.

A policy file an earlier release accepted may now be refused, and because the `MACP_POLICIES_DIR` preload **aborts startup at the first rejection**, a deployment carrying any of these on disk will fail to start:

- `voting.threshold: 0.0` (it made an all-`REJECT` round return `Passed` under both `majority` and `weighted`)
- a `voting.weights` entry of `0.0`, or a supplied but empty `voting.weights: {}`
- a `majority` `voting.threshold` below `0.5`

**Run the dry run with the new binary before you upgrade** -- it is the same pre-upgrade check item 1 describes:

```bash
MACP_POLICIES_DRY_RUN=1 MACP_POLICIES_DIR=/etc/macp/policies macp-runtime
```

Correcting a zero weight is not a matter of picking a small positive number. The `weights` map **is** the weighted electorate: a participant who should carry no voting weight is expressed by **omission** from the map, never by an explicit `0`. Remove the entry rather than nudging it above zero. A map that would be left empty means no weighted electorate at all, which the `weighted` algorithm cannot express -- choose a different algorithm.

This item affects admission only; no stored session's replay changes, because a descriptor carrying any of these values evaluated the same before and after. Sessions already bound to such a descriptor through a checkpoint keep it, exactly as item 3 describes for the Quorum case.

### 6. The `weights` map is now the weighted electorate, and this one can break a stored session

Under `voting.algorithm: "weighted"`, a declared participant absent from `voting.weights` used to weigh `1.0`. It now weighs `0` and is **non-decisive**: its ballot contributes to neither side of the weighted ratio, does not enter the decisive tally, and does not satisfy the decline guard of RFC-MACP-0007 §6.2. (It still counts toward the `voting.quorum` participation floor -- that carve-out is explicit in RFC-MACP-0012 §4.1 and is unchanged.) The `weights` map **is** the electorate; an observer is expressed by omission, which is also why item 5 refuses an explicit `0`.

**This is the one change in this release that can alter a stored session's replay**, and unlike item 4 it is not confined to a hand-built descriptor. The rule is keyed on nothing -- RFC-MACP-0012 §4.1 makes it "normative for **every** schema version", so it reaches stored `schema_version: 1` and `2` descriptors as well as version 3.

**Blast radius -- two conjuncts, and direction is not one of them.** A stored session is affected when **both** hold:

1. the session is bound to a Decision policy whose `voting.algorithm` is `weighted`, **and**
2. at least one accepted `Vote` was cast by a participant that does **not** appear as a key in that policy's `voting.weights` map, with a vote other than `ABSTAIN`.

That is the whole predicate. It does **not** depend on the committed outcome's direction, on `schema_version`, or on `allow_decline_over_approval`. RFC-MACP-0012 §8's "Bounded exception -- weight-`0` decisiveness" describes a narrower case (a decline over a `Passed` tally under `allow_decline_over_approval: true`), because that is the only configuration the *spec's* earlier text had defined an outcome for. This runtime had defined an outcome for **every** unlisted-voter configuration -- it defaulted the weight to `1.0` -- so its blast radius is wider than §8's corner, and §8's decline-only framing must not be read as this runtime's exposure.

**Worked example -- the positive direction, flipping from accepted to denied.** This is the common shape, not an edge case. Weights `{"agent://a": 1.0}`, participants `a`, `b`, `c`; `b` and `c` cast `APPROVE`, `a` casts `REJECT`; `voting.threshold: 0.5`.

- **Before.** Every voter weighed `1.0`, so the total was `3.0`, the approve share `2.0 / 3.0 = 0.667 >= 0.5`, the voting result `Passed`, and a positive `Commitment` was **accepted into history**.
- **After.** The total decisive weight is `1.0` (only `a` is in the electorate, and `a` rejected), the approve share is `0.0`, the result is `Failed`, and the commitment is **denied**.

That session is authorable today with an ordinary, schema-valid, registered `weighted` policy, simply by omitting two participants from `weights` -- the majority-approves-but-the-weighted-voter-dissents shape, which is the most natural reason to choose `weighted` in the first place.

**The failure mode is not "it replays differently" -- an affected session does not load.** Replay dispatches the same commitment path as acceptance, so a `Commitment` the new rule denies becomes a `POLICY_DENIED` error out of `replay_session` rather than a different outcome. On the default recovery path the session is skipped with one `WARN`:

```
failed to replay session; skipping
  session_id=... error=...
```

and **the session disappears from the registry on restart**, silently apart from that line. Under `MACP_STRICT_RECOVERY=1` the same error is fatal and **the runtime refuses to start**. Those are the two symptoms item 3 describes for the Quorum threshold guard, and the shapes are a pair -- read them together. One difference matters: `validate_replay_consistency` **never runs** for these sessions, because replay errors out before the consistency comparison is reached, so that check cannot be relied on to surface this.

**Pre-upgrade audit query.** Find affected sessions before upgrading. Look for any **Decision** session whose bound policy has `voting.algorithm: "weighted"` and whose accepted `Vote` messages include a sender that does **not** appear as a key in that policy's `voting.weights` map. That is the exact and complete predicate -- it does not depend on outcome direction, on `schema_version`, or on `allow_decline_over_approval`. Sessions matching it may fail to replay after the upgrade.

**Contrast with item 4, which shipped ungated for a reason that does not transfer.** Item 4's negative-weighted-total change was ungated because it was "reachable only from a directly-constructed `PolicyDefinition`, since registration already refuses negative weights". **This one is reachable from an ordinary registered policy**: a `weighted` descriptor that simply omits a declared participant from `weights` passes every admission check, before and after. So do not read item 6 as another item 4.

Why the change is nonetheless right: the old reading let a participant the policy author had explicitly given no voting weight cast ballots that moved the outcome -- in §8's corner, the unilateral power to convert an approving weighted electorate's result into a decline; more commonly, as above, the power to carry a positive round the electorate had rejected. §8 accepts the resulting stored-replay break in writing. The rule itself is in [Policy](policy.md#voting-algorithm-semantics).

**Recovery.** There is no configuration that restores the old reading -- the rule is ungated by design. Correct the policy instead: add to `voting.weights`, with the weight they should carry, the participants who were always meant to vote, and leave genuine observers omitted. A registered policy is re-resolved from the live registry on full replay, so correcting it there is enough for a session with no checkpoint; a session that carries the old descriptor in a **checkpoint** keeps that descriptor verbatim, exactly as item 3 describes for the Quorum case.

Be honest about the case the correction does not cover. If the flipped ballots really were cast by intended observers, no weight map makes that session replay: the commitment in its history was authorized by voters the policy had given no weight, which is precisely the outcome §8 calls unsound. The append-only log is untouched either way -- an affected session was not loaded rather than lost -- so the history remains available for audit while you decide.

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `MACP_BIND_ADDR` | `127.0.0.1:50051` | gRPC listen address |
| `MACP_TLS_CERT_PATH` | -- | TLS certificate PEM (required unless insecure) |
| `MACP_TLS_KEY_PATH` | -- | TLS private key PEM (required unless insecure) |
| `MACP_AUTH_TOKENS_FILE` | -- | Path to bearer token configuration file |
| `MACP_AUTH_TOKENS_JSON` | -- | Inline bearer token config as JSON string |
| `MACP_DATA_DIR` | `.macp-data` | Directory for session persistence |
| `MACP_STORAGE_BACKEND` | `file` | Backend: `file`, `rocksdb`, `redis` |
| `MACP_ROCKSDB_PATH` | `.macp-data/rocksdb` | RocksDB database path |
| `MACP_REDIS_URL` | `redis://127.0.0.1:6379` | Redis connection URL |
| `MACP_MEMORY_ONLY` | off | Set to `1` to disable persistence entirely |
| `MACP_ALLOW_INSECURE` | off | Allow plaintext connections (development only) |
| `MACP_AUTH_ISSUER` | -- | JWT resolver expected `iss` claim (enables JWT auth) |
| `MACP_AUTH_AUDIENCE` | `macp-runtime` | JWT resolver expected `aud` claim |
| `MACP_AUTH_JWKS_JSON` | -- | Inline JWKS document (JSON) for JWT validation |
| `MACP_AUTH_JWKS_URL` | -- | JWKS endpoint URL (fetched + cached) |
| `MACP_AUTH_JWKS_TTL_SECS` | `300` | JWKS cache TTL when fetched from URL |
| `MACP_MAX_PAYLOAD_BYTES` | `1048576` | Maximum envelope payload size in bytes |
| `MACP_SESSION_START_LIMIT_PER_MINUTE` | `60` | Per-sender session creation rate limit |
| `MACP_MESSAGE_LIMIT_PER_MINUTE` | `600` | Per-sender message rate limit |
| `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` | `100` | `ListSessions` page size when the request sends `page_size = 0` |
| `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` | `1000` | Hard cap a requested `ListSessions` `page_size` is clamped to |
| `MACP_CHECKPOINT_INTERVAL` | `0` (disabled) | Log entries between checkpoints |
| `MACP_CLEANUP_INTERVAL_SECS` | `60` | Background TTL cleanup interval in seconds |
| `MACP_SESSION_RETENTION_SECS` | `3600` | Age (from session start) at which terminal sessions are evicted from **memory**; their durable data is kept |
| `MACP_SESSION_DISK_RETENTION_SECS` | `0` (keep forever) | Age (from session start) at which terminal sessions' **durable data** is deleted; `0` disables disk GC entirely |
| `MACP_STRICT_RECOVERY` | off | Set to `1` to fail on any recovery error |
| `MACP_POLICIES_DIR` | -- | Directory of governance policy JSON files preloaded at startup; a file that fails validation aborts startup, and the wire registry becomes read-only |
| `MACP_POLICIES_DRY_RUN` | off | Set to `1` to validate `MACP_POLICIES_DIR` and exit `0`/`1` without starting the server |
| `MACP_POLICY_SCHEMAS_DIR` | -- | **Development and CI only; the server never reads it.** Path to the spec repository's `schemas/json/policy` directory, used by the `enum_lists_match_the_canonical_schemas` parity test -- see the warning below |
| `RUST_LOG` | `info` | Log level filter |

`MACP_POLICY_SCHEMAS_DIR` is listed here because it is otherwise documented nowhere, and a contributor changing a registration mirror needs it. It is read only by `macp-policy`'s parity unit test, which asserts the hand-written value-domain mirrors in `crates/macp-policy/src/registry.rs` still match the canonical schemas. Two warnings:

- **Point it at a clean `git archive` export of the spec commit CI reads, never at a sibling working tree.** CI checks the spec repo out at `main` with no pinned ref, so a sibling checkout that is dirty, or on a local branch ahead of `main`, produces parity failures that do not exist in CI -- and it can move under you mid-session. Export first: `git -C <spec-repo> archive <sha> schemas/ | tar -x -C <tmpdir>`, then point the variable at `<tmpdir>/schemas/json/policy`.
- **A set-but-missing directory panics by design.** Setting the variable asserts the canonical schemas are available, so the test refuses to skip silently. Unset it to fall back to a sibling checkout, or to skip the parity check entirely when no checkout exists.

### Governance policy files

Validate a policies directory before you roll it out: `MACP_POLICIES_DRY_RUN=1 MACP_POLICIES_DIR=/etc/macp/policies macp-runtime` reports every file by name and exits `0`/`1` without starting the server. See [Policy](policy.md#validating-a-policies-directory-before-startup).

**When a rejected policy file blocks startup, correct the file — do not delete it.** Deleting it lets the runtime boot but silently voids governance for every in-flight session bound to that `policy_version`: the policy resolves to nothing on replay and commitment enforcement then treats the session as having no policy at all. `UnregisterPolicy` on a policy live sessions are still bound to does the same. The full mechanism, and the four other operational changes in this release, are in [Upgrading into registration-time policy validation](#upgrading-into-registration-time-policy-validation).

## Storage backends

The runtime supports four storage configurations, selected via `MACP_STORAGE_BACKEND`:

**File backend** (default) stores each session in its own directory under `MACP_DATA_DIR/sessions/<session_id>/`. An append-only `log.jsonl` records every accepted message, and a `session.json` snapshot is written on each state change. Writes use an atomic tmp-file-then-rename pattern to prevent partial-write corruption.

**RocksDB backend** uses an embedded key-value store for higher throughput. Enable it by building with the `rocksdb-backend` Cargo feature and setting `MACP_STORAGE_BACKEND=rocksdb`. The database path defaults to `MACP_ROCKSDB_PATH`.

**Redis backend** stores session data in a remote Redis instance. Enable it with the `redis-backend` feature and set `MACP_STORAGE_BACKEND=redis` with `MACP_REDIS_URL` pointing to your Redis instance.

### Durability matrix

The runtime acknowledges a message only after the log append "commit point".
What that acknowledgement guarantees differs by backend:

| Backend | Acked ⇒ survives process crash | Acked ⇒ survives host power loss | Notes |
|---|---|---|---|
| `file` | yes | yes | log appends `fsync` before ack; snapshots are tmp-fsync-rename atomic |
| `rocksdb` | yes | yes | log appends sync the WAL before ack; session snapshots are async (recovered via replay) |
| `redis` | yes (Redis process survives) | **no** | RPUSH acks in Redis memory; no WAIT/AOF barrier. Cache-tier / single-writer only — the runtime logs a warning at startup |

All backends skip individually corrupt log entries on load (with a warning)
rather than failing the whole session; `MACP_STRICT_RECOVERY=1` makes
recovery-level errors fatal.

**Single-writer**: state authority lives in the runtime process's memory.
Two runtimes must never share one data directory or Redis instance.

### Observation-surface authorization

`GetSession` is participant/observer-scoped, while `ListSessions` and
`WatchSessions` return metadata for **all** sessions to any authenticated
identity (RFC-0006 permits this shape). Deployments with confidentiality
requirements between agent groups should front these RPCs with a proxy or
restrict which identities may call them. `WatchSignals` requires
authentication; `ListModes`/`GetManifest`/`WatchModeRegistry`/`WatchRoots`
are open discovery surfaces by design. `ListSessions` is now paged, and the
decision not to sign its opaque `page_token` rests on exactly the unfiltered
property described here -- a forged cursor can only reposition a caller within
a listing it may already read in full, so if per-caller filtering is ever added
to `ListSessions`, that no-signature decision must be re-analyzed first.

**Memory-only mode** disables persistence entirely. Set `MACP_MEMORY_ONLY=1` for testing or ephemeral workloads. All session data is lost when the process exits.

## Authentication

The runtime applies a pluggable resolver chain assembled at startup:

1. **JWT bearer** (active when `MACP_AUTH_ISSUER` is set) -- validates signature, issuer, audience, and expiration against a JWKS. Supported algorithms: `RS256`, `ES256`, `HS256`. The `sub` claim becomes the sender; an optional `macp_scopes` claim carries capability flags (`allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, `is_observer`).
2. **Static bearer** (active when `MACP_AUTH_TOKENS_FILE` or `MACP_AUTH_TOKENS_JSON` is set) -- looks up opaque tokens in a preloaded identity map. Accepts `Authorization: Bearer <token>` or the alternate `x-macp-token: <token>` header.
3. **Dev-mode fallback** -- activates only when **neither** JWT nor static bearer is configured. Any `Authorization: Bearer <value>` header authenticates the caller as sender `<value>` with full capabilities. Intended strictly for local development.

If a credential matches a resolver but fails verification (expired JWT, unknown static token), the request is rejected with `UNAUTHENTICATED` -- the chain does **not** fall through to a later resolver.

## Crash recovery

When persistence is enabled, the runtime rebuilds all sessions from their append-only logs on startup. This process is fully automatic:

- Each session's `log.jsonl` is replayed through the mode engine to reconstruct the session state.
- If a checkpoint exists, replay starts from the checkpoint and only processes subsequent entries.
- Temporary files (`.tmp` suffixes) left by interrupted atomic writes are cleaned up.
- The number of recovered sessions is logged at startup.

Log append failures are treated as fatal: the runtime rejects the message rather than acknowledging it without a durable record. This ensures the log is always the authoritative source of truth.

If `MACP_STRICT_RECOVERY=1` is set, the runtime exits on any recovery error. Without it, individual session recovery failures are logged as warnings and the remaining sessions are loaded normally.

## Monitoring

The runtime provides operational visibility through several mechanisms:

**Logging** -- All significant events are logged to stderr: session creation, resolution, expiration, recovery results, persistence failures, and rate limit hits. Set `RUST_LOG` to `debug` for detailed request-level logging.

**TTL enforcement** -- Sessions are expired both lazily (on next access) and proactively by a background task running every `MACP_CLEANUP_INTERVAL_SECS`. This ensures expired sessions are cleaned up even if no new messages arrive.

**Session eviction** -- Terminal sessions (resolved, expired, or cancelled) are evicted from memory once their age exceeds `MACP_SESSION_RETENTION_SECS` (default one hour), measured from session **start** rather than from when they became terminal. This is on by default and bounds memory usage. Their data remains on disk and can be replayed if needed. Deleting that durable data is a separate, opt-in step governed by `MACP_SESSION_DISK_RETENTION_SECS`, which defaults to `0` -- disk GC does not run at all unless you set it.

**Log compaction** -- When a session reaches a terminal state, the runtime automatically compacts its log into a single checkpoint entry. This reduces storage footprint for completed sessions.

## Container deployment

Use the repository's `Dockerfile` (multi-stage, non-root `macp` user). The
image does **not** enable dev mode: the runtime refuses to start without
configured authentication and TLS unless you explicitly opt in for local
development:

```bash
# Production: configure auth + TLS
docker run -p 50051:50051 \
  -e MACP_AUTH_TOKENS_FILE=/etc/macp/tokens.json \
  -e MACP_TLS_CERT_PATH=/etc/macp/tls.crt -e MACP_TLS_KEY_PATH=/etc/macp/tls.key \
  -v ./secrets:/etc/macp macp-runtime

# Local development ONLY: any bearer token becomes a fully-privileged identity
docker run -p 50051:50051 -e MACP_ALLOW_INSECURE=1 macp-runtime
```

When deploying in containers:

- Mount a persistent volume at `MACP_DATA_DIR` so session logs survive container restarts.
- Expose port 50051 (or the port configured via `MACP_BIND_ADDR`).
- Provide TLS certificates and auth tokens via mounted secrets.
- Set `MACP_BIND_ADDR=0.0.0.0:50051` to accept connections from outside the container.

## Development tools

For development and CI, these additional tools are useful:

- `cargo-tarpaulin` for coverage reporting
- `cargo-audit` for dependency security auditing
- `buf` for protocol buffer linting and management
