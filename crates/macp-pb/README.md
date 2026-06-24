# macp-pb

Generated [Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io) (MACP)
protobuf **message** types, built with `prost`. Transport-free: this crate
contains no tonic/gRPC service stubs, so it can be used to construct and parse
MACP envelopes and payloads without pulling in a transport.

The `macp.v1` gRPC **service** stubs are generated separately in the
[`macp-runtime`](https://crates.io/crates/macp-runtime) crate, which references
these message types via `.extern_path` so they are defined exactly once.

This crate is part of the [`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace and sits at the base of its one-way dependency graph.

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
