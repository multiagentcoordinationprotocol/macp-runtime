# Security Policy

## Reporting a vulnerability

Please report security problems privately through GitHub: open the
repository's **Security** tab and choose **Report a vulnerability**. Don't open
a public issue for them. You should receive an acknowledgement within 72 hours.

Relevant reports include:

- a way to inject into, read, or change a session you are not authorized for;
- a way to make an accepted history, or a replay of it, differ from what the
  runtime acknowledged;
- an authentication or authorization bypass, including a forged
  `Envelope.sender`;
- a denial of service that gets past the configured rate, size, or
  open-session limits;
- the same classes of problem in the embeddable crates (`macp-core`,
  `macp-modes`, `macp-policy`, `macp-pb`) when they are used as libraries
  ([docs/embedding.md](docs/embedding.md)).

A flaw in the protocol itself, one that a conformant implementation would
share, belongs to the
[specification repository](https://github.com/multiagentcoordinationprotocol/multiagentcoordinationprotocol/security).
If you are unsure which applies, report it here.

## Supported versions

Only the latest released minor version receives security fixes. Fixes ship as
a patch release of the crates and a new `ghcr.io/multiagentcoordinationprotocol/macp-runtime`
image tag.

## Security model (summary)

- **Transport**: TLS is required; plaintext needs an explicit
  `MACP_ALLOW_INSECURE=1` opt-in (local development only).
- **Authentication**: static bearer tokens and/or JWT (RS256/ES256 by
  default; HS256 only via explicit `MACP_AUTH_JWT_ALGS` opt-in). With no
  auth configured the runtime refuses to start unless dev mode is explicitly
  enabled.
- **Identity**: `Envelope.sender` is always derived from the authenticated
  identity, never trusted from the payload (RFC-MACP-0004).
- **Authorization**: mode-level authority checks on every session-scoped
  message; commitment authority additionally governed by bound policy.
- **Isolation**: rejected messages never mutate accepted history or dedup
  state; signals never mutate session state.
- **Resource protection**: per-sender rate limits, payload size limits,
  max-open-session limits.

See [docs/deployment.md](docs/deployment.md) for hardening guidance, including
backend durability characteristics.
