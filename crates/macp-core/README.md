# macp-core

The transport-free vocabulary of the [Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io)
(MACP) reference runtime: error types, the session model and its strict
`SessionStart` validation, decision-domain types, policy value types,
`CommitmentRules`, and the `PolicyEvaluator` trait.

`macp-core` depends only on `macp-pb` and serialization crates — no tonic,
tokio, or storage. Library consumers can build on the MACP coordination
vocabulary and drive the modes in
[`macp-modes`](https://crates.io/crates/macp-modes) with their own
`PolicyEvaluator` implementation, without taking on transport, persistence, or
auth.

This crate is part of the [`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace.

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
