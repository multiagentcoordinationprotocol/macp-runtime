# macp-policy

The default governance-policy implementation for the
[Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io)
(MACP) reference runtime: per-mode rule schemas (aligned to RFC-MACP-0012), the
policy registry, and `DefaultPolicyEvaluator` — the built-in implementation of
the `macp_core::PolicyEvaluator` trait.

Modes never depend on this crate directly; they evaluate governance through the
`PolicyEvaluator` trait in [`macp-core`](https://crates.io/crates/macp-core).
The runtime injects `DefaultPolicyEvaluator` at mode construction, and a
consumer may substitute their own evaluator instead.

This crate is part of the [`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace.

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
