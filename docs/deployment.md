# Deployment Guide

This guide covers everything you need to run the MACP Runtime in production: configuration, storage backends, crash recovery, monitoring, and container deployment. For protocol-level deployment topologies and security requirements, see the [protocol deployment](https://www.multiagentcoordinationprotocol.io/docs/deployment) and [protocol security](https://www.multiagentcoordinationprotocol.io/docs/security) documentation.

## Production checklist

Before exposing the runtime to production traffic, ensure these four items are configured:

1. **TLS certificates** -- Set `MACP_TLS_CERT_PATH` and `MACP_TLS_KEY_PATH` to valid PEM files. The runtime refuses to start without TLS unless `MACP_ALLOW_INSECURE=1` is set.

2. **Authentication** -- Configure at least one of the resolvers. For opaque bearer tokens, create a `tokens.json` mapping tokens to agent identities and set `MACP_AUTH_TOKENS_FILE`. For JWT bearer tokens, set `MACP_AUTH_ISSUER` together with a JWKS source (`MACP_AUTH_JWKS_JSON` inline or `MACP_AUTH_JWKS_URL` fetched + cached). Both can be configured at once -- JWT-shaped tokens are routed to the JWT resolver and opaque tokens to the static resolver. See the [Getting Started guide](getting-started.md) for the token format and JWT claim layout.

3. **Data directory** -- Ensure `MACP_DATA_DIR` points to a directory with write permissions. This is where session logs and snapshots are stored.

4. **Bind address** -- Set `MACP_BIND_ADDR` to the desired listen address. The default `127.0.0.1:50051` only accepts local connections.

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
| `MACP_CHECKPOINT_INTERVAL` | `0` (disabled) | Log entries between checkpoints |
| `MACP_CLEANUP_INTERVAL_SECS` | `60` | Background TTL cleanup interval in seconds |
| `MACP_SESSION_RETENTION_SECS` | `3600` | How long terminal sessions stay in memory |
| `MACP_STRICT_RECOVERY` | off | Set to `1` to fail on any recovery error |
| `RUST_LOG` | `info` | Log level filter |

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
are open discovery surfaces by design.

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

**Session eviction** -- Terminal sessions (resolved or expired) are evicted from memory after `MACP_SESSION_RETENTION_SECS` to bound memory usage. Their data remains on disk and can be replayed if needed.

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
