# API Reference

This is the reference for all 24 gRPC RPCs exposed by the MACP Runtime on `macp.v1.MACPRuntimeService`. The default endpoint is `127.0.0.1:50051`, configurable via `MACP_BIND_ADDR`.

For protocol-level transport semantics, see the [protocol transports documentation](https://www.multiagentcoordinationprotocol.io/docs/transports).

## Protocol Handshake

### Initialize

Every client session should begin with an `Initialize` call to negotiate the protocol version and discover runtime capabilities.

```protobuf
rpc Initialize(InitializeRequest) returns (InitializeResponse)
```

The client sends its supported protocol versions in descending preference order. The runtime selects the highest mutually supported version and returns it along with its identity, capabilities, and supported modes.

**Request fields**:
| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `supported_protocol_versions` | repeated string | Yes | Versions in descending preference |
| `client_info` | ClientInfo | No | Client name, version, description |
| `capabilities` | Capabilities | No | Client capabilities |

**Response fields**:
| Field | Type | Description |
|-------|------|-------------|
| `selected_protocol_version` | string | Selected mutual version |
| `runtime_info` | RuntimeInfo | `name: "macp-runtime"`, `version: "0.5.0"` (tracks the crate version) |
| `capabilities` | Capabilities | Runtime capabilities (streaming, cancellation, policy, etc.) |
| `supported_modes` | repeated string | All supported mode identifiers |
| `instructions` | string | Optional human-readable guidance |

Returns `UNSUPPORTED_PROTOCOL_VERSION` if no mutual version exists.

**Capabilities advertised**: `sessions.stream`, `sessions.list_sessions`, `sessions.watch_sessions`, `cancellation.cancel_session`, `progress.progress`, `manifest.get_manifest`, `mode_registry.list_modes`, `mode_registry.list_changed`, `roots.list_roots`, `roots.list_changed`, `policy_registry.register_policy`, `policy_registry.list_policies`, `policy_registry.list_changed`.

## Message Transport

### Send

The primary RPC for submitting messages. Accepts a single envelope and returns an acknowledgement indicating whether the message was accepted.

```protobuf
rpc Send(SendRequest) returns (SendResponse)
```

**Envelope fields**:
| Field | Type | Description |
|-------|------|-------------|
| `macp_version` | string | Must be `"1.0"` |
| `mode` | string | Mode identifier (empty for signals) |
| `message_type` | string | `"SessionStart"`, `"Proposal"`, `"Commitment"`, `"Signal"`, etc. |
| `message_id` | string | Unique ID for deduplication |
| `session_id` | string | Target session (empty for signals) |
| `sender` | string | Overridden by runtime with authenticated identity |
| `timestamp_unix_ms` | int64 | Client timestamp (informational) |
| `payload` | bytes | Protobuf-encoded mode-specific payload |

**Ack fields**:
| Field | Type | Description |
|-------|------|-------------|
| `ok` | bool | Whether the message was accepted |
| `duplicate` | bool | True if `message_id` was already processed |
| `message_id` | string | Echo of the submitted ID |
| `session_id` | string | Session the message was applied to |
| `accepted_at_unix_ms` | int64 | Server acceptance timestamp |
| `session_state` | SessionState | Session state after processing |
| `error` | MACPError | Present when `ok` is false |

The runtime overrides `envelope.sender` with the authenticated identity. If the envelope contains a non-empty `sender` that does not match the authenticated identity, the request is rejected with `UNAUTHENTICATED`.

### StreamSession

Provides bidirectional streaming scoped to a single session. Clients send envelopes and receive all accepted envelopes for that session in real time.

```protobuf
rpc StreamSession(stream StreamSessionRequest) returns (stream StreamSessionResponse)
```

The first envelope on the stream binds it to a `session_id`. All subsequent envelopes must target the same session. Responses contain either an accepted `envelope` or an application-level `error` (the stream stays open for application errors). If the client falls behind the broadcast buffer, the stream terminates with `ResourceExhausted`.

**Passive subscribe** (RFC-MACP-0006-A1). A client may observe a session without sending envelopes by sending a request frame where `envelope` is absent and `subscribe_session_id` is set. The runtime replays the session's accepted history starting at log index `after_sequence` (0 = replay from session start) and then delivers live envelopes on the same stream. A single frame must not contain both an `envelope` and `subscribe_session_id` -- the stream terminates with `InvalidArgument` if both are set. Subscribes bind the stream to the given session just like a first envelope; mixing session IDs on the same stream is rejected. Authorization: the caller must be the session initiator, a declared participant, or hold the `is_observer` identity capability. Non-participants receive an inline `FORBIDDEN` error frame and the stream stays open.

## Session Lifecycle

### GetSession

Retrieves metadata and current state for a session.

```protobuf
rpc GetSession(GetSessionRequest) returns (GetSessionResponse)
```

Returns `SessionMetadata` with the session's mode, state, TTL deadline, bound versions, participants, per-participant activity summaries, and initiator identity. Only the session initiator and declared participants can query a session.

### ListSessions

Enumerates metadata for the sessions currently held in the registry (including terminal sessions still within the retention window), **one bounded page per call**.

```protobuf
rpc ListSessions(ListSessionsRequest) returns (ListSessionsResponse)
```

**Request fields.**

| Field | Type | Meaning |
|---|---|---|
| `page_size` | `int32` | Maximum entries in this page. `0` means "server default" -- see below. |
| `page_token` | `string` | Opaque continuation token from a previous response's `next_page_token`. Empty starts a new traversal. |

**Page-size default and clamp.**

- `page_size = 0` yields the server default, `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` (default `100`).
- `page_size` above `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` (default `1000`) is clamped down to that maximum. The clamp is silent -- the response simply carries at most the maximum number of entries, with `next_page_token` set if more remain.
- Otherwise `page_size` is honored as requested.

See [Resource limits](#resource-limits) for both variables.

**`INVALID_ARGUMENT` is returned when:**

- `page_size` is negative (`INVALID_ARGUMENT: page_size must not be negative`), or
- `page_token` is non-empty and cannot be decoded as a continuation token -- oversized, not valid base64url, not valid UTF-8, missing this runtime's `v1:` token-version prefix, or carrying an empty cursor after that prefix (`INVALID_ARGUMENT: page_token is not a valid continuation token`). The message is deliberately identical for every rejection cause, so the token is not an oracle for which check failed.

The decode is a **format check, not a provenance check**. The token is unsigned and carries no proof it was minted by this runtime, so any correctly-formed token a caller synthesizes is accepted and used as a cursor. Do not treat a page token as an authenticated capability -- see [Observation-surface authorization](deployment.md#observation-surface-authorization) for why that is safe for this RPC today, and the condition that would void it.

**Response.** A `sessions` array of `SessionMetadata` entries plus a `next_page_token`. Pass `next_page_token` back **verbatim** in the next request's `page_token` and stop when it comes back empty:

```text
token = ""
loop:
    resp = ListSessions(page_size = 100, page_token = token)
    process(resp.sessions)
    token = resp.next_page_token
    if token == "":
        break
```

Do not parse, truncate, or synthesize a token -- it is opaque and its format may change between runtime versions.

**Traversal semantics.**

> Traversal is a keyset scan over session IDs in ascending byte order. Every session that exists for the whole traversal is returned exactly once. A session created during a traversal is returned if and only if its ID sorts after the current cursor. A session deleted during a traversal may or may not appear, depending on whether the cursor had already passed it. A page is not a point-in-time snapshot; `next_page_token` is a position, not a snapshot handle. A page may contain fewer than `page_size` entries while `next_page_token` is still non-empty — per the proto, only an empty `next_page_token` means the result set is complete.

Authentication is required; the RPC is not filtered by caller identity, so callers should apply their own participation or tenancy checks before exposing results to end users. (This unfiltered property is also why the continuation token carries no signature -- see [Observation-surface authorization](deployment.md#observation-surface-authorization).)

> **Normative source.** This section follows the `macp-proto` contract in `proto/macp/v1/core.proto:411-426`, which defines `page_size`, `page_token`, and `next_page_token`. RFC-MACP-0006 §3.8 still describes `ListSessions` as an unpaginated listing: the RFC prose was never updated when the proto fields shipped. Where the two disagree, the proto is what this runtime implements. The RFC correction is tracked upstream as [multiagentcoordinationprotocol/multiagentcoordinationprotocol#76](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/issues/76), "RFC-MACP-0006 §3.8 and RFC-MACP-0001 §7 still describe ListSessions as unpaginated".

### WatchSessions

Server-streaming RPC for observing session lifecycle transitions across the runtime.

```protobuf
rpc WatchSessions(WatchSessionsRequest) returns (stream WatchSessionsResponse)
```

On connect, the runtime emits one `Created` event per session currently in the registry (initial sync), then streams live `SessionLifecycleEvent` entries as sessions are `Created`, `Resolved`, or `Expired`. Each event carries `event_type`, the current `SessionMetadata` snapshot, and `observed_at_unix_ms`. The underlying broadcast channel has a bounded capacity -- slow subscribers that fall behind will miss events, so consumers should reconcile with `ListSessions` on reconnect.

### CancelSession

Allows the session initiator to terminate a session. This is a core control-plane operation -- mode authorization does not apply.

```protobuf
rpc CancelSession(CancelSessionRequest) returns (CancelSessionResponse)
```

**Request fields**: `session_id` (string), `reason` (string, optional).

Only the session initiator can cancel. The runtime writes a `SessionCancelPayload` to the log with `cancelled_by` set to the authenticated sender. If the session is already terminal, the current state is returned without error.

### SuspendSession

Suspends an `OPEN` session (RFC-MACP-0001 §7.5). Like `CancelSession`, this is a core control-plane operation restricted to the session initiator or policy-delegated roles -- mode authorization does not apply.

```protobuf
rpc SuspendSession(SuspendSessionRequest) returns (SuspendSessionResponse)
```

**Request fields**: `session_id` (string), `reason` (string, optional).

While suspended, mode traffic into the session is rejected. Time spent suspended is banked against the session's `max_suspend_ms` bound (from `SessionStartPayload`, defaulting to the runtime cap when 0); exceeding it expires the session.

### ResumeSession

Resumes a `SUSPENDED` session back to `OPEN`, banking the suspended duration into the TTL deadline. Same authority model as `SuspendSession`.

```protobuf
rpc ResumeSession(ResumeSessionRequest) returns (ResumeSessionResponse)
```

**Request fields**: `session_id` (string), `reason` (string, optional).

## Discovery

### GetManifest

Returns the runtime's full capability manifest, including all supported modes (standards-track and extensions), content types, and identity information.

```protobuf
rpc GetManifest(GetManifestRequest) returns (GetManifestResponse)
```

### ListModes

Returns descriptors for standards-track modes only. Extension modes are excluded.

```protobuf
rpc ListModes(ListModesRequest) returns (ListModesResponse)
```

Each `ModeDescriptor` includes the mode identifier, version, title, description, determinism class, participant model, accepted message types, terminal message types, and schema URIs.

### ListRoots

Discovers available resource roots.

```protobuf
rpc ListRoots(ListRootsRequest) returns (ListRootsResponse)
```

Returns a list of `Root` entries, each with a `uri` and `name`.

## Extension Mode Lifecycle

### ListExtModes

Returns descriptors for extension modes, including both built-in extensions (like `ext.multi_round.v1`) and dynamically registered ones.

```protobuf
rpc ListExtModes(ListExtModesRequest) returns (ListExtModesResponse)
```

### RegisterExtMode

Dynamically registers a new extension mode. The mode identifier must not be empty, must not already exist, and must not use the reserved `macp.mode.*` namespace. Requires `can_manage_mode_registry` on the auth identity.

```protobuf
rpc RegisterExtMode(RegisterExtModeRequest) returns (RegisterExtModeResponse)
```

### UnregisterExtMode

Removes a dynamically registered extension mode. Built-in modes cannot be unregistered.

```protobuf
rpc UnregisterExtMode(UnregisterExtModeRequest) returns (UnregisterExtModeResponse)
```

### PromoteMode

Promotes an extension mode to standards-track status, optionally assigning a new identifier.

```protobuf
rpc PromoteMode(PromoteModeRequest) returns (PromoteModeResponse)
```

## Governance Policy

### RegisterPolicy

Registers a governance policy definition. The runtime validates the rules against the target mode's schema and enforces conditional constraints (for example, `weighted` algorithm requires a non-empty `weights` map). The built-in `policy.default` cannot be overwritten.

```protobuf
rpc RegisterPolicy(RegisterPolicyRequest) returns (RegisterPolicyResponse)
```

See the [Policy page](policy.md) for JSON rule examples and validation details.

### UnregisterPolicy, GetPolicy, ListPolicies

Standard CRUD operations for the policy registry. `UnregisterPolicy` cannot remove `policy.default`. `ListPolicies` accepts an optional mode filter.

```protobuf
rpc UnregisterPolicy(UnregisterPolicyRequest) returns (UnregisterPolicyResponse)
rpc GetPolicy(GetPolicyRequest) returns (GetPolicyResponse)
rpc ListPolicies(ListPoliciesRequest) returns (ListPoliciesResponse)
```

## Streaming Watches

### WatchModeRegistry

Server-streaming RPC that sends a notification on connection and then fires whenever the mode registry changes (register, unregister, or promote).

```protobuf
rpc WatchModeRegistry(WatchModeRegistryRequest) returns (stream WatchModeRegistryResponse)
```

### WatchRoots

Server-streaming RPC for root change notifications.

```protobuf
rpc WatchRoots(WatchRootsRequest) returns (stream WatchRootsResponse)
```

### WatchSignals

Server-streaming RPC that delivers ambient signal broadcasts. Signals have empty `session_id` and empty `mode`, carry a `SignalPayload` with `signal_type`, `data`, optional `confidence`, and optional `correlation_session_id`. Signals never enter session history.

```protobuf
rpc WatchSignals(WatchSignalsRequest) returns (stream WatchSignalsResponse)
```

### WatchPolicies

Server-streaming RPC that fires when policies are registered or unregistered.

```protobuf
rpc WatchPolicies(WatchPoliciesRequest) returns (stream WatchPoliciesResponse)
```

## Authentication

The runtime applies a resolver chain in this order:

1. **JWT bearer** (when `MACP_AUTH_ISSUER` is set): `Authorization: Bearer <jwt>`. The JWT's `sub` claim becomes the sender; `macp_scopes` carries capability flags (`allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, `is_observer`).
2. **Static bearer** (when `MACP_AUTH_TOKENS_*` is set): `Authorization: Bearer <token>` or `x-macp-token: <token>` header. The opaque token is mapped to an `AuthIdentity` via the configured token file.
3. **Dev-mode fallback** (when neither JWT nor static bearer is configured): any `Authorization: Bearer <value>` header authenticates the caller as sender `<value>` with all capabilities. Intended only for local development.
4. **Reject**: Returns `UNAUTHENTICATED`.

See the [Getting Started guide](getting-started.md) for token configuration examples.

## Resource limits

Five bounds on request size, request frequency, and response size:

| Variable | Meaning | Default |
|---|---|---|
| `MACP_MAX_PAYLOAD_BYTES` | max envelope payload size, in bytes | `1048576` |
| `MACP_SESSION_START_LIMIT_PER_MINUTE` | per-sender session start limit | `60` |
| `MACP_MESSAGE_LIMIT_PER_MINUTE` | per-sender message limit | `600` |
| `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` | `ListSessions` page size used when the request sends `page_size = 0` | `100` |
| `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` | hard cap a requested `ListSessions` `page_size` is clamped to | `1000` |

`MACP_MAX_PAYLOAD_BYTES` bounds the envelope *payload*; the gRPC request ceiling is `MACP_MAX_PAYLOAD_BYTES` plus a fixed 64 KiB envelope-overhead allowance (~1.06 MiB at the default), which is what `max_decoding_message_size` is set to.

The same five variables appear in [`README.md`](../README.md) and [`docs/deployment.md`](deployment.md).

### Rate limits

`MACP_SESSION_START_LIMIT_PER_MINUTE` and `MACP_MESSAGE_LIMIT_PER_MINUTE` are per-sender sliding-window limits on session creation and message throughput. When either is exceeded, the runtime returns `RATE_LIMITED`.

### Page caps

`MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` and `MACP_LIST_SESSIONS_MAX_PAGE_SIZE` bound the size of a single [`ListSessions`](#listsessions) response. They are **not** rate limits: exceeding the cap is not an error and never returns `RATE_LIMITED` -- an over-large `page_size` is silently clamped, and the caller continues the traversal with `next_page_token`. A caller may page as fast as the message rate limit allows.
