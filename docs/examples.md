# Examples

The runtime ships with example clients in `src/bin` that demonstrate each coordination mode. This page walks through what each example does and highlights the runtime behavior you should pay attention to.

For protocol-level example transcripts, see the [protocol examples documentation](https://www.multiagentcoordinationprotocol.io/docs/examples).

All examples target a development runtime on `127.0.0.1:50051`, from the published image or from a checkout:

```bash
docker run -p 50051:50051 -e MACP_ALLOW_INSECURE=1 -e MACP_MEMORY_ONLY=1 \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:0.9.0
# or, from a checkout:
MACP_ALLOW_INSECURE=1 MACP_MEMORY_ONLY=1 cargo run
```

The example binaries attach `Authorization: Bearer <sender>` metadata so the runtime derives the authenticated sender from the bearer token. Every example creates a session with the required fields: `participants`, `mode_version` (`"1.0.0"`), `configuration_version` (`"config.default"`), and a positive `ttl_ms`.

## Decision Mode

```bash
cargo run --bin client
```

The coordinator initializes the connection, lists available modes, starts a Decision session, submits a proposal, receives an evaluation and a vote from participants, then commits the outcome. After commitment, it queries the session with `GetSession` to confirm the terminal state.

The runtime enforces automatic phase progression: the first evaluation moves the session from the Proposal phase to Evaluation, and the first vote moves it to Voting. Once in the Voting phase, new proposals are rejected. Commitment version fields must match the bound session versions.

## Proposal Mode

```bash
cargo run --bin proposal_client
```

A buyer starts a session, the seller creates a proposal, the buyer counters with a different offer, both participants accept the same live proposal, and the buyer commits. The session does not resolve merely because a proposal exists -- convergence (all required parties accepting the same proposal) must be reached before a commitment is accepted.

## Task Mode

```bash
cargo run --bin task_client
```

A planner starts a session, sends a task request, a worker accepts the task, sends a progress update, and reports completion. The planner then commits the result. Only the active assignee can send task updates -- this is enforced against the authenticated sender identity.

## Handoff Mode

```bash
cargo run --bin handoff_client
```

An owner starts a session, sends a handoff offer with context, and the target agent accepts the offer. The owner commits to finalize the responsibility transfer. Only one outstanding offer is allowed at a time, and once an offer is accepted, no further offers can be issued.

## Quorum Mode

```bash
cargo run --bin quorum_client
```

A coordinator starts a session, sends an approval request, participants submit their ballots, and the coordinator commits once the approval threshold is met. The runtime accepts a commitment either when enough approvals exist or when the threshold becomes mathematically unreachable.

## Multi-Round Mode

```bash
cargo run --bin multi_round_client
```

A coordinator starts a session using `ext.multi_round.v1`, participants exchange contributions across multiple rounds, and the runtime tracks convergence (all participants contributing the same value). Convergence does not auto-resolve the session -- an explicit commitment is still required. This mode is discoverable via `ListExtModes`, not `ListModes`.

## StreamSession

`StreamSession` provides per-session bidirectional streaming. A stream is bound to a session by sending the first session-scoped envelope. From that point, the client receives all accepted envelopes for that session in real time.

Key behaviors to note:

- Use `SessionStart` to create a new session over the stream, or send a session-scoped message to attach to an existing one.
- For observers and late joiners, send a passive-subscribe frame (`subscribe_session_id` + `after_sequence`) as the first frame -- the runtime replays accepted history and then delivers live envelopes on the same stream. `after_sequence` is the 1-based ordinal of accepted session-scoped envelopes and is **exclusive**: replay resumes at `after_sequence + 1`, and `0` replays from the session's first accepted envelope (RFC-MACP-0006 §3.2 "Sequence semantics").
- Mixed-session streams (envelopes targeting different sessions) are rejected.
- A single frame must not carry both `envelope` and `subscribe_session_id` -- the stream terminates with `InvalidArgument`.
- Application errors are delivered inline without closing the stream.

## HTTP/JSON binding (curl)

The same RPCs are served as HTTP/JSON (RFC-MACP-0006 §4) by a runtime built with the `http-listener` Cargo feature (the published image includes it) and started with `MACP_HTTP_BIND_ADDR`. The binding is described in [API: HTTP/JSON binding](API.md#httpjson-binding). Start a development runtime with the listener on port 8080:

```bash
docker run -p 50051:50051 -p 8080:8080 \
  -e MACP_ALLOW_INSECURE=1 -e MACP_MEMORY_ONLY=1 -e MACP_HTTP_BIND_ADDR=0.0.0.0:8080 \
  ghcr.io/multiagentcoordinationprotocol/macp-runtime:latest
# or, from a checkout:
MACP_ALLOW_INSECURE=1 MACP_MEMORY_ONLY=1 MACP_HTTP_BIND_ADDR=127.0.0.1:8080 \
  cargo run --features http-listener
```

Images up to and including `0.11.0` do not include the listener; until a release after it is published, use `:latest` or build from a checkout. Dev-mode auth is active (no `MACP_AUTH_*` set), so each `Authorization: Bearer <value>` header authenticates the caller as sender `<value>`. Every request is a `POST` with `Content-Type: application/json` and a JSON body; a body is the canonical JSON of the request message (RFC-MACP-0001 §10), and a `payload` with a schema is plain JSON.

<!-- Every block in this section marked `check-http-example` is executed, in order, by
     scripts/check-http-examples.sh in CI: `status=` is the expected HTTP status, `expect=`
     a string the output must contain (`%20` stands for a space), `stream` runs the block in the background until
     its `expect=` string arrives, `setup` runs it with no checks. A curl block here
     without a marker fails the script. -->

<!-- check-http-example setup -->
```bash
export MACP_HTTP=${MACP_HTTP:-http://127.0.0.1:8080}
SID=$(python3 -c 'import uuid; print(uuid.uuid4())')   # session ids are UUID v4/v7 or base64url
```

### Send

Start a Decision session. `sender` may be omitted: the runtime derives it from the credential.

<!-- check-http-example status=200 expect="ok":true -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://orchestrator' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
  "message_type": "SessionStart", "message_id": "m1", "session_id": "$SID",
  "payload": {"intent": "pick a vendor",
    "participants": ["agent://orchestrator", "agent://security"],
    "mode_version": "1.0.0", "configuration_version": "config.default", "ttl_ms": 600000}}}
EOF
```

```json
{"ack":{"accepted_at_unix_ms":1793502846950,"duplicate":false,"message_id":"m1","ok":true,"session_id":"…","session_state":"SESSION_STATE_OPEN"}}
```

A proposal from the orchestrator and a vote from the security agent:

<!-- check-http-example status=200 expect="ok":true -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://orchestrator' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
  "message_type": "Proposal", "message_id": "m2", "session_id": "$SID",
  "payload": {"proposal_id": "p1", "option": "vendor-a", "rationale": "cheapest"}}}
EOF
```

<!-- check-http-example status=200 expect="ok":true -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://security' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
  "message_type": "Vote", "message_id": "m3", "session_id": "$SID",
  "payload": {"proposal_id": "p1", "vote": "APPROVE", "reason": "passes review"}}}
EOF
```

### GetSession

<!-- check-http-example status=200 expect="state":"SESSION_STATE_OPEN" -->
```bash
curl -sS "$MACP_HTTP/macp/v1/GetSession" \
  -H 'Authorization: Bearer agent://security' -H 'Content-Type: application/json' \
  -d "{\"session_id\": \"$SID\"}"
```

The orchestrator commits, which resolves the session (ordinal 4):

<!-- check-http-example status=200 expect="session_state":"SESSION_STATE_RESOLVED" -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://orchestrator' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
  "message_type": "Commitment", "message_id": "m4", "session_id": "$SID",
  "payload": {"commitment_id": "c1", "action": "decision.selected",
    "authority_scope": "procurement", "reason": "approved",
    "mode_version": "1.0.0", "configuration_version": "config.default",
    "outcome_positive": true}}}
EOF
```

### Refusals: in the Ack, or request-level

A refused Envelope is still `200`: the decision is in the Ack (`ok: false` and an `error`), exactly as on gRPC. A proposal to the resolved session:

<!-- check-http-example status=200 expect="code":"SESSION_NOT_OPEN" -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://orchestrator' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "macp.mode.decision.v1",
  "message_type": "Proposal", "message_id": "m5", "session_id": "$SID",
  "payload": {"proposal_id": "p2", "option": "vendor-b", "rationale": "late"}}}
EOF
```

Any other RPC's refusal, and a `Send` the runtime cannot read (no credential, body over the cap, not a `SendRequest`), is request-level: the registry's HTTP status, a `MACP-Error-Code` header and `{"error": …}`. Without a credential:

<!-- check-http-example status=401 expect="code":"UNAUTHENTICATED" -->
```bash
curl -sS -i "$MACP_HTTP/macp/v1/GetSession" \
  -H 'Content-Type: application/json' -d "{\"session_id\": \"$SID\"}"
```

```text
HTTP/1.1 401 Unauthorized
content-type: application/json
macp-error-code: UNAUTHENTICATED
www-authenticate: Bearer

{"error":{"code":"UNAUTHENTICATED","details":"","message":"Unauthenticated","message_id":"","session_id":""}}
```

### Subscribe with resume

`StreamSession`'s passive-subscribe form is a server-sent event stream. Each envelope event's `id:` is its ordinal; `Last-Event-ID: 2` (or `"after_sequence": 2` in the body) resumes after ordinal 2, so the replay starts at the vote. The session is resolved, so the stream ends normally after the Commitment; on an open session it stays open with live envelopes and a `: keepalive` comment after 30 s of silence.

<!-- check-http-example status=200 expect=id:%204 -->
```bash
curl -sS -N "$MACP_HTTP/macp/v1/StreamSession" \
  -H 'Authorization: Bearer agent://security' -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' -H 'Last-Event-ID: 2' \
  -d "{\"subscribe_session_id\": \"$SID\"}"
```

```text
id: 3
data: {"envelope":{"macp_version":"1.0","message_id":"m3","message_type":"Vote",…}}

id: 4
data: {"envelope":{"macp_version":"1.0","message_id":"m4","message_type":"Commitment",…}}
```

### WatchSignals

Watch the ambient plane in one terminal (the stream has no natural end; stop it with ctrl-c):

<!-- check-http-example stream status=200 expect="signal_type":"progress" -->
```bash
curl -sS -N "$MACP_HTTP/macp/v1/WatchSignals" \
  -H 'Authorization: Bearer agent://observer' -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' -d '{}'
```

and send a Signal from another (empty `mode` and `session_id`; `data` is bytes, so base64):

<!-- check-http-example status=200 expect="ok":true -->
```bash
curl -sS "$MACP_HTTP/macp/v1/Send" \
  -H 'Authorization: Bearer agent://telemetry' -H 'Content-Type: application/json' \
  --data @- <<EOF
{"envelope": {"macp_version": "1.0", "mode": "", "message_type": "Signal",
  "message_id": "sig-1", "session_id": "",
  "payload": {"signal_type": "progress", "data": "AAEC", "confidence": 0.9,
    "correlation_session_id": "$SID"}}}
EOF
```

The watcher receives one event per Signal, with no `id:` (watch streams carry none). Under dev-mode auth `sender_authenticated` is `false`:

```text
data: {"envelope":{"macp_version":"1.0","message_id":"sig-1","message_type":"Signal","mode":"","payload":{…,"signal_type":"progress"},"sender":"agent://telemetry",…},"sender_authenticated":false}
```

### Discovery

The well-known manifest needs no credential:

<!-- check-http-example status=200 expect=macp.mode.decision.v1 -->
```bash
curl -sS "$MACP_HTTP/.well-known/macp.json"
```

## Extension Mode Lifecycle

The runtime supports dynamic extension management through four RPCs:

1. **`ListExtModes`** discovers available extensions (including `ext.multi_round.v1`).
2. **`RegisterExtMode`** registers a new extension with a mode descriptor. The runtime creates a passthrough handler for it.
3. **`UnregisterExtMode`** removes a dynamic extension (built-in modes are protected).
4. **`PromoteMode`** promotes an extension to standards-track, optionally renaming it -- except into the reserved `macp.mode.*` namespace, which this runtime refuses unconditionally (RFC-MACP-0002 §12 permits it under conditions the runtime cannot verify, so it does not try).

Extension mode names must not use the reserved `macp.mode.*` namespace. All registry changes are broadcast to `WatchModeRegistry` subscribers, and both `GetManifest` and `Initialize` include all modes.

## Error Path Testing

```bash
cargo run --bin fuzz_client
```

This client deliberately exercises failure paths: invalid protocol versions, empty modes, malformed payloads, duplicate messages, unauthorized sender spoofing, oversized payloads, and session access without membership. Use it to verify that the runtime rejects invalid requests correctly.

## Troubleshooting

**`UNAUTHENTICATED`**: Start the runtime without `MACP_AUTH_*` configured (so dev-mode auth is active) and ensure the client sets `Authorization: Bearer <sender>`. With auth resolvers configured, send a matching static bearer or JWT instead.

**`INVALID_ENVELOPE` on SessionStart**: Verify that the mode name is canonical, the payload is not empty, and all four required fields (`mode_version`, `configuration_version`, `ttl_ms > 0`, `participants`) are present.

**`SESSION_NOT_OPEN`**: The session has already been resolved or expired. Use `GetSession` to confirm the terminal state.

**`RATE_LIMITED`**: Increase the limits if appropriate:

```bash
export MACP_SESSION_START_LIMIT_PER_MINUTE=120
export MACP_MESSAGE_LIMIT_PER_MINUTE=2000
```
