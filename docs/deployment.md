# Deployment Guide

This guide covers everything you need to run the MACP Runtime in production: configuration, storage backends, crash recovery, monitoring, and container deployment. For protocol-level deployment topologies and security requirements, see the [protocol deployment](https://www.multiagentcoordinationprotocol.io/docs/deployment) and [protocol security](https://www.multiagentcoordinationprotocol.io/docs/security) documentation.

## Production checklist

Before exposing the runtime to production traffic, ensure these four items are configured:

1. **TLS certificates** -- Set `MACP_TLS_CERT_PATH` and `MACP_TLS_KEY_PATH` to valid PEM files. The runtime refuses to start without TLS unless `MACP_ALLOW_INSECURE=1` is set.

2. **Authentication** -- Configure at least one of the resolvers. For opaque bearer tokens, create a `tokens.json` mapping tokens to agent identities and set `MACP_AUTH_TOKENS_FILE`. For JWT bearer tokens, set `MACP_AUTH_ISSUER` together with a JWKS source (`MACP_AUTH_JWKS_JSON` inline or `MACP_AUTH_JWKS_URL` fetched + cached). Both can be configured at once -- JWT-shaped tokens are routed to the JWT resolver and opaque tokens to the static resolver. See the [Getting Started guide](getting-started.md) for the token format and JWT claim layout.

3. **Data directory** -- Ensure `MACP_DATA_DIR` points to a directory with write permissions. This is where session logs and snapshots are stored.

4. **Bind address** -- Set `MACP_BIND_ADDR` to the desired listen address. The default `127.0.0.1:50051` only accepts local connections.

## Upgrading to the release after 0.11.0 (OpenTelemetry)

This release adds OpenTelemetry traces and metrics (`macp-semconv/0.1`, experimental). Nothing about sessions, messages, replay or recovery behaves differently, with or without telemetry. Items 3, 4, 5 and 7 affect only a binary built with `--features otel`; published images up to and including `0.11.0` are plain builds, and images published after it include `otel` (see [Container deployment](#container-deployment)) and export nothing until an endpoint is set.

1. **New `/metrics` families, in every build.** With `MACP_METRICS_ADDR` set, `/metrics` also renders `macp_envelope_processed_total`, `macp_envelope_process_duration_seconds_{bucket,sum,count}`, `macp_session_transitions_total` and `macp_session_duration_seconds_{bucket,sum,count}`, after every existing line. Each appears only after its first observation. Labels are bounded (2,000 label sets, then `_OTHER`) and never carry a session, message or sender id. See [Monitoring](#monitoring).
2. **The per-mode `macp_*_total{mode="..."}` families are deprecated.** `macp_sessions_started_total`, `macp_messages_accepted_total` and the rest are unchanged, byte for byte, until they are removed no earlier than the next minor release after the one that introduces their replacements (0.12.0 if that release is 0.11.x, 0.13.0 if it is 0.12.0). Move dashboards and alerts to the families in item 1.
3. **An `otel` build no longer exports by default.** It used to export traces to `http://localhost:4317` whenever `OTEL_EXPORTER_OTLP_ENDPOINT` was unset; it now exports nothing until an endpoint variable is set. Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317` to keep the old behaviour. Also: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` now wins over the generic variable and turns telemetry on by itself; `service.name` is `macp-runtime` (was `unknown_service`) unless `OTEL_SERVICE_NAME` or `OTEL_RESOURCE_ATTRIBUTES` names one; and `RUST_LOG` no longer filters telemetry. See [OpenTelemetry](#opentelemetry).
4. **New storage records** (`otel` build with telemetry on only). Each session's trace anchor is written to `sessions/<id>/anchor.json` (`file`), key `<id>\0anchor` in the existing `receipts` column family (`rocksdb`, no new column family), or `<prefix>:anchor:<id>` (`redis`). Nothing is written with telemetry off or with `MACP_MEMORY_ONLY=1`. See "Trace anchor" under [the durability matrix](#durability-matrix).
5. **Rollback is safe.** A 0.11.0 binary ignores all three records and reads the same sessions and logs. Its disk GC (`MACP_SESSION_DISK_RETENTION_SECS > 0`) deletes a `file` anchor with the session directory, but leaves the `redis` and `rocksdb` keys behind: a `redis` anchor key expires by itself (its expiry is set whenever retention is above 0); a `rocksdb` orphan stays until a newer binary with retention above 0 starts, which sweeps orphans once after recovery, off the serving path. With retention 0 nothing is ever GC'd, so nothing is orphaned.
6. **Two counters now differ by design.** `macp_messages_accepted_total` counts the runtime's handoff synthetic implicit accepts (RFC-MACP-0010 §5.1(2)); `macp_envelope_processed_total{macp_envelope_outcome="accepted"}` does not, because no Envelope is received. Do not alert on the two being unequal.
7. **A telemetry misconfiguration never stops the runtime.** An unsupported `OTEL_EXPORTER_OTLP_PROTOCOL` (anything but `grpc`, including the `_TRACES_`/`_METRICS_` variants), an unreadable `OTEL_EXPORTER_OTLP_CERTIFICATE` (or a per-signal variant), or an invalid endpoint logs one `ERROR` line naming the variable and leaves telemetry off; the runtime serves normally.
8. **A `StreamSession` resume below the compacted base now ends the stream** (RFC-MACP-0006 §3.2), with gRPC `FAILED_PRECONDITION`, `macp-error-code: HISTORY_UNAVAILABLE` and `macp-error-details-bin` = `{"lowest_available_sequence":N}`. It was an inline `INVALID_ENVELOPE` frame on a stream left open. The status message keeps the sentence `session history before ordinal N was compacted; ...`, so a client matching that text on a terminal error keeps working; a client that waited for the inline frame must now handle the status and resume on a new stream. Separately, an installed ingress policy engine's `PERMISSION_DENIED` now carries `macp-error-code: FORBIDDEN` (gRPC code unchanged).
9. **SIGTERM now drains, as SIGINT always did.** The runtime used to drain only on SIGINT (ctrl-c); SIGTERM, which `docker stop` and Kubernetes send, killed it at once with no drain and no final session snapshot. Both signals now stop new connections, close the `MACP_METRICS_ADDR` port, let in-flight requests finish for up to `MACP_SHUTDOWN_DRAIN_SECS` (default 10), persist sessions and exit 0. A second signal during the drain exits at once (code 130 for SIGINT, 143 for SIGTERM) without the snapshot. This applies to gRPC-only deployments too: set Kubernetes `terminationGracePeriodSeconds` (or `docker stop -t`) **above** `MACP_SHUTDOWN_DRAIN_SECS`, plus 5 seconds with telemetry on, or the platform's SIGKILL cuts the drain short. See [Graceful shutdown](#graceful-shutdown).
10. **An opt-in HTTP/JSON listener** (RFC-MACP-0006 §4), compiled with the `http-listener` Cargo feature and included in images published after `0.11.0`. Nothing changes unless `MACP_HTTP_BIND_ADDR` is set: no socket, no task, no log line, and `GetManifest` is byte-identical. A binary built without the feature refuses to start when `MACP_HTTP_BIND_ADDR` is set, rather than ignoring it. See [HTTP/JSON listener](#httpjson-listener).
11. **`WatchPolicies` requires a credential** (#274), like `ListPolicies`/`GetPolicy`; an unauthenticated call is refused with `UNAUTHENTICATED` before the stream opens.

## Upgrading to 0.11.0

0.11.0 adds decision receipts (RFC-MACP-0013 §13). Nothing changes for a deployment that sets none of the receipt variables: it serves unsigned receipts with issuer `runtime:macp-runtime`, and no message, replay or recovery path behaves differently.

1. **New RPCs.** `GetDecisionReceipt` and `GetReceiptKeys` are served, and `Initialize` advertises `decision_receipt`. A proxy or gateway that allow-lists `macp.v1` methods must let both through. `GetReceiptKeys` needs no credentials.
2. **New startup aborts.** The runtime refuses to start on a malformed or unreadable receipt key, on both key forms set, on `MACP_RECEIPT_SIGNING_KID` without a key, on a bad `MACP_RECEIPT_PUBLIC_KEYS_PATH`, and on a signing key without `MACP_RUNTIME_ID`. Unset or empty variables are not an error.
3. **To sign**, set a key, its `kid` and a stable `MACP_RUNTIME_ID`, as described in [Decision receipts](#decision-receipts). Choose `MACP_RUNTIME_ID` once: it is the `issuer` of every receipt and must not change across restarts.
4. **Disclosure.** A receipt carries the whole resolving Commitment, including `reason`, and any reader of the session can fetch it and pass it on.
5. **Storage.** The `file` backend writes `sessions/<id>/receipt.json` on the first fetch. On a file system without hard links, receipts live only in memory, and the runtime warns at startup when it signs there.

## Upgrading to 0.9.0

### 1. Quorum: an over-participant policy threshold is accepted and declinable at once

A deliberate reversal of issue #145, made to follow RFC-MACP-0011 §5 rule 6 (Seam finding S4; owner decision 2026-10-16). A policy `threshold` above the declared participant count -- `{"type": "n_of_m", "value": 5}` on three participants -- no longer refuses the `ApprovalRequest`, and the `counted > 0` gate is gone, so the coordinator may seal `quorum.rejected` before any ballot. A `percentage` threshold can never exceed the pool (100% of 3 is 3), so it is unaffected: a negative commitment there still waits for the ballots that make the bar unreachable. The positive outcome under an over-pool bar remains impossible.

Who it reaches: only sessions bound to a policy whose `n_of_m`/`count` value exceeds the session's participant count. Persisted sessions of that kind that 0.8 skipped at replay (item 3 of the next section) now **load again** -- and are immediately declinable. If such a session must not be declined without a vote, correct the policy before upgrading. An `Unsatisfiable` threshold (unrecognised `type`) is still refused at `ApprovalRequest`.

### 2. Revision 4: three validation tightenings for new sessions

Sessions started by 0.9.0 are bound to `semantics_rev` 4. A session started
by an older release keeps its revision across the upgrade, so it replays
and continues under the rules it was accepted under. Only new sessions see
the following, and each refusal is `INVALID_ENVELOPE`:

- **Decision (Seam S7, RFC-MACP-0007 §4).** `Vote.vote` and
  `Evaluation.recommendation` must be UPPER_CASE, exactly as written
  (`APPROVE`, not `approve`). `Objection.severity` must be lowercase and
  non-empty; it no longer defaults to `medium`. **Clients that send
  lowercase votes must change before they open new sessions.**
- **Handoff (Seam S5, RFC-MACP-0010 §5 rule 3a).** After a target declines
  an offer, the owner can't offer that target anything else in the same
  Session. Offer to a different participant, or commit negatively.
- **Task (Seam S6, RFC-MACP-0009 §5 rule 2a).** A `TaskReject` sent before
  anyone is assigned declines the task for that participant only:
  - That participant can never `TaskAccept` the task.
  - Other participants can still accept it.
    `assignment.allow_reassignment_on_reject` now applies only to a reject
    from the *active assignee*. Before 0.9.0, a decline from anyone blocked
    every later accept unless the policy allowed reassignment.
  - If the task names a `requested_assignee` and that participant declines,
    no one is left who can take it. The initiator may then send a negative
    `Commitment` without a terminal report.

### 3. Every gRPC `Status` names its registry code

Control-plane RPCs that used to return a bare status (unknown session, read-only policy registry, bad `ListSessions` paging, stream lag, unknown manifest agent) now carry `macp-error-code` metadata like `Send` does. The gRPC status code itself did not change for any of them, so a client that branches on the status keeps working. Four codes are new, added to the spec registry in spec #195 and permanent since spec #196: `REGISTRY_READ_ONLY`, `INVALID_REQUEST`, `STREAM_LAGGED` and `AGENT_NOT_FOUND`. See the "Control-plane codes" table in [the SDK guide](sdk-guide.md). Alerting that groups on `macp-error-code` sees these codes where it used to see none.

### 4. `session_id` is capped at 128 bytes

`SessionStart` now refuses a `session_id` longer than 128 bytes with `INVALID_SESSION_ID`. That is the RFC-MACP-0004 §11.2 floor (spec #194): every conformant runtime must accept ids up to 128 bytes, so ids within the cap are portable. A persisted session with a longer id still replays, because the cap applies only at admission. UUIDs (36 chars) are unaffected.

### 5. Replay now restores the TTL deadline and default policy that the live runtime bound

Up to 0.8.x, replay could rebuild a session from its log with two bindings that differed from what the live runtime had set at `SessionStart`. Startup recovery serves the replayed session and saves it over the snapshot, so these values changed on every restart:

- **The TTL deadline.** Live acceptance computes it as the `SessionStart` envelope's `timestamp_unix_ms` plus `ttl_ms`, as RFC-MACP-0003 §2 requires. Replay used the runtime's own acceptance clock instead. When the client's clock disagreed with the runtime's, a restarted session expired that much earlier or later than it would have without the restart.
- **The default policy.** When `policy_version` is empty, live acceptance binds `policy.default` and stores that definition (RFC-MACP-0012 §6.1). Replay left `policy_version` empty and stored no definition. So after a restart, `GetSession` reported an empty policy.

Both now replay to the live values. On the first boot after upgrading, a session that a 0.8.x runtime had already restarted may log a single warning like this, and it does not recur:

```
WARN replay/snapshot ttl_expiry mismatch
```

This is the same benign pattern as the `suspension_intervals` warning described below. The snapshot holds the shifted deadline, while replay restores the original one, and the original is the deadline the runtime then serves. The `ttl_expiry` field is new to the consistency check (`validate_replay_consistency`), and it is why the divergence was invisible until now.

The replay logic itself now lives in the library, as `macp_modes::replay`, and the server's log replay is a thin mapping onto it. An embedder that drives sessions through `macp_modes::step` and records the start and each event gets the same rebuild the server performs. See `docs/embedding.md`.

## Upgrading into registration-time policy validation

This release tightens what the governance policy registry accepts, what the Quorum mode will bind, how the Decision evaluator reads a `weighted` round, when a decline may be finalized over a passing vote, and how a `percentage` Quorum threshold is computed. Ten changes are operationally visible -- seven tightenings, one relaxation (item 7, which cannot affect an existing deployment) and two corrections that *lower* a bar (items 9 and 10) -- and **item 6 is the only one that can change how an already-stored session replays** -- read it first if you have any persisted Decision session at all. Item 6 carries two independent predicates and **only one of them involves `weighted`**: the other reaches any Decision policy that sets `commitment.allow_decline_over_approval: true`, `majority` policies included. Read this section before upgrading any deployment that sets `MACP_POLICIES_DIR`, or that has persisted sessions bound to a policy with a Quorum `threshold`, a `weighted` `voting.algorithm`, or `commitment.allow_decline_over_approval: true`. `CHANGELOG.md` is generated from commit subjects and does not carry this detail.

### 1. An invalid policy file now refuses startup

Both routes into the registry -- the `RegisterPolicy` RPC and the `MACP_POLICIES_DIR` preload -- gained value-domain and conditional checks; the enforced set is listed in [Policy](policy.md#what-registration-checks). A file an earlier release accepted may now be out of domain: a fractional or zero Quorum `threshold.value`, `threshold.type: "weighted"`, an unknown `voting.algorithm` or `voting.quorum.type`, a `weighted` algorithm with an empty `weights` map, a `supermajority` `threshold` at or below `0.5` (including one that omits `threshold` entirely and relies on the `0.5` default), or a wildcard (`"*"`) policy carrying a Quorum `threshold` that was previously validated against the Decision schema alone and therefore never checked. Loading stops at the first rejection and **startup aborts** -- the preload error is propagated, not logged and skipped.

That is deliberate fail-closed behaviour, and it is why `MACP_POLICIES_DRY_RUN=1` exists. **Run the dry run with the new binary before you upgrade:**

```bash
MACP_POLICIES_DRY_RUN=1 MACP_POLICIES_DIR=/etc/macp/policies macp-runtime
```

It reports every `*.json` file by name as `OK <path> (<policy_id>)` or `REJECTED <path>: <reason>`, prints a checked/rejected count, and exits `0` if the directory would load or `1` if anything in it would be rejected. It binds no port, opens no storage, and replays nothing, so it needs neither TLS nor `MACP_ALLOW_INSECURE=1`. Its output is written to stdout/stderr directly rather than through `tracing`, so no `RUST_LOG` filter can suppress the report. Two things to know: the startup environment-configuration check still runs ahead of it, so an unrelated malformed variable aborts before the report is produced; and a readable directory containing no `*.json` exits `0` with an explicit `WARNING`, because a mis-pointed `MACP_POLICIES_DIR` otherwise looks identical to a clean run.

### 2. Do not unblock startup by deleting the rejected file

When a policy file blocks startup, the natural fix is to delete it. **Correct the file instead.** Deleting it does let the runtime boot, but persisted sessions bound to that `policy_version` are then replayed with the policy unresolved: replay resolves the version best-effort and leaves `policy_definition` empty when it cannot, and commitment enforcement treats an absent policy definition as "no policy to enforce" and returns early. Every in-flight session governed by the deleted policy therefore loses its governance **silently** -- commitments the policy would have denied are accepted, with no error and no log line tying it back to the deletion. The same applies to `UnregisterPolicy` on a policy that live sessions are still bound to.

Note the interaction with the next item: because an unresolved policy makes the Quorum mode fall back to the `ApprovalRequest`'s own `required_approvals` -- a value the mode already constrains to `1..=participants` -- deleting the policy also makes the replay failure below disappear. The two symptoms clear together, and the reason they clear is that the governance bar is no longer being applied.

### 3. A persisted Quorum session with an out-of-domain policy threshold no longer replays

> **Superseded in 0.9.0 for numeric thresholds** -- see [Upgrading to 0.9.0](#1-quorum-an-over-participant-policy-threshold-is-accepted-and-declinable-at-once). What follows still holds for `weighted`/unrecognised types.

RFC-MACP-0011 §5 rule 6 makes a policy `threshold` *replace* the `ApprovalRequest`'s `required_approvals`, but nothing previously held the replacement to the same `1..=participants` domain the runtime enforces on the field it replaces. The Quorum mode now refuses an `ApprovalRequest` whose effective threshold falls outside that domain, and replay dispatches the same code -- so such a session fails to replay. It is skipped with a warning, or is fatal at startup under `MACP_STRICT_RECOVERY=1`.

Detect it from the logs. The mode emits, at `WARN`:

```
quorum policy threshold is outside 1..=participants; refusing the ApprovalRequest
  session_id=... policy_id=... effective_threshold=... participants=...
```

naming the session, the bound policy, the computed threshold and the declared participant count -- everything needed to identify which policy to correct. Recovery follows it with `failed to replay session; skipping` carrying the same `session_id`.

**What it takes to reach this.** Not a legacy *policy* -- an **`ApprovalRequest` this runtime accepted before the guard above existed**. Registration is no substitute for the guard, because registration has no participant count to bound the threshold against: `{"type": "n_of_m", "value": 66}` passes every check in [Policy](policy.md#what-registration-checks) under the **new** binary and still trips the guard on a three-participant session. Two classes of threshold reach it, and they are not equally benign:

- **An out-of-domain numeric threshold** -- `n_of_m` or `count` above the declared participant count. The positive outcome was unreachable from the first message, and before this release `commitment_ready` carried no `counted > 0` guard, so `approvals + remaining < required` held with no ballot cast and the coordinator could seal a binding `quorum.rejected` with **zero** approvals (issue #145). That is the condition RFC-MACP-0011 §5 rule 4a reads as grounds for a decline, reached without a vote. Such a session really was broken: the only outcome it could ever have sealed was a decline nobody cast a ballot for.
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

**This item is the only place in this release where a stored session's replay can change**, and unlike item 4 it is not confined to a hand-built descriptor. The electorate rule is keyed on nothing -- RFC-MACP-0012 §4.1 makes it "normative for **every** schema version", so it reaches stored `schema_version: 1` and `2` descriptors as well as version 3.

**This item carries two independent changes, and the second one needs no `weights` map at all.** Alongside the electorate rule above, the decline guard of RFC-MACP-0007 §6.2 is now applied on a `Passed` round as well as on `Failed` and `NoVotes` -- §6.2 says "the guard applies across all three voting results" and had said so *before* spec #99, while this runtime applied it in only one. So `commitment.allow_decline_over_approval` now waives the approval *result* and not the guard: a decline over a round with no decisive reject is denied whatever the knob says. That reaches ordinary `majority` policies that have never had a `voting.weights` key. Audit for **both** predicates below; neither subsumes the other.

**Blast radius -- two independent predicates, either one sufficient.**

**Predicate A -- the weighted electorate.** A stored session matches when **both** hold:

1. the session is bound to a Decision policy whose `voting.algorithm` is `weighted`, **and**
2. at least one accepted `Vote` was cast by a participant that does **not** appear as a key in that policy's `voting.weights` map, with a vote other than `ABSTAIN`.

A does **not** depend on the committed outcome's direction, on `schema_version`, or on `allow_decline_over_approval`.

**Predicate B -- the decline guard on a passing round.** A stored session matches when **all three** hold:

1. the session is bound to a Decision policy that sets `commitment.allow_decline_over_approval: true`, **and**
2. its accepted history contains a **negative** `Commitment` (`outcome_positive: false`), **and**
3. the round that commitment sealed was `Passed` with **zero** decisive rejects -- in the ordinary case an all-`APPROVE` tally, since neither an abstention nor a missing ballot is a rejection.

B needs no `weighted` algorithm and no `weights` map. The minimal instance is fully registerable and trivially reachable: `{"voting": {"algorithm": "majority", "threshold": 0.5}, "commitment": {"allow_decline_over_approval": true}}` with three `APPROVE`s and a negative `Commitment` was `Allow` before this release and is `Deny` after.

**Neither predicate is complete on its own, and A is not the safe one to check.** An operator who audits only for `weighted` policies, finds none, and concludes the deployment is unaffected **can be wrong** -- B reaches ordinary `majority`, `supermajority`, `unanimous` and `plurality` policies. RFC-MACP-0012 §8's "Bounded exception -- weight-`0` decisiveness" describes something narrower than either predicate (a decline over a `Passed` tally under `allow_decline_over_approval: true` whose only reject came from a weight-`0` participant), because that is the only configuration the *spec's* earlier text had defined an outcome for. Two reasons that framing must not be read as this runtime's exposure. For A: this runtime had defined an outcome for **every** unlisted-voter configuration -- it defaulted the weight to `1.0` -- so A is wider than §8's corner in both directions. For B: §8 does not cover it at all. §8 is about weight-`0` decisiveness, and B has no weights; B is a **pre-existing conformance bug being fixed**, not a semantics change §8 sanctioned, which is precisely why the bounded exception does not extend to it.

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

**Pre-upgrade audit query.** Find affected sessions before upgrading. Run **both** predicates -- a deployment can match B without owning a single `weighted` policy:

- **For A:** any **Decision** session whose bound policy has `voting.algorithm: "weighted"` and whose accepted `Vote` messages include a sender that does **not** appear as a key in that policy's `voting.weights` map, with a vote other than `ABSTAIN`.
- **For B:** any **Decision** session whose bound policy sets `commitment.allow_decline_over_approval: true` and whose accepted history contains a negative `Commitment` sealed over a round carrying **zero** accepted `REJECT` ballots.

Sessions matching either may fail to replay after the upgrade.

**How to run it -- the surfaces that carry each fact.** Everything both predicates name is readable on the **old** binary, before you upgrade, over the ordinary RPC surface:

- **The session list and its mode.** `ListSessions` enumerates current session metadata; page it with `page_size` and the returned `next_page_token` until the token comes back empty. `GetSession` returns one session's `SessionMetadata`, which carries its `mode` and the `policy_version` it bound.
- **The policy's rules.** `GetPolicy` on that `policy_version` returns the `PolicyDefinition`; read `voting.algorithm`, the key set of `voting.weights`, and `commitment.allow_decline_over_approval` out of its `rules`. `ListPolicies` with a mode filter narrows the sweep to Decision policies first, which is usually the cheaper order -- a deployment with no matching policy needs no per-session pass at all.
- **The ballots and the committed outcome.** `StreamSession` replays a session's accepted history: send a frame carrying `subscribe_session_id` with `after_sequence: 0` and it emits the accepted envelopes in order, which is where the `Vote` senders and their values are, and where the `Commitment`'s `outcome_positive` is. Note the one gap: a session whose log has been compacted ends the stream with `FAILED_PRECONDITION` / `macp-error-code: HISTORY_UNAVAILABLE` (message `session history before ordinal N was compacted; ...`, details `{"lowest_available_sequence":N}`) instead of emitting the early entries, so treat a compaction response as "not audited" rather than "clean". Up to and including 0.11.0 the same refusal was an inline `INVALID_ENVELOPE` frame on a stream left open.
- **Offline, with the runtime down.** The same accepted history is on disk under the file backend at `<MACP_DATA_DIR>/sessions/<session_id>/log.jsonl`, one JSON entry per line, filterable with `jq` without starting the runtime. (A `SessionResume` entry's `banked_ms` field changed meaning in this release: it now records the remaining TTL banked at suspend rather than the pause's duration -- entries written before this change carry the older quantity under the same field name, with no discriminator between the two.)

**Contrast with item 4, which shipped ungated for a reason that does not transfer.** Item 4's negative-weighted-total change was ungated because it was "reachable only from a directly-constructed `PolicyDefinition`, since registration already refuses negative weights". **This one is reachable from an ordinary registered policy**: a `weighted` descriptor that simply omits a declared participant from `weights` passes every admission check, before and after. So do not read item 6 as another item 4.

Why the change is nonetheless right: the old reading let a participant the policy author had explicitly given no voting weight cast ballots that moved the outcome -- in §8's corner, the unilateral power to convert an approving weighted electorate's result into a decline; more commonly, as above, the power to carry a positive round the electorate had rejected. §8 accepts the resulting stored-replay break in writing. The rule itself is in [Policy](policy.md#voting-algorithm-semantics).

**Recovery for A.** There is no configuration that restores the old reading -- the rule is ungated by design. Correct the policy instead: add to `voting.weights`, with the weight they should carry, the participants who were always meant to vote, and leave genuine observers omitted. A registered policy is re-resolved from the live registry on full replay, so correcting it there is enough for a session with no checkpoint; a session that carries the old descriptor in a **checkpoint** keeps that descriptor verbatim, exactly as item 3 describes for the Quorum case.

Be honest about the case the correction does not cover. If the flipped ballots really were cast by intended observers, no weight map makes that session replay: the commitment in its history was authorized by voters the policy had given no weight, which is precisely the outcome §8 calls unsound. The append-only log is untouched either way -- an affected session was not loaded rather than lost -- so the history remains available for audit while you decide.

**Recovery for B: there is none, and that is the honest answer.** No policy edit makes a B session replay. Clearing `commitment.allow_decline_over_approval` denies the decline earlier rather than later; setting it changes nothing, because it is the guard and not the knob that refuses. The reject the guard requires was never cast, so there is no descriptor under which that history is authorized -- RFC-MACP-0007 §6.2 held the same before spec #99, which is what makes this a bug fix rather than a semantics change. As with A the append-only log is untouched, so such a session is not loaded rather than lost and its history stays available for audit; but plan for it to stay unloaded, and under `MACP_STRICT_RECOVERY=1` plan to clear it before the upgrade rather than after.

### 7. A Decision `SessionStart` may now bind an empty `participants` list

This one is a relaxation and **cannot affect an existing deployment**. `SessionStart` for `macp.mode.decision.v1` is accepted with `participants: []`, where earlier releases refused it; every other standards-track mode still requires a non-empty roster. Nothing that used to be accepted is now refused, so no stored session's replay changes and no policy file needs correcting -- and no stored session can already contain an empty roster, because one could not have been accepted.

The resulting session is **inert**, which is worth knowing before an operator reads one in `ListSessions` and expects it to progress: with no declared participants, `Proposal`, `Evaluation`, `Objection` and `Vote` are all refused with `FORBIDDEN` (the initiator's included), so no proposal can exist and no `Commitment` can be sealed. Such a session can only expire or be cancelled, and the roster cannot be added to afterwards. See [API](API.md#send) for the full `SessionStart` contract and [Modes](modes.md#decision-mode) for why Decision is the one carve-out.

### 8. A Quorum `threshold.value` of `0` no longer registers

RFC-MACP-0012 1.2.0-draft moved quorum `threshold.value` from `minimum: 0` to `exclusiveMinimum: 0` -- the quorum-side twin of the Decision-side floor this release already tightened (item 5). A zero approval bar is trivially satisfied, so a policy that reads as restrictive approved everything. `{"threshold": {"type": "n_of_m", "value": 0}}` and its `percentage` equivalent are now refused at registration, which means a `MACP_POLICIES_DIR` file carrying one **aborts startup** -- see items 1 and 2.

The floor is keyed on the `value` key being **present**. A rules object that omits `threshold` entirely, or supplies `{"threshold": {}}` or `{"threshold": {"type": "percentage"}}`, still registers and still leaves the rule inert, so the `ApprovalRequest`'s own `required_approvals` stands. Nothing about the built-in `policy.default` wildcard changes.

### 9. A `percentage` Quorum threshold can now resolve one approval lower

RFC-MACP-0012 §4.2 promoted the `percentage` ceiling rule into normative text and pinned its arithmetic: the effective bar is `ceil(value x declared_participant_count / 100)`, computed with **exact integer arithmetic**, and implementations "MUST NOT use floating-point division". This runtime already ceiled, but divided by `100` first, and that division is inexact in binary64 for most integer percentages -- the error survived into the ceiling and produced a bar one approval too high. `value: 28` over 25 participants required 8 approvals where the rule gives 7; `value: 7` over 100 gave 8 instead of 7; `value: 68` over 75 gave 52 instead of 51. Thirteen `(value, participants)` pairs diverge within `value` 1-100 and participants 1-100, and the divergence grows more common at larger rosters.

**Who this reaches.** Only sessions bound to a policy whose `threshold.type` is `percentage`, and only those whose `(value, participant count)` pair is one of the affected ones. For them the bar **drops by one**, so a positive commitment that this runtime used to deny is now allowed at one fewer approval. No stored session's accepted history changes: a commitment the old bar denied was *rejected*, and rejected messages never enter accepted history (RFC-MACP-0001 §8.3), so replay is unaffected. What changes is the live bar and anything computed from it -- `QuorumMode::effective_threshold_for_session`, the `1..=participants` domain guard on an `ApprovalRequest`, and the readiness the coordinator polls. RFC-MACP-0012 §8's completion note covers this explicitly: the earlier text stated no rounding direction, so it determined no outcome at a non-integral product, and a runtime that had privately chosen one is the only thing this can perturb.

The denominator is also pinned: it is the participant count declared at `SessionStart` and does **not** shrink as ballots, abstentions included, are cast. This runtime already read it that way.

### 10. An objection-authorized decline is no longer denied by `require_vote_quorum` or the evaluation gate

RFC-MACP-0007 §6.2 exempts an *objection-authorized* decline -- a negative `Commitment` under `objection_handling.critical_objection_action: "finalize_decline"` with a standing critical `Objection` -- from the voting tri-state and the decline guard. The guard is a two-conjunct conjunction whose second conjunct is `commitment.require_vote_quorum`, and §6.2 did not say whether the waiver reached it. This runtime raised that as spec issue #117 and, pending the ruling, kept the quorum gate and the `evaluation.*` prerequisites applying. Spec PR #126 ruled the waiver covers the guard **whole**: the quorum condition legitimizes an outcome deriving its authority from the voting result, an objection-authorized decline derives none, so a runtime "MUST NOT deny it for an unmet voting quorum", and `evaluation.required_before_voting` / `evaluation.minimum_confidence` "are prerequisites of the same voting pipeline and likewise MUST NOT be applied".

**Who this reaches.** Only sessions bound to a Decision policy that sets `critical_objection_action: "finalize_decline"` *and* at least one of `commitment.require_vote_quorum: true` or `evaluation.required_before_voting: true`, and only once a critical objection is standing. For them a negative `Commitment` that this runtime used to deny with `POLICY_DENIED` is now allowed. Nothing moves from allowed to denied, and the positive direction is untouched -- a positive commitment under the same veto is still denied, and still reports the unmet quorum and evaluation prerequisite among its reasons.

**No stored session's replay changes.** The commitment the old gates denied was *rejected*, and rejected messages never enter accepted history (RFC-MACP-0001 §8.3), so no stored history can contain one. §6.2 states this explicitly for this rule. What changes is live acceptance: a `finalize_decline` session that was previously reachable only by TTL expiry or an initiator `CancelSession` can now record a committed negative outcome, so an operator who was working around the strand -- leaving `require_vote_quorum` `false`, or soliciting a throwaway `ABSTAIN` to clear the participation floor -- can drop the workaround. See [Policy](policy.md#decision-mode) for the full rule and `tests/conformance/decision_finalize_decline_quorum_waiver.json` for the canonical discriminator.

## Upgrading into suspension-interval recording

### 1. Expect one-time `suspension_intervals mismatch` replay warnings on the first boot

A session now records each completed `(suspended_at, resumed_at)` pair, so that time spent `Suspended` can be subtracted from the Handoff implicit-accept timeout (RFC-MACP-0010 §5.1). On the **first** boot after upgrading, startup recovery emits one warning per persisted session that was ever suspended and resumed:

```
WARN replay/snapshot suspension_intervals mismatch
  session_id=... replayed_suspension_cycles=2 snapshot_suspension_cycles=0
```

**It is benign, and it does not recur.** The warning is the startup consistency check comparing two sources that necessarily disagree exactly once: the snapshot on disk was written by the *previous* release, which had no such field, so it deserializes as empty; replay rebuilds the pairs correctly from the `SessionSuspend`/`SessionResume` entries in the append-only log, which were always there. **Replay is the authority** -- the recovered session is the correct one, and it is what the runtime serves. Recovery then re-saves each replayed session, so the next boot's snapshot carries the intervals and the comparison agrees. Nothing is lost and no action is required.

Two consequences worth knowing:

- The warning is **advisory only**. It increments `recovery_replay_mismatches`, which is read zero-vs-nonzero, so a nonzero count on this one boot is expected and does not indicate a determinism bug. `MACP_STRICT_RECOVERY` does **not** turn these into startup failures -- it governs recovery *errors*, not consistency warnings.
- A session whose recovery goes through a **mid-session checkpoint written before this release** replays with its pre-checkpoint pauses missing for good: the checkpoint fast path replays only the entries after the checkpoint, and a legacy checkpoint carries no intervals. That is deliberately the safe direction -- a short interval list can only make a newly computed implicit-accept deadline land *earlier*, never later, and it never changes a deadline already recorded in accepted history. There is no setting that avoids this for a checkpoint already on disk: `MACP_CHECKPOINT_INTERVAL` gates only whether *new* checkpoints are written, while the recovery fast path keys on a `Checkpoint` entry being present in the log and never consults the variable. Leaving it at `0` (the default) prevents *future* legacy-shaped gaps; it does not undo an existing one. The condition is also self-limiting -- every pause recorded from this release forward is in the log after the old checkpoint, so it replays normally.

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
| `MACP_AUTH_JWT_ALGS` | `RS256,ES256` | Comma-separated JWT algorithm allowlist (`HS256` requires explicit opt-in) |
| `MACP_MAX_PAYLOAD_BYTES` | `1048576` | Maximum envelope payload size in bytes |
| `MACP_SESSION_START_LIMIT_PER_MINUTE` | `60` | Per-sender session creation rate limit |
| `MACP_MESSAGE_LIMIT_PER_MINUTE` | `600` | Per-sender message rate limit |
| `MACP_RECEIPT_SIGNING_KEY_PATH` | -- | File holding the Ed25519 decision-receipt signing key: a PKCS#8 PEM or a private OKP JWK. Unset: receipts are unsigned. See [Decision receipts](#decision-receipts) |
| `MACP_RECEIPT_SIGNING_KEY` | -- | The same key inline, for platform secret stores. Setting both forms is a startup error |
| `MACP_RECEIPT_SIGNING_KID` | -- | The signing key's `kid`. Required for a PEM; for a JWK it must match the JWK's own `kid` when both are set |
| `MACP_RECEIPT_PUBLIC_KEYS_PATH` | -- | JWKS of retired public keys, still published by `GetReceiptKeys` so receipts signed before a rotation keep verifying |
| `MACP_RUNTIME_ID` | `macp-runtime` (unsigned only) | Receipt `issuer` is `runtime:<id>`. Required when a signing key is set; use a distinct, stable value per deployment |
| `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` | `100` | `ListSessions` page size when the request sends `page_size = 0` |
| `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` | `1000` | Hard cap a requested `ListSessions` `page_size` is clamped to |
| `MACP_METRICS_ADDR` | -- (off) | Prometheus text endpoint bind address, e.g. `127.0.0.1:9464` (`src/main.rs:554`) |
| `MACP_CONCURRENCY_LIMIT_PER_CONNECTION` | `64` | tonic per-connection concurrency limit (`src/main.rs:427`) |
| `MACP_MAX_CONCURRENT_STREAMS` | `128` | HTTP/2 max concurrent streams (`src/main.rs:431`) |
| `MACP_REQUEST_TIMEOUT_SECS` | `30` | Per-request timeout (`src/main.rs:435`) |
| `MACP_SHUTDOWN_DRAIN_SECS` | `10` | Graceful-shutdown drain deadline after SIGINT or SIGTERM (`src/main.rs:501`). See [Graceful shutdown](#graceful-shutdown) |
| `MACP_CHECKPOINT_INTERVAL` | `0` (disabled) | Log entries between checkpoints |
| `MACP_COMPACT_TERMINAL_LOGS` | on | `0` or `false` keeps an ended session's full log, so `WhatIfReplay` can replay it, instead of compacting it to one checkpoint |
| `MACP_AUDIT_LEVEL` | `debug` | `info` logs every accepted message at `info` ("message accepted (audit)"); replaces the pre-0.9 `rules.audit` policy block |
| `MACP_AUDIT_INFO_POLICIES` | — | Comma-separated `policy_id`s whose sessions are audited at `info` |
| `MACP_AUDIT_INFO_MODES` | — | Comma-separated mode names whose sessions are audited at `info` |
| `MACP_CLEANUP_INTERVAL_SECS` | `60` | Background maintenance interval in seconds: TTL expiry, memory eviction, disk GC, and eager observation of mode-computed deadlines (the handoff implicit accept, RFC-MACP-0010 §5.1(2)) |
| `MACP_SESSION_RETENTION_SECS` | `3600` | Age (from session start) at which terminal sessions are evicted from **memory**; their durable data is kept |
| `MACP_SESSION_DISK_RETENTION_SECS` | `0` (keep forever) | Age (from session start) at which terminal sessions' **durable data** is deleted; `0` disables disk GC entirely |
| `MACP_STRICT_RECOVERY` | off | Set to `1` to fail on any recovery error |
| `MACP_POLICIES_DIR` | -- | Directory of governance policy JSON files preloaded at startup; a file that fails validation aborts startup, and the wire registry becomes read-only |
| `MACP_POLICIES_DRY_RUN` | off | Set to `1` to validate `MACP_POLICIES_DIR` and exit `0`/`1` without starting the server |
| `MACP_POLICY_SCHEMAS_DIR` | -- | **Development and CI only; the server never reads it.** Path to the spec repository's `schemas/json/policy` directory, used by the `enum_lists_match_the_canonical_schemas`, `closed_key_sets_match_the_canonical_schemas` and `canonical_schema_fixtures_register_or_are_refused` parity tests (all three run in the `conformance-oracle` CI job) -- see the warning below |
| `RUST_LOG` | `info` | Log level filter |

`MACP_POLICY_SCHEMAS_DIR` is listed here because it is otherwise documented nowhere, and a contributor changing a registration mirror needs it. It is read only by `macp-policy`'s parity unit test, which asserts the hand-written value-domain mirrors in `crates/macp-policy/src/registry.rs` still match the canonical schemas. Two warnings:

- **Point it at a clean `git archive` export of the spec commit CI reads, never at a sibling working tree.** CI checks the spec repo out at `SPEC_REV` (`.github/workflows/ci.yml`), so a sibling checkout that is dirty, or ahead of or behind that pin, produces parity failures that do not exist in CI -- and it can move under you mid-session. Export first: `git -C <spec-repo> archive <sha> schemas/ | tar -x -C <tmpdir>`, then point the variable at `<tmpdir>/schemas/json/policy`.
- **A set-but-missing directory panics by design.** Setting the variable asserts the canonical schemas are available, so the test refuses to skip silently. Unset it to fall back to a sibling checkout, or to skip the parity check entirely when no checkout exists.

### OpenTelemetry

These variables take effect **only in a binary built with the `otel` Cargo feature** (`cargo build --features otel`). Images published after `0.11.0` include it (see [Container deployment](#container-deployment)); a default `cargo build` and images up to `0.11.0` do not. A binary without the feature logs one warning at startup when an endpoint variable below is set, and otherwise ignores every `OTEL_*` variable.

Telemetry is **off unless an endpoint is configured**. It turns on when at least one of `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` is set to a non-empty value and `OTEL_SDK_DISABLED` is not `true`. With telemetry off the runtime installs no exporter and no propagator, opens no connection, and logs exactly what a default build logs. A telemetry misconfiguration never stops the runtime: it logs one `ERROR` line naming the variable and runs with telemetry off.

| Variable | Default | Description |
|----------|---------|-------------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | -- (off) | OTLP/gRPC collector for traces and metrics, `http://host:4317` or `https://host:4317`. There is no built-in default endpoint |
| `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | -- | Traces only; overrides the generic endpoint for traces. Setting only this exports traces and no metrics |
| `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` | -- | Metrics only; overrides the generic endpoint for metrics |
| `OTEL_EXPORTER_OTLP_HEADERS` | -- | Extra gRPC metadata for the collector, `key1=value1,key2=value2` (`_TRACES_`/`_METRICS_` variants too) |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` | Only `grpc` is supported, here and in `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` / `OTEL_EXPORTER_OTLP_METRICS_PROTOCOL`. Any other value logs an error and leaves telemetry off |
| `OTEL_EXPORTER_OTLP_CERTIFICATE` | -- | PEM file of the CA to trust for an `https://` endpoint. When set it is the **only** trust anchor (the platform's native roots are not consulted, so it works on a host without a root store); `OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE` / `OTEL_EXPORTER_OTLP_METRICS_CERTIFICATE` override it per signal. Read by the runtime itself, because the OTLP exporter does not honour it. An unreadable file logs an error and leaves telemetry off |
| `OTEL_SERVICE_NAME` | `macp-runtime` | `service.name` resource attribute; beats a `service.name` in `OTEL_RESOURCE_ATTRIBUTES` |
| `OTEL_RESOURCE_ATTRIBUTES` | -- | Extra resource attributes, `key1=value1,key2=value2`. `service.version` defaults to the runtime version |
| `OTEL_TRACES_SAMPLER` / `OTEL_TRACES_SAMPLER_ARG` | `parentbased_always_on` | Trace sampler, read by the OpenTelemetry SDK (`always_on`, `always_off`, `traceidratio`, `parentbased_always_on`, `parentbased_always_off`, `parentbased_traceidratio`) |
| `OTEL_METRIC_EXPORT_INTERVAL` | `60000` | Metric export interval in milliseconds |
| `OTEL_SDK_DISABLED` | `false` | `true` (any case) turns telemetry off even with an endpoint set |

Notes:

- **TLS.** An `https://` endpoint is verified against `OTEL_EXPORTER_OTLP_CERTIFICATE` alone when it is set, and otherwise against the platform's native root store (the published image installs `ca-certificates`; a host with no root store needs the certificate variable). Client certificates (`OTEL_EXPORTER_OTLP_CLIENT_KEY`, `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE`) are **not supported**.
- **Propagation.** With telemetry on, the only propagator installed is W3C trace context (`traceparent`/`tracestate`, carried in gRPC metadata per RFC-MACP-0006 §2.1). No baggage propagator is installed, and trace context never enters an Envelope or accepted history.
- **What is exported today.** This release installs the pipeline (resource, propagator, exporter, sampler), the gRPC SERVER span of every RPC, the Envelope `process` spans, the session transition spans with their anchor links, and the `macp.envelope.*` and `macp.session.*` metric instruments of `macp-semconv/0.1` (see Monitoring).
- **Logs are unchanged.** `RUST_LOG` governs logging only: it can neither silence telemetry spans nor add them to log lines, and no log event (including any `reason` text) is exported as telemetry.
- **An unreachable collector is not fatal.** Exports fail in the background, the OpenTelemetry SDK's own warnings reach the log subject to `RUST_LOG`, and shutdown waits at most 5 seconds for the final flush. That 5-second cap comes **after** the `MACP_SHUTDOWN_DRAIN_SECS` drain (and the final session snapshot), so the worst-case exit time with telemetry on is the drain deadline plus 5 seconds.

### HTTP/JSON listener

These variables take effect **only in a binary built with the `http-listener` Cargo feature** (`cargo build --features http-listener`); images published after `0.11.0` include it (see [Container deployment](#container-deployment)). The listener is a second port beside gRPC, serving the same RPCs as HTTP/JSON with server-sent events; the wire format is in [API: HTTP/JSON binding](API.md#httpjson-binding).

The listener starts **only when `MACP_HTTP_BIND_ADDR` is set** to a non-empty value. Unset (or empty), no socket is opened, no task runs, nothing is logged, and no other `MACP_HTTP_*` variable is read. Set, every `MACP_HTTP_*` variable is validated before anything binds, and every error is reported at once, prefixed with its variable name, and aborts startup. A binary built without the feature refuses to start when `MACP_HTTP_BIND_ADDR` is set, so the setting is never silently ignored.

| Variable | Default | Description |
|----------|---------|-------------|
| `MACP_HTTP_BIND_ADDR` | -- (listener off) | Socket address of the HTTP listener, e.g. `0.0.0.0:8080`. Must not collide with `MACP_BIND_ADDR` (same port with the same IP, or either side unspecified); port `0` never collides |
| `MACP_HTTP_PUBLIC_URL` | -- | The binding's public base URL (RFC-MACP-0006 §4.1 `{base}`), e.g. `https://macp.example.com/api`: absolute, no trailing slash, no query or fragment, no userinfo, `https` unless `MACP_ALLOW_INSECURE=1`. Its path is the route prefix (`{prefix}/macp/v1/{Method}`; `/macp/v1/{Method}` is served too). When set, `GetManifest` and `/.well-known/macp.json` advertise it as the `macp.transport.http.v1` endpoint |
| `MACP_HTTP_CORS_ORIGINS` | empty (closed) | Comma-separated browser origins allowed cross-origin, each `scheme://host[:port]` (`http` or `https`). `*`, a path and an empty entry are refused; a trailing `/` and a default port (`:443`, `:80`) are dropped, and scheme and host are lowercased |
| `MACP_HTTP_MAX_BODY_BYTES` | `ceil(4/3 × MACP_MAX_PAYLOAD_BYTES) + 65536` (1,463,638 at the default payload limit) | Request body cap; a larger body is `413 PAYLOAD_TOO_LARGE`. A value below the default is refused, so a base64 payload at the payload limit always reaches the payload check |
| `MACP_HTTP_MAX_CONNECTIONS` | `1024` | Concurrent connections the listener admits; at the bound new connections wait in the kernel backlog |

The listener **reuses** these gRPC settings, parsed exactly as the gRPC listener parses them: `MACP_TLS_CERT_PATH`/`MACP_TLS_KEY_PATH` (one identity for both ports), `MACP_ALLOW_INSECURE`, `MACP_MAX_PAYLOAD_BYTES` (checked in band, in the `Send` Ack), `MACP_REQUEST_TIMEOUT_SECS` (bounds producing a response head; an SSE stream's events flow after it and are not bounded), `MACP_MAX_CONCURRENT_STREAMS` (the HTTP/2 stream bound) and `MACP_SHUTDOWN_DRAIN_SECS` (one drain for both listeners). Authentication and the per-sender rate limits are the same objects on both bindings, so a sender's buckets are shared, not doubled.

**TLS.** With both PEM paths set, the listener serves TLS (rustls) and offers ALPN `h2` then `http/1.1`: a client that negotiates `h2` gets HTTP/2, anything else (including no ALPN) gets HTTP/1.1. Without TLS the listener refuses to start unless `MACP_ALLOW_INSECURE=1`, and then serves HTTP/1.1 and prior-knowledge HTTP/2 in the clear and logs a warning. Connections that send nothing are dropped: 10 s for the TLS handshake and for the first bytes, 10 s to read an HTTP/1.1 request head (also between keep-alive requests), and an HTTP/2 keep-alive PING every 30 s that must be answered within 20 s. These deadlines are fixed.

**CORS.** Closed unless `MACP_HTTP_CORS_ORIGINS` lists origins. Every `OPTIONS` is a preflight answered `204`; only a listed origin gets `Access-Control-Allow-*` headers. Credentials mode is never allowed (the binding uses `Authorization`, never cookies). Details in [API: CORS](API.md#cors).

**Behind a reverse proxy or load balancer:**

- **Do not buffer `text/event-stream` responses.** The streaming RPCs (`StreamSession`, `Watch*`) answer with `Content-Type: text/event-stream` and `Cache-Control: no-store`, one event per message; a proxy that buffers the response delays every event until its buffer fills. On nginx set `proxy_buffering off;` (and `proxy_cache off;`) for the `/macp/v1/` location; on other proxies disable response buffering or compression for that content type.
- **Set idle and read timeouts above 30 s.** An idle stream carries a `: keepalive` comment every 30 s, so a proxy idle timeout of 30 s or less cuts healthy streams; 60 s or more is safe (nginx `proxy_read_timeout` defaults to 60 s; check your load balancer's idle timeout).
- **Pass `Last-Event-ID` through**, together with `Authorization`, `Content-Type`, `Accept`, `traceparent` and `tracestate`. A client resumes a subscribe stream with `Last-Event-ID`; a proxy that drops it makes every reconnect replay from the start of the session.
- **Body size.** Allow request bodies up to `MACP_HTTP_MAX_BODY_BYTES` (nginx `client_max_body_size`), or a `Send` near the payload limit is refused by the proxy instead of answered by the runtime.
- **Path prefix.** Mount the runtime under a prefix by setting `MACP_HTTP_PUBLIC_URL` to the public base; the proxy may forward the prefix or strip it, since both route shapes are served. `/.well-known/macp.json` is answered at the root of the listener whatever the prefix, so route it there if clients discover the runtime through it.
- **Shutdown and failover.** When the runtime drains, open SSE streams end without an error event, as a lost connection, and clients reconnect with `Last-Event-ID` to another instance or after the restart.

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

**Trace anchor** (`otel` feature with telemetry active only): each session's
anchor (the SessionStart span context, `{"v":1,"traceparent":"00-…"}`) is a
best-effort side record outside session state, the log and checkpoints. It is
written once per session off the SessionStart path, never awaited by the ack,
and loaded lazily after a restart so later spans keep linking to the
pre-restart SessionStart. A crash right after a SessionStart can lose it; the
session itself is unaffected and its later spans simply have no link. A
missing or unreadable anchor means no link, never an error.

| Backend | Where | Survives restart | Removed by disk GC |
|---|---|---|---|
| `file` | `sessions/<id>/anchor.json` | yes | yes (the session directory) |
| `rocksdb` | `receipts` column family, key `<id>\0anchor` (no new column family) | yes | yes |
| `redis` | `<prefix>:anchor:<id>`; with `MACP_SESSION_DISK_RETENTION_SECS > 0` it expires after retention + 24 h + 1 h | yes | yes |
| memory (`MACP_MEMORY_ONLY=1`) | the runtime's memory only | no | n/a |

Rolling back to a binary that predates anchors keeps all three backends
working: the old binary ignores `anchor.json` and the extra RocksDB/Redis key.
Its disk GC does not delete the RocksDB/Redis anchor key, though. Redis
expires such a key by itself (its expiry is set whenever retention > 0), and
on RocksDB the next start of a newer binary with
`MACP_SESSION_DISK_RETENTION_SECS > 0` sweeps those orphans once, after
recovery, off the serving path.

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
restrict which identities may call them. `WatchSignals` and (since 0.12.0,
#274) `WatchPolicies` require authentication; `ListModes`/`GetManifest`/`WatchModeRegistry`/`WatchRoots`
are open discovery surfaces by design. `ListSessions` is now paged, and the
decision not to sign its opaque `page_token` rests on exactly the unfiltered
property described here -- a forged cursor can only reposition a caller within
a listing it may already read in full, so if per-caller filtering is ever added
to `ListSessions`, that no-signature decision must be re-analyzed first.

**Memory-only mode** disables persistence entirely. Set `MACP_MEMORY_ONLY=1` for testing or ephemeral workloads. All session data is lost when the process exits.

## Authentication

The runtime applies a pluggable resolver chain assembled at startup:

1. **JWT bearer** (active when `MACP_AUTH_ISSUER` is set) -- validates signature, issuer, audience, and expiration against a JWKS. Default algorithm allowlist: `RS256`, `ES256`; `HS256` (shared-secret) requires explicit opt-in via `MACP_AUTH_JWT_ALGS=HS256`. The `sub` claim becomes the sender; an optional `macp_scopes` claim carries capability flags (`allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, `is_observer`).
2. **Static bearer** (active when `MACP_AUTH_TOKENS_FILE` or `MACP_AUTH_TOKENS_JSON` is set) -- looks up opaque tokens in a preloaded identity map. Accepts `Authorization: Bearer <token>` or the alternate `x-macp-token: <token>` header.
3. **Dev-mode fallback** -- activates only when **neither** JWT nor static bearer is configured. Any `Authorization: Bearer <value>` header authenticates the caller as sender `<value>` with full capabilities **except** `is_observer`: it can start sessions and manage the mode registry, but it cannot read a session it neither started nor joined. An observer such as a control plane therefore cannot watch other agents' sessions under dev mode; give it a static token with `"is_observer": true` instead (`crates/macp-auth/src/security.rs`, `dev_authenticate`). Intended strictly for local development.

If a credential matches a resolver but fails verification (expired JWT, unknown static token), the request is rejected with `UNAUTHENTICATED` -- the chain does **not** fall through to a later resolver.

## Decision receipts

`GetDecisionReceipt` returns a `macp-receipt/1` decision receipt for a resolved
standards-track session (RFC-MACP-0013 §13): the resolving Commitment, its
`commitment_hash`, the payload bytes verbatim, and -- when a signing key is
configured -- a detached Ed25519 JWS over the body. `GetReceiptKeys` publishes
the JWKS a verifier checks it against. `macp verify` checks a receipt offline.

**Issuance is lazy.** A receipt is built on the first `GetDecisionReceipt` for
a session, persisted first-writer-wins, and every later call -- across
restarts, eviction from memory and key rotation -- serves the same bytes.
Nothing on the message path, in replay or in recovery issues a receipt, so a
receipt problem can never fail or delay a Commitment. A session that is not
resolved, or whose mode is an extension (`ext.*`, including
`ext.multi_round.v1`), gets `RECEIPT_UNAVAILABLE`.

**A receipt discloses the whole Commitment.** It carries the payload verbatim,
`reason` and every other field included, and anyone holding it can show it to
a third party -- that is its purpose. `GetDecisionReceipt` is authorized like
`GetSession` (the session's initiator and participants, and observers), so
treat a Commitment's `reason` as something every reader may publish.

**Generate a key** and keep it secret; never commit it:

```bash
openssl genpkey -algorithm ed25519 -out receipt-signing.pem
export MACP_RECEIPT_SIGNING_KEY_PATH=$PWD/receipt-signing.pem
export MACP_RECEIPT_SIGNING_KID=prod-2026-10
export MACP_RUNTIME_ID=prod-eu-1
```

A malformed or unreadable key, both `MACP_RECEIPT_SIGNING_KEY` and
`MACP_RECEIPT_SIGNING_KEY_PATH` set, `MACP_RECEIPT_SIGNING_KID` without a key, a
PEM without `MACP_RECEIPT_SIGNING_KID`, a signing key without `MACP_RUNTIME_ID`,
a `kid` conflict, or a published key
carrying a private member or reusing the active `kid` aborts startup naming the
variable; the key text is never logged.

**`MACP_RUNTIME_ID` is part of every receipt.** The `issuer` is
`runtime:<MACP_RUNTIME_ID>`, so it must be stable across restarts and distinct
per deployment -- two deployments sharing one `issuer` cannot be told apart by
a verifier. A signing key without `MACP_RUNTIME_ID` aborts startup, so no
signed receipt carries a shared default. Unsigned receipts fall back to
`runtime:macp-runtime`. There is no hostname fallback.

**Rotating a key.** Move the old public key into the JWKS at
`MACP_RECEIPT_PUBLIC_KEYS_PATH` and configure the new private key under a new
`kid`. Receipts already issued keep their original signature and keep verifying
against the published old key; new sessions are signed with the new one.

**Durability.** The `file` backend stores `sessions/<id>/receipt.json` with an
atomic hard link, `rocksdb` and `redis` with a compare-and-set. On a file
system without hard links, in memory-only mode, or on a third-party backend
without receipt support, a receipt lives only in memory: it is kept for as
long as the session can be read, eviction included, but a restart can serve a
different signed receipt for the same session. The runtime warns at startup
when it signs on such a backend. Disk GC deletes a receipt with its
session.

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

**Mode deadlines** -- The same background task observes deadlines a mode computes, on the same interval. The one in the standards-track modes today is the handoff implicit accept (RFC-MACP-0010 §5.1(2)): an outstanding `HandoffOffer` past its `acceptance.implicit_accept_timeout_ms` is accepted by the runtime, which appends the `HandoffAccept` to accepted history and publishes it to `StreamSession` subscribers. `MACP_CLEANUP_INTERVAL_SECS` is therefore the observation-latency bound on that acceptance -- but only the latency: the recorded `timestamp_unix_ms` is the computed deadline whichever path emits it, so changing the interval never changes permanent history. The deadline is also observed on demand, ahead of the next session-scoped message, so no message is ever evaluated against a stale offer regardless of the interval. The sweep runs *after* TTL expiry within a pass: a session whose TTL and implicit-accept deadline both lapsed unobserved expires rather than accepting, matching the on-demand path's precedence.

**Session eviction** -- Terminal sessions (resolved, expired, or cancelled) are evicted from memory once their age exceeds `MACP_SESSION_RETENTION_SECS` (default one hour), measured from session **start** rather than from when they became terminal. This is on by default and bounds memory usage. Their data remains on disk and can be replayed if needed. Deleting that durable data is a separate, opt-in step governed by `MACP_SESSION_DISK_RETENTION_SECS`, which defaults to `0` -- disk GC does not run at all unless you set it.

**Decision receipts** -- The `/metrics` endpoint reports `macp_receipts_issued_total{signed="true|false"}` (receipts issued, persisted or, on a backend without receipt support, held in memory) and `macp_receipt_issue_failures_total` (storage failures or unbuildable receipts on `GetDecisionReceipt`).

**Log compaction** -- When a session reaches a terminal state, the runtime compacts its log into a single checkpoint entry by default. This shrinks storage for completed sessions, but it discards the history that `WhatIfReplay` and `StreamSession` replay from the start need. `MACP_COMPACT_TERMINAL_LOGS` is the runtime's history-retention control (RFC-MACP-0012 §8.1 item 8). Set it to `0` to keep the full log; the runtime then appends a checkpoint instead. The self-host stack in `deploy/` sets it. Only sessions that end after the change keep their history; a log already compacted cannot be restored, and `WhatIfReplay`, like a `StreamSession` resume below the compacted base, refuses it with `HISTORY_UNAVAILABLE`. A kept log is never trimmed by the runtime; `MACP_SESSION_DISK_RETENTION_SECS` still deletes a terminal session's durable data at its age limit.

**Tracing** (`otel` builds with telemetry on; see "OpenTelemetry" under Environment variables) -- Every `macp.v1.MACPRuntimeService` RPC gets one OpenTelemetry span of kind SERVER, named `macp.v1.MACPRuntimeService/{Method}` (a method the service does not define is named `_OTHER`), with the gRPC attributes `rpc.system=grpc`, `rpc.service`, `rpc.method` and `rpc.grpc.status_code`. Its parent is the caller's W3C `traceparent`/`tracestate`, read from **gRPC metadata only** (RFC-MACP-0006 §2.1), never from an Envelope; without a valid `traceparent` the span starts a new trace. The span status is ERROR only for `UNKNOWN`, `DEADLINE_EXCEEDED`, `UNIMPLEMENTED`, `INTERNAL`, `UNAVAILABLE` and `DATA_LOSS`, with the code name as its description (never the error message); `NOT_FOUND`, `INVALID_ARGUMENT`, `PERMISSION_DENIED` and the other codes are protocol outcomes and leave it unset. A streaming RPC's span lasts until its stream ends, so a long-lived `Watch*` stream is exported when the client disconnects (and is lost on a hard shutdown). Health checks and reflection get no span, and no peer address is recorded. A request that hits `MACP_REQUEST_TIMEOUT_SECS` before its response headers go out ends its span without `rpc.grpc.status_code` (tonic answers `CANCELLED`, outside the error set, and applies that timeout outside the tracing layer).

**HTTP tracing and metrics** (`otel` builds with the `http-listener` feature and telemetry on) -- Every request the HTTP listener answers gets one span of kind SERVER, named `{method} {route}` per the OpenTelemetry HTTP semantic conventions (`POST /macp/v1/Send`; `GET /.well-known/macp.json`), or the method alone when no route matched (an `OPTIONS` preflight, a transport `404`); a method outside the RFC 9110 set is recorded as `_OTHER` and the span is named `HTTP`. Attributes: `http.request.method`, `http.route` (the RPC path **without** the `MACP_HTTP_PUBLIC_URL` prefix, only for a matched route), `url.scheme`, `network.protocol.version` and `http.response.status_code`, plus `error.type` (the status code) on a `5xx`. No header, token, body, query, client-supplied path or peer address is recorded. The span status is ERROR, with no description, only for a `5xx`; a `4xx` leaves it unset. Its parent is the request's `traceparent`/`tracestate` header, and the `process` span of a `Send` is its child. An SSE stream's span lasts as long as the stream. The same point records the histogram `http.server.request.duration` (unit `s`, bucket bounds 5 ms to 10 s) with `http.request.method`, `http.route`, `http.response.status_code`, `url.scheme` and, on a `5xx`, `error.type`; it is exported over OTLP only (it is not rendered on `/metrics`), and for a stream it measures the stream's lifetime.

**Envelope processing spans** (`otel` builds with telemetry on) -- Every received Envelope gets one INTERNAL span covering receipt to the accept/reject decision, named `process {macp.mode.id}` (`process _OTHER` for a mode the runtime has not registered, bare `process` for an ambient Signal or Progress). On `Send` its parent is the RPC's SERVER span; on `StreamSession` every Envelope on a stream is a child of the stream-open SERVER span. The attributes are those of `span.macp.process` in the protocol's [OpenTelemetry semantic conventions](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/blob/main/docs/opentelemetry.md) (`macp-semconv/0.1`); `macp.mode.id` keeps the mode as received, and `macp.sender.id` is the **authenticated** sender, absent when the Envelope is refused before authentication. Privacy defaults: spans carry identifiers and types only -- never a payload, an extension value, a `reason`, or an error message. Every client-supplied string is cut to 256 bytes. A rejection records its registry code as `error.type` and outcome `rejected`, and leaves the span status unset; only an implementation failure (a storage failure, recorded as `INTERNAL_ERROR`) or a span that never reached a decision (a panic, or a client that disconnected mid-request) is ERROR, with `INTERNAL_ERROR` as the description. An Envelope-less `SendRequest`, a stream subscribe frame, and the handoff synthetic implicit accept (no Envelope is received) get no `process` span.

**Envelope metrics** -- In **every** build, `/metrics` also renders the semconv Envelope families, after all the lines above: `macp_envelope_processed_total{macp_mode_id,macp_message_type,macp_envelope_outcome}` (a counter; `macp_envelope_outcome` is `accepted`, `duplicate` or `rejected`) and the histogram `macp_envelope_process_duration_seconds_{bucket,sum,count}` with the same labels and the bucket bounds 0.5 ms to 10 s. `macp_mode_id` is omitted for an ambient Signal or Progress. Neither family appears before the first Envelope. An `otel` build with metrics export on also exports them over OTLP as `macp.envelope.processed` and `macp.envelope.process.duration`, every `OTEL_METRIC_EXPORT_INTERVAL` (default 60 s). Labels cannot be grown by a client: a mode the runtime has not registered is recorded as `_OTHER` with message type `_OTHER`, a message type the mode's descriptor does not list is `_OTHER` (`Signal` and `Progress` are kept under every registered mode), and beyond 2,000 label sets new ones fold into `macp_mode_id="_OTHER",macp_message_type="_OTHER"`. No session, message or sender identifier is ever a label. Every received Envelope is counted, including those refused before the kernel (shape, authentication, rate limit) and those on `StreamSession`. `macp_messages_accepted_total` also counts the runtime's handoff synthetic implicit accepts, which `macp_envelope_processed_total` does not (no Envelope is received), so the two differ by that number; this is expected.

**Session transition spans** (`otel` builds with telemetry on) -- Every session lifecycle transition gets one INTERNAL span, named `{op} {macp.mode.id}` (`{op} _OTHER` once the session's mode is no longer registered), where `{op}` is `resolve`, `expire`, `cancel`, `suspend` or `resume` -- the `span.macp.{op}` groups of `macp-semconv/0.1`. Each carries `macp.operation.name`, `macp.session.id`, `macp.mode.id` and `macp.session.state`, the `SESSION_STATE_*` value the session **entered** (`resume` enters `SESSION_STATE_OPEN`). `resolve` also carries `macp.mode.version`, `macp.configuration.version` and `macp.policy.id`, plus `macp.commitment.hash` (the RFC-MACP-0013 `sha256:<hex>` hash of the payload) when a `Commitment` resolved a standards-track session; an extension or passthrough mode's resolve carries no hash, whatever its payload. `resolve` is a child of the resolving Envelope's `process` span; `cancel`, `suspend` and `resume` are children of their RPC's SERVER span. A resume that force-expires the session (past its suspension cap, or past the suspend/resume cycle cap) is an `expire`, not a `resume`. The handoff synthetic implicit accept leaves the session open, so it emits no transition span.

**Session anchor and links** -- The `process` span of a session's accepted `SessionStart` is its **anchor**. Every later span of the session -- each `process` span, whatever its outcome (accepted, duplicate or rejected), and each transition span -- carries exactly one span link to the anchor, so a backend can gather every trace that touched a session from one span. The anchor is cached in memory beside the session registry and also stored as a best-effort side record (see "Trace anchor" under the durability matrix): it is never written to the log, the session snapshot or a checkpoint, never replayed, and it changes nothing a client sees. It is recorded only for a session the runtime accepted, looked up only for a session resident in memory, and dropped from memory when the session leaves it (`MACP_SESSION_RETENTION_SECS` eviction, `MACP_SESSION_DISK_RETENTION_SECS` deletion, or any other removal, pruned on the next maintenance pass), so Envelopes naming arbitrary session ids cannot grow it. After a restart the first span of a resident session loads the stored anchor once (a miss is cached), so spans keep linking to the pre-restart SessionStart; a session whose anchor was never stored (started before telemetry was on, memory-only storage, or a crash right after its SessionStart) has no link.

**`expire` is a root span** -- No request causes an expiry, even when a request is what observes it: a message sent after the deadline, a read such as `GetSession`, a cancel or suspend that finds the session past its TTL, the background sweep every `MACP_CLEANUP_INTERVAL_SECS`, or a force-expiring resume. So `expire` always starts its own trace (it has no parent) and is reachable only through its link to the anchor; it is not in the trace of the request that observed it.

**Session metrics** -- In **every** build, `/metrics` also renders the semconv session families after the Envelope families: the counter `macp_session_transitions_total{macp_mode_id,macp_operation_name}` and the histogram `macp_session_duration_seconds_{bucket,sum,count}{macp_mode_id,macp_session_state}`, with bucket bounds 1 s to 24 h. The duration is recorded once per session, on its terminal transition (`RESOLVED`, `EXPIRED` or `CANCELLED`), as the time from `SessionStart` acceptance to the transition on the runtime's clock -- for an expiry, the moment the expiry was observed (the sweep or the late message), which is at or after `started_at + ttl_ms`; a suspension is not subtracted. An `otel` build with metrics export on also exports them over OTLP as `macp.session.transitions` (unit `{transition}`) and `macp.session.duration` (unit `s`). A mode no longer registered is labelled `_OTHER`, beyond 2,000 label sets new ones fold into `macp_mode_id="_OTHER"`, and no session identifier is ever a label. Neither family appears before the first transition. The legacy `macp_sessions_{resolved,expired,cancelled,suspended,resumed}_total{mode}` counters are bumped at the same points.

> **Deprecation.** The per-mode `macp_*_total{mode="..."}` families (`macp_sessions_started_total`, `macp_messages_accepted_total` and the rest) are a compatibility rendering, kept byte-identical until they are removed no earlier than the next minor release after the one that introduces their replacements (0.12.0 if that release is 0.11.x, 0.13.0 if it is 0.12.0). Move dashboards and alerts to the `macp_envelope_*` families.

**gRPC reflection** (dev/debug only) -- Build with the `reflection` Cargo feature (`cargo build --features reflection`) to enable the standard gRPC Server Reflection API, so tools like `grpcurl`/`grpcui` can call RPCs without a local `.proto`/`.protoset` file (e.g. `grpcurl -plaintext localhost:50051 list`). Off by default and never enabled in the published Docker image -- it exposes the full service/message schema to any client that can reach the port, so treat it the same as `MACP_ALLOW_INSECURE`: fine for a local instance or an ephemeral CI container, not for a production deployment. The runtime logs a warning at startup when it's active. Without this feature, point `grpcurl -proto`/`-protoset` at the published proto instead (`@multiagentcoordinationprotocol/proto` on npm, `macp-proto` on crates.io).

## Graceful shutdown

SIGINT (ctrl-c) and, on Unix, SIGTERM trigger the same graceful shutdown; non-Unix builds handle ctrl-c only. One shared signal feeds every listener (`src/shutdown.rs`), so none can miss it:

1. The gRPC server stops accepting connections and drains: requests already in flight run to completion.
2. The Prometheus listener (`MACP_METRICS_ADDR`) stops accepting at once and closes its port.
3. The HTTP listener, when `MACP_HTTP_BIND_ADDR` is set, stops accepting, closes its port and lets in-flight requests finish; its SSE streams end at once without an error event, so they never hold the drain open, and clients resume with `Last-Event-ID`.
4. A deadline of `MACP_SHUTDOWN_DRAIN_SECS` (default `10`) starts. Long-lived gRPC server streams (`StreamSession`, `WatchSessions`, `WatchSignals`, ...) never end by themselves, so with any open the runtime waits out the whole deadline, logs `drain deadline reached with connections still open; forcing shutdown` and stops anyway.
5. Every in-memory session is persisted (unless `MACP_MEMORY_ONLY=1`), the OTLP providers flush for at most 5 seconds (`otel` builds with telemetry on), and the process exits 0.

A second SIGINT or SIGTERM during the drain exits immediately with `128 + signo` (130 or 143), skipping steps 4 and 5. The accepted history is already durable (every accepted message is appended before it is acknowledged), so only the final session snapshot is lost, and recovery rebuilds it from the log.

Size the platform's grace period above the drain: Kubernetes `terminationGracePeriodSeconds` (default 30) or `docker stop -t` (default 10) must exceed `MACP_SHUTDOWN_DRAIN_SECS`, plus 5 seconds with telemetry on. With Docker's default of 10 seconds and the default drain of 10, an open stream lets SIGKILL arrive before the snapshot.

## Container deployment

Use the repository's `Dockerfile` (multi-stage, non-root `macp` user) to build
your own image, or pull the image this repo publishes to GitHub Container
Registry: `ghcr.io/multiagentcoordinationprotocol/macp-runtime`. The
image does **not** enable dev mode: the runtime refuses to start without
configured authentication and TLS unless you explicitly opt in for local
development:

```bash
# Production: configure auth + TLS, persist session logs on a volume, and pin
# an immutable release tag (see "Published image tags" below).
docker run -p 50051:50051 \
  -e MACP_AUTH_TOKENS_FILE=/etc/macp/tokens.json \
  -e MACP_TLS_CERT_PATH=/etc/macp/tls.crt -e MACP_TLS_KEY_PATH=/etc/macp/tls.key \
  -v ./secrets:/etc/macp -v macp-data:/home/macp/.macp-data \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:0.10.0

# Local development ONLY: any bearer token becomes a fully-privileged identity,
# and all state is lost when the container stops.
docker run -p 50051:50051 -e MACP_ALLOW_INSECURE=1 -e MACP_MEMORY_ONLY=1 \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:0.10.0
```

`0.10.0` is the newest release when this was written; check the
[releases](https://github.com/multiagentcoordinationprotocol/macp-runtime/releases)
for a newer one. The image sets `MACP_DATA_DIR=/home/macp/.macp-data` and runs
the runtime as the non-root user `macp`. Without `MACP_MEMORY_ONLY=1` or a volume there, the
development command still starts, but writes its logs into the container's
writable layer.

Images up to and including `0.9.0` do not create the data directory, so Docker
creates a new named volume's mount point as root and the runtime exits with
`Permission denied`. With those images, hand the volume to `macp` once before
the first start:

```bash
docker run --rm --user root --entrypoint chown \
  -v macp-data:/home/macp/.macp-data \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:0.9.0 macp:macp /home/macp/.macp-data
```

Later images create the directory as `macp`, and a new volume inherits that
ownership.

From 0.10.1 the container starts as root only so its entrypoint can hand
`MACP_DATA_DIR` to `macp` (recursively, when it is not already `macp`'s), and
then drops to `macp` with `setpriv` before the runtime starts. That covers what
a named volume's inherited ownership does not: bind mounts and platform volumes
(Railway, for one) that arrive root-owned. Started as another user (`--user`,
Kubernetes `runAsUser`), the entrypoint changes nothing and that user needs
write access to the directory. A cluster that enforces `runAsNonRoot` must set
`runAsUser` for that reason.

#### OpenTelemetry in the image

Images published after `0.11.0` are built with `--features otel,http-listener`
(the `Dockerfile` default; `docker.yml` and the CI `docker-build` gate pass
`--build-arg CARGO_FEATURES=otel,http-listener` explicitly). The `otel`
feature is inert until you configure it:

- **Zero egress without `OTEL_*`.** With none of `OTEL_EXPORTER_OTLP_ENDPOINT`,
  `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
  set (or with `OTEL_SDK_DISABLED=true`), the runtime installs no exporter and
  no propagator and opens **no connection** of any kind for telemetry. There is
  no built-in default endpoint: nothing dials `localhost:4317`. Its log output
  and gRPC responses are identical to the plain build's.
  `tests/otel_binary.rs` pins this against the real binary with a listener on
  `127.0.0.1:4317`, and `scripts/measure-otel-image.sh` compares the two
  images' responses and log lines.
- **Turning it on** takes one variable, for example
  `-e OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317`. See
  [OpenTelemetry](#opentelemetry) for the rest.
- **Cost.** Measured with `scripts/measure-otel-image.sh` on one host (Apple
  M4 Pro, Docker 28.4; `linux/arm64` native and `linux/amd64` emulated): the
  binary grows by about 3.5 MiB and the image by about 1 MiB compressed (5 MB
  unpacked). With no `OTEL_*` variable set, median startup-to-SERVING moved by
  under 3 ms and idle memory by under 1 MiB, both within run-to-run noise.

#### HTTP/JSON listener in the image

The `http-listener` feature is inert until `MACP_HTTP_BIND_ADDR` is set: the
container listens on 50051 only, logs nothing about HTTP and answers exactly
as an image without the feature. The CI `docker-build` gate checks both
sides on every PR: without `MACP_HTTP_*` the container's only listening
socket is 50051, and with `MACP_HTTP_BIND_ADDR=0.0.0.0:8080` `GetManifest`
over HTTP answers `200`. To turn it on, bind inside the container and publish
the port (the image `EXPOSE`s 8080 for this):

```bash
docker run -p 50051:50051 -p 8080:8080 \
  -e MACP_HTTP_BIND_ADDR=0.0.0.0:8080 \
  -e MACP_HTTP_PUBLIC_URL=https://macp.example.com \
  -e MACP_AUTH_TOKENS_FILE=/etc/macp/tokens.json \
  -e MACP_TLS_CERT_PATH=/etc/macp/tls.crt -e MACP_TLS_KEY_PATH=/etc/macp/tls.key \
  -v ./secrets:/etc/macp -v macp-data:/home/macp/.macp-data \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:<release after 0.11.0>
```

See [HTTP/JSON listener](#httpjson-listener) for the variables and the
reverse-proxy notes.

To build an image **without** either feature, pass an empty build argument;
to pick features for a self-built image, pass them explicitly:

```bash
docker build -t macp-runtime .                                          # otel,http-listener (default)
docker build --build-arg CARGO_FEATURES= -t macp-runtime:plain .        # neither
docker build --build-arg CARGO_FEATURES=otel -t macp-runtime:otel .     # otel only
```

`CARGO_FEATURES` is passed verbatim to `cargo build --release --features`;
empty builds the plain binary.

When deploying in containers:

- Mount a persistent volume at `MACP_DATA_DIR` so session logs survive container restarts.
- Expose port 50051 (or the port configured via `MACP_BIND_ADDR`), and the HTTP port when `MACP_HTTP_BIND_ADDR` is set.
- Provide TLS certificates and auth tokens via mounted secrets.
- Set `MACP_BIND_ADDR=0.0.0.0:50051` to accept connections from outside the container.

### Published image tags

Every image is multi-arch (`linux/amd64`, `linux/arm64`) with SLSA provenance
and an SBOM attached, built by `.github/workflows/docker.yml`. Every image
published after `0.11.0`, release or branch build, is built with
`--features otel,http-listener` (inert until an `OTEL_EXPORTER_OTLP_*`
endpoint, respectively `MACP_HTTP_BIND_ADDR`, is set; see
[OpenTelemetry in the image](#opentelemetry-in-the-image) and
[HTTP/JSON listener in the image](#httpjson-listener-in-the-image)); `0.11.0`
and earlier are plain builds. Every push to
`main` (a "branch build") publishes three of these together, always pointing
at the same digest; a release publishes the other two, also always sharing a
digest with each other but never with a branch build's:

| Tag | Mutability | Meaning |
|-----|------------|---------|
| `:X.Y.Z` (e.g. `0.9.0`) | Immutable | Exactly one release. Pin this for production. |
| `:X.Y` (e.g. `0.9`) | Moves within the minor | Always the newest `X.Y.Z` patch published so far. |
| `:latest` | Moves on every push to `main` | The tip of `main` — **not** the newest release. Do not use `latest` in production; it is not a release channel. |
| `:main` | Moves on every push to `main` | Synonymous with `:latest` today (both are branch-build tags on the one `push: branches: [main]` trigger) — documented separately since nothing in `docker.yml` guarantees they stay synonymous if a second branch trigger is ever added. |
| `:<sha>` (e.g. `:bb45705`) | Immutable | One image per commit pushed to `main` (branch builds only — a release build never gets a SHA tag; see below). |

A release commit **is** `main`'s tip at the moment it's cut, but `main` moves
on immediately afterward as later commits land — so by the time you read
this, `latest`/`main`/`:<sha>` are very likely already ahead of every semver
tag. Never assume `latest` matches the newest release.

**A `:X.Y.Z` tag and its release commit's `:<sha>` tag do not share a
digest**, even though they're built from the same source tree: the release
build and the branch build for that commit run as two separate, concurrent
CI jobs, and each stamps its own `org.opencontainers.image.created`
timestamp. Same source, two images — don't expect `docker inspect` to report
identical digests across them.

Every image's `org.opencontainers.image.revision` label names the exact
commit it was built from — this is reliable even for a manually backfilled
release image (a `workflow_dispatch` naming an older tag), where
`org.opencontainers.image.revision` still correctly reports the tag's
commit. The one residual gap on a manual backfill: buildkit's SLSA
provenance *attestation* records the runner's own `GITHUB_SHA` (the dispatch
commit, i.e. whatever `main` was at dispatch time), not the backfilled tag's
commit — only the `revision` label is authoritative there. Every image
published by the normal, automatic release path (cutting a release via
release-plz) has correct provenance with no such gap.

**Do not pin the `macp-runtime-v*` git tag format as an image tag** — GHCR
image tags drop the `macp-runtime-v` prefix (`macp-runtime-v0.8.1` → image
tag `0.8.1`). `docker.yml`'s resolve step strips it deliberately, to emit a
bare semver value for `docker/metadata-action`'s `{{version}}`/
`{{major}}.{{minor}}` patterns — the prefix is redundant once the image
already lives in the `macp-runtime` repository path, not a Docker tag
syntax restriction (`macp-runtime-v0.8.1` would itself be a perfectly valid
tag string).

## Development tools

For development and CI, these additional tools are useful:

- `cargo-tarpaulin` for coverage reporting
- `cargo-audit` for dependency security auditing
- `buf` for protocol buffer linting and management
