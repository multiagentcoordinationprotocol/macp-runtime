# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately via GitHub Security
Advisories ("Report a vulnerability" on the repository's Security tab).
Do not open public issues for security reports. You should receive an
acknowledgement within 72 hours.

## Supported versions

Only the latest released minor version receives security fixes.

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

See `docs/deployment.md` for hardening guidance (including backend
durability characteristics) and `plans/IMPROVEMENT_PLAN.md` for the audited
security backlog.
