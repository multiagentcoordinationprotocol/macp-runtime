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

Enumerates metadata for every session currently held in the registry (including terminal sessions still within the retention window).

```protobuf
rpc ListSessions(ListSessionsRequest) returns (ListSessionsResponse)
```

Returns a `sessions` array of `SessionMetadata` entries. Authentication is required; the RPC is not filtered by caller identity, so callers should apply their own participation or tenancy checks before exposing results to end users.

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

## Rate Limiting

The runtime enforces per-sender sliding-window rate limits:

| Limit | Default | Environment variable |
|-------|---------|---------------------|
| Session starts per minute | 60 | `MACP_SESSION_START_LIMIT_PER_MINUTE` |
| Messages per minute | 600 | `MACP_MESSAGE_LIMIT_PER_MINUTE` |

When a limit is exceeded, the runtime returns `RATE_LIMITED`.
