# Runtime Architecture

The MACP Runtime is a coordination kernel written in Rust. It receives agent messages over gRPC, validates and orders them, dispatches to mode-specific logic, and persists an append-only history that can be replayed to reconstruct any session.

This page describes the runtime's internal design: its layer structure, how requests flow through the system, how concurrency is managed, and how persistence works. For the protocol-level architecture -- the two-plane model, session lifecycle, and determinism guarantees -- see the [protocol architecture documentation](https://www.multiagentcoordinationprotocol.io/docs/architecture).

## Layers

The runtime is organized into seven layers, each with a clear responsibility boundary:

```
Agents (external)
  |
  v  gRPC (tonic)
+-----------------------------------------------------------+
|  Transport Layer (src/server.rs)                          |
|    24 RPC handlers, envelope validation, sender            |
|    derivation, rate limiting                               |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Auth Layer (macp-auth)                                   |
|    Resolver chain: JWT bearer → static bearer → dev-mode   |
|    fallback; capability flags (allowed_modes,              |
|    max_open_sessions, is_observer, …)                      |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Coordination Kernel (src/runtime.rs)                     |
|    Session lifecycle, deduplication, TTL enforcement,      |
|    mode dispatch, signal broadcast, session lifecycle      |
|    observability (WatchSessions)                           |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Mode Layer (macp-modes)                                  |
|    Decision, Proposal, Task, Handoff, Quorum,             |
|    Multi-Round, Passthrough; governance via an injected    |
|    macp_core::PolicyEvaluator                              |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Policy Layer (macp-policy)                               |
|    Policy registry, DefaultPolicyEvaluator, per-mode       |
|    commitment evaluators, rule validation                  |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Storage Layer (macp-storage)                             |
|    File, RocksDB, Redis, and in-memory backends,          |
|    append-only logs, checkpointing, compaction             |
+-----------------------------------------------------------+
  |
  v
+-----------------------------------------------------------+
|  Replay (src/replay.rs)                                   |
|    Session rebuild from log entries, checkpoint fast path  |
+-----------------------------------------------------------+
```

> Vocabulary types (`error`, `session`, decision domain types, policy value
> types, the `PolicyEvaluator` trait) and the generated protobuf message types
> live in the transport-free `macp-core` and `macp-pb` crates, which the layers
> above build on. See `CLAUDE.md` → "Workspace crates" for the full crate map.

The **transport layer** terminates gRPC connections, delegates authentication to the auth layer, validates envelope structure, overrides the sender field with the authenticated identity, and enforces per-sender rate limits. It never touches session state directly.

The **auth layer** implements a pluggable resolver chain. Each resolver inspects the request metadata and returns either a verified identity, a pass (the credential isn't ours, try the next resolver), or a hard reject (the credential is ours but invalid). The built-in resolvers are `jwt_bearer` (validates signature, issuer, audience, and expiration via a JWKS) and `static_bearer` (looks up opaque tokens in a preloaded map). When neither is configured, the layer falls back to dev-mode auth where any bearer value becomes the sender -- strictly for local development. Every resolved identity carries capability flags (`allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, `is_observer`) that later authorization checks consult.

The **coordination kernel** is the heart of the runtime. It manages session creation, deduplication, lazy TTL expiration, and dispatches messages to the appropriate mode handler. It also broadcasts signals on the ambient plane and publishes accepted envelopes to streaming subscribers.

The **mode layer** implements each coordination mode as a pure function over session state. Mode handlers receive an immutable session reference and an envelope, and return a declarative response telling the kernel what to do -- persist state, resolve the session, or both. Modes never mutate state directly.

The **policy layer** evaluates governance rules at commitment time. Each mode has a dedicated evaluator that checks whether accumulated session history satisfies the policy's constraints.

The **storage layer** provides a pluggable persistence backend behind a common trait. All backends support the same operations: creating session storage, appending log entries, loading logs for replay, and managing checkpoints.

The **replay engine** reconstructs sessions from their append-only logs on startup. It can optionally use checkpoints as a fast path, only replaying entries written after the last checkpoint.

## Request flow: Send RPC

When a client calls `Send` with an envelope, the request passes through the full pipeline:

```
Client --Send(Envelope)--> server.rs::send()
  |
  +-- authenticate_metadata()       -> AuthIdentity
  +-- validate_envelope_shape()     -> check version, non-empty fields, size
  +-- apply_authenticated_sender()  -> override envelope.sender
  +-- authorize_mode()              -> check allowed_modes whitelist
  +-- enforce_rate_limit()          -> sliding window check
  |
  +-- runtime.process(envelope)
  |     |
  |     +-- [SessionStart]
  |     |     validate session ID format
  |     |     look up mode in registry
  |     |     parse and validate payload (participants, versions, TTL)
  |     |     resolve policy version
  |     |     call mode.on_session_start()
  |     |     persist log entry               <-- COMMIT POINT
  |     |     insert session into registry
  |     |     publish to stream subscribers
  |     |
  |     +-- [Signal]
  |     |     validate SignalPayload
  |     |     broadcast on signal bus (no state mutation)
  |     |
  |     +-- [Session message]
  |           dedup check (seen_message_ids)
  |           verify mode matches session
  |           lazy TTL expiration check
  |           verify session is OPEN
  |           call mode.authorize_sender()
  |           call mode.on_message()
  |           persist log entry               <-- COMMIT POINT
  |           apply mode response to session
  |           if resolved: compact or checkpoint
  |           publish to stream subscribers
  |
  +-- build Ack and return SendResponse
```

The critical property is the **commit point**: the log entry is persisted to durable storage before any in-memory state is updated. If the server crashes after the commit point, replay will reconstruct the session correctly on restart. If it crashes before, the message was never acknowledged and the client can safely retry.

## Request flow: StreamSession

Bidirectional streaming works similarly but binds the stream to a single session:

The first frame on the stream establishes the session binding. It can be either (a) an envelope -- the stream then behaves as an active participant, or (b) a passive-subscribe frame (`subscribe_session_id` + `after_sequence`) -- the runtime replays accepted envelopes from log index `after_sequence` and then delivers live envelopes on the same stream. All subsequent envelope frames must target the same session. Passive subscribe is authorized for session initiators, declared participants, and identities carrying the `is_observer` capability; non-participants receive an inline `FORBIDDEN` error and the stream stays open. A frame that sets both `envelope` and `subscribe_session_id` is rejected with `InvalidArgument`.

Application-level errors like validation failures or authorization denials are sent as inline error messages on the stream without closing it. Transport-level errors like authentication failure terminate the stream. If the client falls behind the broadcast buffer (capacity: 256 envelopes), the stream is terminated with `ResourceExhausted` and the client must reconnect -- optionally resuming via passive subscribe with an `after_sequence` derived from the last envelope it saw.

## Key types

The runtime's core abstractions are expressed as a small set of Rust types:

```rust
// Session state machine -- monotonic transitions only
enum SessionState { Open, Resolved, Expired }

// Mode contract -- modes are pure functions over session state
trait Mode: Send + Sync {
    fn on_session_start(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError>;
    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError>;
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError>;
}

// Declarative responses -- modes never mutate state directly
enum ModeResponse {
    NoOp,                                          // no state change
    PersistState(Vec<u8>),                         // update mode state
    Resolve(Vec<u8>),                              // terminate session
    PersistAndResolve { state: Vec<u8>, resolution: Vec<u8> },
}

// Storage contract -- all backends implement this trait
trait StorageBackend: Send + Sync {
    async fn append_log_entry(&self, session_id: &str, entry: &LogEntry) -> io::Result<()>;
    async fn load_log(&self, session_id: &str) -> io::Result<Vec<LogEntry>>;
    async fn save_session(&self, session: &Session) -> io::Result<()>;
    async fn load_session(&self, session_id: &str) -> io::Result<Option<Session>>;
    // ... additional methods for lifecycle management
}

// Security identity -- derived from auth tokens, never from envelope
struct AuthIdentity {
    sender: String,
    allowed_modes: Option<HashSet<String>>,
    can_start_sessions: bool,
    max_open_sessions: Option<usize>,
    can_manage_mode_registry: bool,
}
```

The `Mode` trait is the central abstraction for extensibility. Each mode receives an immutable reference to the session and returns a `ModeResponse` that the kernel applies. This design ensures modes cannot introduce inconsistencies by directly mutating shared state.

## Durability model

The runtime uses a two-phase write strategy:

1. **Log entry persisted first** -- This is the commit point. The append-only log survives server crashes, and the replay engine can reconstruct any session from it.

2. **In-memory state updated second** -- Session snapshots are a cache optimization. If a snapshot is missing or stale, replay rebuilds the correct state from the log.

Session snapshots are written on each state change as a best-effort optimization. For `SessionStart`, snapshot failure is treated as fatal (the initial snapshot must be durable). For subsequent messages, the log entry is the authoritative record and snapshot failure is non-fatal.

### Checkpointing

The runtime supports two forms of log compaction:

- **Interval-based checkpoints**: After a configurable number of log entries (`MACP_CHECKPOINT_INTERVAL`), the runtime writes a checkpoint that captures the full session state. Replay can start from the checkpoint instead of replaying the entire log.

- **Terminal compaction**: When a session reaches a terminal state (resolved, cancelled, or expired), the runtime compacts its log into a single checkpoint entry. If compaction fails, a forced checkpoint is written as a fallback.

## Concurrency model

The runtime processes different sessions in parallel but serializes access within each session:

- **Per-session serialization**: The session registry uses `RwLock<HashMap>`. Mutations to a session acquire a write lock, ensuring only one message is processed at a time for that session.

- **Cross-session parallelism**: Messages targeting different sessions are processed concurrently with no coordination between them.

- **Stream bus**: Each session has a `tokio::sync::broadcast` channel (capacity 256) for delivering accepted envelopes to `StreamSession` subscribers. A separate global broadcast channel handles ambient signals via `WatchSignals`, and a third broadcast channel (capacity 64) carries session lifecycle events (`Created`, `Resolved`, `Expired`) for `WatchSessions` subscribers.

- **Background tasks**: A periodic cleanup task runs every 60 seconds (configurable via `MACP_CLEANUP_INTERVAL_SECS`) to expire sessions that have exceeded their TTL and evict terminal sessions from memory after a retention period.

## Mode registry

The `ModeRegistry` is the single source of truth for which modes the runtime supports. It is built at startup with the five standards-track modes and the built-in `ext.multi_round.v1` extension.

At runtime, the registry supports dynamic extension management: `RegisterExtMode` adds new extensions backed by a generic passthrough handler, `UnregisterExtMode` removes them (built-in modes are protected), and `PromoteMode` elevates an extension to standards-track status. All changes are broadcast to `WatchModeRegistry` subscribers.

## Source layout

```
src/
  main.rs              -- Startup, TLS config, persistence wiring, background tasks
  server.rs            -- gRPC adapter (24 RPCs), envelope validation, streaming
  runtime.rs           -- Coordination kernel, session lifecycle, mode dispatch,
                          session lifecycle broadcast
  session.rs           -- Session model, SessionStart validation, ID rules
  security.rs          -- Auth config loader, sender derivation, rate limiting
  error.rs             -- Error types and RFC error code mapping
  registry.rs          -- In-memory session registry
  log_store.rs         -- In-memory log cache; passive-subscribe replay helpers
  stream_bus.rs        -- Per-session broadcast channels for StreamSession
  metrics.rs           -- Atomic per-mode counters
  mode_registry.rs     -- Mode lookup, extension lifecycle
  replay.rs            -- Session rebuild from logs, checkpoint fast path
  auth/
    mod.rs             -- Public exports for chain, resolver, resolvers
    chain.rs           -- AuthResolverChain: walks resolvers in order
    resolver.rs        -- AuthResolver trait, ResolvedIdentity, AuthError
    resolvers/
      mod.rs           -- Built-in resolver exports
      jwt_bearer.rs    -- JWT bearer resolver (inline JWKS or URL, with cache)
      static_bearer.rs -- Opaque bearer token → identity map resolver
  extensions/
    mod.rs             -- Public exports for extension plumbing
    provider.rs        -- SessionExtensionProvider trait, SessionOutcome
    registry.rs        -- ExtensionProviderRegistry lifecycle fan-out
  mode/
    mod.rs             -- Mode trait and standard descriptors
    decision.rs        -- Decision mode state machine
    proposal.rs        -- Proposal mode (negotiation, convergence)
    task.rs            -- Task mode (assignment, progress, completion)
    handoff.rs         -- Handoff mode (serial offers, context, acceptance)
    quorum.rs          -- Quorum mode (threshold approval, ballots)
    multi_round.rs     -- Built-in extension: iterative convergence
    passthrough.rs     -- Generic handler for dynamic extensions
    util.rs            -- Shared commitment validation and authority checks
  policy/
    mod.rs             -- Policy types and decision structures
    registry.rs        -- Policy CRUD with broadcast notifications
    evaluator.rs       -- Per-mode commitment evaluation
    rules.rs           -- Serde structs for mode-specific rule schemas
    defaults.rs        -- Default policy (no constraints)
  storage/
    mod.rs             -- StorageBackend trait and backend selection
    file.rs            -- File backend: per-session dirs, atomic writes
    memory.rs          -- In-memory backend for testing
    rocksdb.rs         -- RocksDB backend (feature-gated)
    redis_backend.rs   -- Redis backend (feature-gated)
    compaction.rs      -- Log compaction for terminal sessions
    recovery.rs        -- Crash recovery (.tmp file cleanup)
    migration.rs       -- Storage format migration
```
