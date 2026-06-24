# MACP Runtime Documentation

**Version**: v0.4.0 | **Protocol**: MACP 1.0 | **Language**: Rust

The MACP Runtime is the reference implementation of the [Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io). It is a coordination kernel written in Rust that enforces session boundaries, validates messages, manages append-only history, and serializes concurrent agent interactions over gRPC.

This documentation covers the **runtime implementation** -- how to build, configure, deploy, and integrate with it. For protocol-level concepts like sessions, modes, signals, determinism, and the two-plane architecture, see the [protocol documentation](https://www.multiagentcoordinationprotocol.io/docs).

## What the runtime provides

The runtime ships as a single binary that exposes 22 gRPC RPCs over TLS. It supports the five standards-track coordination modes (Decision, Proposal, Task, Handoff, Quorum) and one built-in extension mode for iterative convergence. A governance policy framework evaluates rules at commitment time, and pluggable storage backends (file, RocksDB, Redis, or in-memory) handle persistence with append-only logs and checkpoint-based replay.

Authentication is layered as a resolver chain: JWT bearer (when an issuer and JWKS are configured), then static bearer tokens, with a dev-mode fallback only when neither is set. Identities expose capability flags -- `allowed_modes`, `can_start_sessions`, `max_open_sessions`, `can_manage_mode_registry`, and `is_observer` -- that are enforced on every request. Per-sender sliding-window rate limits cover both session creation and message throughput. Session lifecycle transitions can be observed in real time through `ListSessions` and `WatchSessions`, and accepted envelope history can be replayed into a stream via passive subscribe.

Internally the runtime is a Cargo workspace with a one-way dependency graph: the coordination vocabulary (`macp-core`), generated messages (`macp-pb`), mode logic (`macp-modes`), policy (`macp-policy`), storage (`macp-storage`), and auth (`macp-auth`) are separate crates, and the root `macp-runtime` crate is the kernel + gRPC server + binary. `macp-core` and `macp-pb` are transport-free, so library consumers can build on the MACP vocabulary and mode logic — driving them with a custom `PolicyEvaluator` — without pulling in transport, storage, or auth. See [Architecture](architecture.md) and `CLAUDE.md` → "Workspace crates" for the crate map.

## Documentation

### Getting started
- [**Getting Started**](getting-started.md) -- Build the runtime, start a server, and run your first coordination session
- [**Examples**](examples.md) -- Runnable example clients for every mode, with troubleshooting tips

### Implementation reference
- [**Architecture**](architecture.md) -- Rust layer design, request processing flows, concurrency model, and source layout
- [**API Reference**](API.md) -- All 22 gRPC RPCs with request/response fields, authentication, and rate limiting
- [**Modes**](modes.md) -- Runtime implementation details for each mode's state machine
- [**Policy**](policy.md) -- Policy registration, JSON rule examples, evaluation internals, and error handling

### Operations
- [**Deployment**](deployment.md) -- Production configuration, storage backends, crash recovery, containers, and monitoring
- [**Testing**](testing.md) -- Three test tiers, conformance fixtures, and CI/CD integration

### For SDK authors
- [**SDK Developer Guide**](sdk-guide.md) -- Patterns for building client libraries: envelope construction, error handling, streaming, and retries

## Protocol documentation

The runtime implements the protocol as specified in the RFCs. For protocol-level topics, refer to the specification documentation:

| Topic | Link |
|-------|------|
| Architecture and two-plane model | [Protocol Architecture](https://www.multiagentcoordinationprotocol.io/docs/architecture) |
| Session lifecycle | [Protocol Lifecycle](https://www.multiagentcoordinationprotocol.io/docs/lifecycle) |
| Coordination modes | [Protocol Modes](https://www.multiagentcoordinationprotocol.io/docs/modes) |
| Governance policies | [Protocol Policy](https://www.multiagentcoordinationprotocol.io/docs/policy) |
| Determinism and replay | [Protocol Determinism](https://www.multiagentcoordinationprotocol.io/docs/determinism) |
| Security model | [Protocol Security](https://www.multiagentcoordinationprotocol.io/docs/security) |
| Transport bindings | [Protocol Transports](https://www.multiagentcoordinationprotocol.io/docs/transports) |
| Agent discovery | [Protocol Discovery](https://www.multiagentcoordinationprotocol.io/docs/discovery) |
