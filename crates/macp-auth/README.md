# macp-auth

The authentication and security layer of the
[Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io)
(MACP) reference runtime.

Provides:

- request identity derivation (the authenticated identity becomes the
  `Envelope.sender`; self-asserted senders are never trusted)
- the security layer: per-sender rate limits and payload-size enforcement
- a pluggable bearer-token resolver chain — JWT bearer (signature, issuer,
  audience, and expiration validated against a JWKS) and static bearer tokens,
  with a dev-mode fallback for local development

This crate confines `jsonwebtoken` and `reqwest` so the rest of the workspace
stays free of them. It is part of the
[`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace and depends on [`macp-core`](https://crates.io/crates/macp-core).

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
