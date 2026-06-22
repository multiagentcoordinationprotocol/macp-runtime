//! `macp-storage` — the MACP persistence layer.
//!
//! Holds the append-only accepted-history log ([`log_store`]), the in-memory
//! session registry with its on-disk mirror ([`registry`]), and the pluggable
//! [`storage::StorageBackend`] trait with its file, memory, rocksdb, and redis
//! implementations. The heavy native backends (`rocksdb`, `redis`) are gated
//! behind the `rocksdb-backend` / `redis-backend` features so the kernel and
//! library consumers never compile them unless asked.

pub mod log_store;
pub mod registry;
pub mod storage;
