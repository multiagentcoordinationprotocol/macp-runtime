# SDK Developer Guide

This guide is for developers building client libraries that connect to the MACP Runtime. It covers the practical patterns your SDK needs to implement: envelope construction, authentication, error handling, streaming, and retry logic.

For protocol-level SDK conformance requirements, see the [protocol SDK parity documentation](https://www.multiagentcoordinationprotocol.io/docs/sdk-parity). For transport binding specifications, see the [protocol transports documentation](https://www.multiagentcoordinationprotocol.io/docs/transports).

**Reference implementations.** [`macp-sdk-python`](https://github.com/multiagentcoordinationprotocol/macp-sdk-python) (`pip install macp-sdk-python`) and [`macp-sdk-typescript`](https://github.com/multiagentcoordinationprotocol/macp-sdk-typescript) (`npm install macp-sdk-typescript`) are full client implementations of the contract this page describes. Installation, language-specific API, and client usage patterns live in those repos' own docs -- this page specifies only the runtime-side gRPC contract an SDK must satisfy, not how to use either SDK.

## What your SDK should handle

A well-built MACP SDK takes care of seven concerns so that application code can focus on coordination logic:

1. **gRPC transport** -- Connection management, TLS configuration, and metadata injection.
2. **Authentication** -- Storing the caller's bearer credential (opaque static token or JWT) and attaching it to every request as `Authorization: Bearer <token>`.
3. **Envelope construction** -- Building protobuf-encoded envelopes with correct version and mode fields.
4. **Message ID generation** -- Producing unique IDs for deduplication, and never generating one in the runtime-reserved `implicit-accept:` namespace.
5. **Session ID generation** -- Creating IDs in an accepted format (UUID v4/v7 or base64url).
6. **Error handling** -- Distinguishing transient from permanent failures and applying appropriate retry logic.
7. **Streaming** -- Managing `StreamSession` connections (including passive subscribe), handling inline errors, and recovering from lag.

The runtime treats JWT and opaque static tokens interchangeably at the wire level: both travel in `Authorization: Bearer`. A JWT is detected by the presence of dots in the token; otherwise it is treated as opaque. SDKs do not need to know which resolver the server is running.

## Starting a connection

Every SDK session should begin with an `Initialize` call:

```
-> InitializeRequest {
     supported_protocol_versions: ["1.0"],
     client_info: { name: "my-sdk", version: "1.0.0" }
   }
<- InitializeResponse {
     selected_protocol_version: "1.0",
     capabilities: { sessions: { stream: true }, ... },
     supported_modes: ["macp.mode.decision.v1", ...]
   }
```

Cache the response. Use `selected_protocol_version` as the `macp_version` in all subsequent envelopes. Check `capabilities` to determine which features are available and store `supported_modes` for client-side validation before sending.

## Building envelopes

Every message to the runtime is wrapped in an `Envelope`:

```protobuf
message Envelope {
  string macp_version = 1;       // Always "1.0"
  string mode = 2;               // Mode identifier (empty for signals)
  string message_type = 3;       // "SessionStart", "Proposal", etc.
  string message_id = 4;         // Unique per message
  string session_id = 5;         // Target session (empty for signals)
  string sender = 6;             // Set by SDK, overridden by runtime
  int64 timestamp_unix_ms = 7;   // Current time in milliseconds
  bytes payload = 8;             // Protobuf-encoded mode-specific payload
}
```

This mirrors `macp-proto`'s own `Envelope` definition (RFC-MACP-0001 §6) -- diff against that crate if this table and the wire format ever disagree.

The runtime overrides `envelope.sender` with the authenticated identity. If the SDK sets a sender that does not match, the request is rejected. The safest approach is to either leave `sender` empty or set it to the expected authenticated identity.

**Message IDs** must be unique per sender. UUID v4 is a good default. The runtime deduplicates on `message_id`, so sending the same ID twice returns a duplicate acknowledgement without reprocessing. In a `macp.mode.handoff.v1` session at `semantics_rev >= 2`, a client `message_id` must not begin with the literal `implicit-accept:` -- that namespace is reserved for the runtime's own synthetic accept and is rejected with `INVALID_ENVELOPE` whatever the message type. See [Runtime API § Send](API.md#send) for the full rule and RFC-MACP-0010 §5.1(3) for the normative source.

**Session IDs** must be UUID v4/v7 (hyphenated lowercase, 36 characters) or base64url tokens (22+ characters). UUID v4 is the simplest choice.

**Payloads** are protobuf-encoded bytes. Import the mode-specific `.proto` files, serialize the payload struct, and set `envelope.payload` to the resulting bytes.

## Error handling

### Error categories

The canonical error code list, with HTTP status mappings and the `permanent`/`deprecated` status each code carries, is the spec repo's [error code registry](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/blob/main/registries/error-codes.md) (`registries/error-codes.md`) -- this page does not restate it. The table below is **runtime-local operational advice layered on top of** that registry, not an alternate classification: the registry marks every code `permanent` (including `RATE_LIMITED` and `INTERNAL_ERROR`), and "Transient" below means *worth retrying in practice*, not a registry status.

| Category | Examples | Retry? |
|----------|---------|--------|
| **Transient** (operational advice, not a registry status) | `RATE_LIMITED`, `INTERNAL_ERROR`, network timeout | Yes, with backoff |
| **Envelope errors** | `INVALID_ENVELOPE`, `INVALID_SESSION_ID`, `PAYLOAD_TOO_LARGE` | No -- fix the request |
| **State errors** | `SESSION_NOT_FOUND`, `SESSION_NOT_OPEN`, `SESSION_ALREADY_EXISTS` | No -- session state is permanent |
| **Auth errors** | `UNAUTHENTICATED`, `FORBIDDEN` | No -- fix credentials or permissions |
| **Policy errors** | `POLICY_DENIED`, `UNKNOWN_POLICY_VERSION` | No -- fix policy or session configuration |

### Idempotency

The `Send` RPC is idempotent on `message_id`. If a network error occurs after sending but before receiving the acknowledgement, the SDK can safely retry with the same `message_id`. The runtime returns `Ack { ok: true, duplicate: true }` for already-processed messages.

### Retry strategies

For `RATE_LIMITED` errors, wait for the rate window to expire (default: 60-second sliding window) or use exponential backoff starting at 1 second with a 30-second cap.

For `INTERNAL_ERROR` or network failures, retry with the same `message_id` using exponential backoff (100ms, 200ms, 400ms, 800ms) up to 5 attempts with a 10-second cap.

For `ResourceExhausted` on a stream (lag detection), reconnect the stream, call `GetSession` to verify the current state, and resume from there.

## Streaming

### When to use Send vs StreamSession

Use `Send` when you need an explicit acknowledgement per message, or for fire-and-forget with retry (idempotent via `message_id`). Use `StreamSession` for real-time observation of a session or high-frequency message exchange.

### StreamSession lifecycle

A `StreamSession` connection follows this pattern:

1. Open a bidirectional stream.
2. Send the first envelope, which binds the stream to that `session_id`. Alternatively, send a **passive subscribe** frame (RFC-MACP-0006-A1) where `envelope` is absent and `subscribe_session_id` is set -- the runtime replays accepted history and then delivers live envelopes on the same stream. `after_sequence` is the 1-based ordinal of accepted session-scoped envelopes and is **exclusive**: replay resumes at `after_sequence + 1`, and `0` replays from the session's first accepted envelope (RFC-MACP-0006 §3.2 "Sequence semantics").
3. Receive accepted envelopes from all participants in the session.
4. Send additional envelopes as needed (not required for passive observers).
5. The stream closes on client disconnect, lag overflow, auth failure, or server shutdown.

All envelopes on a stream must target the same session. A single frame must not set both `envelope` and `subscribe_session_id` -- the stream terminates with `InvalidArgument` if both are set. Passive subscribe is authorized for the session initiator, declared participants, and observer identities; non-participants receive an inline `FORBIDDEN` error frame without closing the stream.

Two further resume rules your SDK must implement (RFC-MACP-0006 §3.2 "Sequence semantics"):

- **Compaction.** A resume whose `after_sequence` falls below the compacted base is rejected with `FAILED_PRECONDITION` -- `session history before ordinal {base} was compacted; resume with after_sequence >= {base} or re-read state via GetSession` -- so retry at or above the reported base rather than falling back to `0`.
- **Redelivery.** Key duplicate detection on `message_id`, and do not let a redelivered envelope advance your sequence position -- only a distinct accepted envelope does.

Application-level errors (validation failures, authorization denials) are delivered as inline `MACPError` messages and the stream stays open. Transport-level errors (unauthenticated, internal, unknown session on subscribe) close the stream.

In a `macp.mode.handoff.v1` session, a stream can deliver a `HandoffAccept` that **no participant sent**: the runtime's synthetic implicit accept. It consumes an accepted ordinal and replays identically to any other accepted envelope, but its payload carries `implicit = true` and its `timestamp_unix_ms` is the computed deadline, not the time your SDK observed it. A client must never originate one. See [Handoff implicit accept](modes.md#implicit-accept-rfc-macp-0010-51).

### Handling stream lag

The runtime's broadcast buffer holds 256 envelopes per session. If a client falls behind, the stream terminates with `ResourceExhausted`. Your SDK should detect this, reconnect with a new stream, and use a passive-subscribe frame with `after_sequence` set to the ordinal of the last accepted envelope it saw -- the runtime will replay missed history and then resume live delivery on the same stream.

### Observer identities

Non-participant agents (audit agents, dashboards, read-only observers) should be provisioned with an identity that carries `is_observer: true`. Observers can open passive-subscribe streams for any session and consume its accepted history and live envelopes without being listed in the session's `participants`. Attempting to `Send` into a session as an observer still requires normal participation rules, so passive observation does not bypass mode authority.

## Capability negotiation

After `Initialize`, check the runtime's capabilities to determine what features are available:

```python
resp = client.initialize(...)

if resp.capabilities.sessions.stream:
    # StreamSession is available
if resp.capabilities.cancellation.cancel_session:
    # CancelSession is available
if resp.capabilities.policy_registry.register_policy:
    # Policy management is available
```

SDKs should degrade gracefully when capabilities are absent rather than failing.

## Version negotiation

Send supported versions in descending preference order. The runtime selects the highest mutual version. If no match exists, it returns `UNSUPPORTED_PROTOCOL_VERSION`.

Unknown fields in protobuf messages are silently ignored, so SDKs built for protocol version 1.0 will work with a 1.1 runtime -- new fields are always optional.

## Testing your SDK

### Against a local runtime

```bash
MACP_ALLOW_INSECURE=1 cargo run
# SDK connects to localhost:50051 sending Authorization: Bearer <sender-id>
```

### Test checklist

- Initialize with version negotiation
- SessionStart with all required fields
- Full message flow through to commitment for each mode
- Duplicate message handling (same `message_id` returns duplicate ack)
- Error paths (invalid payload, forbidden, session not found)
- StreamSession connect, receive, and lag recovery
- GetSession returns correct metadata
- CancelSession by initiator succeeds, by non-initiator fails

## Proto files

Proto definitions are available in the `macp-proto` crate, currently pinned at `0.1.10`
(root `Cargo.toml`'s `[workspace.dependencies]`) -- check that pin rather than trusting
this list if a mode's `.proto` file is missing from it:

```
macp/v1/envelope.proto                      -- Envelope, Ack, MACPError
macp/v1/core.proto                          -- All RPCs, SessionStartPayload, CommitmentPayload
macp/v1/policy.proto                        -- PolicyDescriptor, policy RPCs
macp/modes/decision/v1/decision.proto       -- Decision mode payloads
macp/modes/proposal/v1/proposal.proto       -- Proposal mode payloads
macp/modes/task/v1/task.proto               -- Task mode payloads
macp/modes/handoff/v1/handoff.proto         -- Handoff mode payloads
macp/modes/quorum/v1/quorum.proto           -- Quorum mode payloads
macp/modes/multi_round/v1/multi_round.proto -- multi_round extension mode payloads (Contribute)
```

Generate language-specific bindings using `protoc` or `buf`. The TypeScript SDK instead
consumes these as a prebuilt package, `@multiagentcoordinationprotocol/proto`, distributed
via GitHub Packages (requires an `.npmrc` entry and a GitHub PAT with `read:packages`
scope) -- see that SDK's own README for setup.
