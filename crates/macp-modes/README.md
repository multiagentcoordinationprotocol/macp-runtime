# macp-modes

Coordination **mode** implementations for the
[Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io)
(MACP) reference runtime, plus the mode registry.

Standards-track modes: `decision`, `proposal`, `task`, `handoff`, `quorum`.
Built-in extension: `multi_round`. Dynamically registered extensions are backed
by a generic passthrough handler.

Modes evaluate governance through the `macp_core::PolicyEvaluator` trait — an
evaluator is injected at construction — so this crate has no dependency on any
concrete policy engine. Pair it with
[`macp-policy`](https://crates.io/crates/macp-policy) for the default behavior,
or supply your own evaluator.

This crate is part of the [`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace and depends on [`macp-core`](https://crates.io/crates/macp-core).

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
