# macp-storage

The persistence layer of the [Multi-Agent Coordination Protocol](https://www.multiagentcoordinationprotocol.io)
(MACP) reference runtime: the append-only accepted-history log, the in-memory
session registry, and a pluggable storage-backend trait.

Backends:

- **file** (default) — per-session append-only log files and session snapshots,
  with atomic writes and crash recovery
- **memory** — in-process backend for tests
- **rocksdb** — enable the `rocksdb-backend` feature
- **redis** — enable the `redis-backend` feature

This crate is part of the [`macp-runtime`](https://github.com/multiagentcoordinationprotocol/runtime)
workspace and depends on [`macp-core`](https://crates.io/crates/macp-core).

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).
