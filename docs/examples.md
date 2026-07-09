# Examples

The runtime ships with example clients in `src/bin` that demonstrate each coordination mode. This page walks through what each example does and highlights the runtime behavior you should pay attention to.

For protocol-level example transcripts, see the [protocol examples documentation](https://www.multiagentcoordinationprotocol.io/docs/examples).

All examples target `macp-runtime v0.5.0` and use the development security shortcut:

```bash
export MACP_ALLOW_INSECURE=1
cargo run
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
- For observers and late joiners, send a passive-subscribe frame (`subscribe_session_id` + `after_sequence`) as the first frame -- the runtime replays accepted history starting at `after_sequence` and then delivers live envelopes on the same stream. Use `after_sequence = 0` to replay from session start.
- Mixed-session streams (envelopes targeting different sessions) are rejected.
- A single frame must not carry both `envelope` and `subscribe_session_id` -- the stream terminates with `InvalidArgument`.
- Application errors are delivered inline without closing the stream.

## Extension Mode Lifecycle

The runtime supports dynamic extension management through four RPCs:

1. **`ListExtModes`** discovers available extensions (including `ext.multi_round.v1`).
2. **`RegisterExtMode`** registers a new extension with a mode descriptor. The runtime creates a passthrough handler for it.
3. **`UnregisterExtMode`** removes a dynamic extension (built-in modes are protected).
4. **`PromoteMode`** promotes an extension to standards-track, optionally renaming it.

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
