mod file;
mod memory;
mod migration;
mod recovery;

#[cfg(feature = "rocksdb-backend")]
pub mod rocksdb;
#[cfg(feature = "rocksdb-backend")]
pub use self::rocksdb::RocksDbBackend;

#[cfg(feature = "redis-backend")]
pub mod redis_backend;
#[cfg(feature = "redis-backend")]
pub use redis_backend::RedisBackend;

pub mod compaction;

pub use file::FileBackend;

pub use memory::MemoryBackend;
pub use migration::migrate_if_needed;
pub use recovery::{cleanup_temp_files, recover_session};

/// Whether `session_id` names exactly one plain path component, so a backend
/// may key files or directories by it: non-empty, at most 255 bytes, no `/`,
/// `\` or NUL, and not `.` or `..`. Every id `SessionStart` accepts passes;
/// a read keyed by an id from the wire must check it before touching storage.
pub fn is_storage_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 255
        && session_id != "."
        && session_id != ".."
        && !session_id.contains(['/', '\\', '\0'])
}

/// Refuse a trace-anchor key that [`is_storage_safe_session_id`] refuses. A
/// NUL-bearing id could otherwise collide with another session's composite
/// key in a backend that joins ids with `\0`.
#[cfg(any(feature = "rocksdb-backend", feature = "redis-backend"))]
pub(crate) fn check_anchor_session_id(session_id: &str) -> io::Result<()> {
    if is_storage_safe_session_id(session_id) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session id is not storage-safe",
        ))
    }
}

use crate::log_store::LogEntry;
use macp_core::session::Session;
use std::io;

// ---------------------------------------------------------------------------
// StorageBackend trait
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    async fn save_session(&self, session: &Session) -> io::Result<()>;
    async fn load_session(&self, session_id: &str) -> io::Result<Option<Session>>;
    async fn load_all_sessions(&self) -> io::Result<Vec<Session>>;
    async fn delete_session(&self, session_id: &str) -> io::Result<()>;
    async fn list_session_ids(&self) -> io::Result<Vec<String>>;
    async fn append_log_entry(&self, session_id: &str, entry: &LogEntry) -> io::Result<()>;
    async fn load_log(&self, session_id: &str) -> io::Result<Vec<LogEntry>>;
    async fn create_session_storage(&self, session_id: &str) -> io::Result<()>;

    async fn replace_log(&self, _session_id: &str, _entries: &[LogEntry]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "compaction not supported by this backend",
        ))
    }

    /// The decision receipt stored for `session_id` (RFC-MACP-0013 §13), if
    /// any. Receipts are opaque bytes to storage.
    async fn load_receipt(&self, _session_id: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Store `receipt` for `session_id` unless one is already stored:
    /// first writer wins, and a later call gets the stored bytes back as
    /// [`ReceiptWrite::Existing`]. A stored receipt is never replaced. The
    /// default is `Unsupported`: the backend cannot keep receipts.
    async fn store_receipt_if_absent(
        &self,
        _session_id: &str,
        _receipt: &[u8],
    ) -> io::Result<ReceiptWrite> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "receipts not supported by this backend",
        ))
    }

    /// Whether [`Self::store_receipt_if_absent`] persists receipts across a
    /// restart. A runtime that signs receipts on a backend that does not may
    /// serve a different attested receipt after restarting.
    fn receipts_durable(&self) -> bool {
        false
    }

    /// Store `session_id`'s trace anchor, replacing any stored one. The
    /// anchor is an opaque, best-effort side record kept **outside** session
    /// state: never in the session snapshot, the log, a checkpoint or
    /// anything replay reads. The caller writes it at most once per session
    /// and never fails a message on an error here. The default keeps
    /// nothing (`Ok`): the anchor then lives in the caller's memory only.
    async fn store_trace_anchor(&self, _session_id: &str, _anchor: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// The trace anchor stored for `session_id`, if any (opaque bytes).
    /// `delete_session` removes it with the session.
    async fn load_trace_anchor(&self, _session_id: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Delete stored trace anchors whose session no longer exists, returning
    /// how many were deleted. Only a backend whose `delete_session` could
    /// once leave an anchor behind (an older binary that did not know about
    /// anchors) needs to override it; the default deletes nothing.
    async fn sweep_orphan_trace_anchors(&self) -> io::Result<usize> {
        Ok(0)
    }
}

/// The outcome of [`StorageBackend::store_receipt_if_absent`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptWrite {
    /// These bytes are now the session's receipt.
    Stored,
    /// A receipt was already stored; these are its bytes.
    Existing(Vec<u8>),
}

/// The trace-anchor contract, run by each backend's own tests against a
/// fresh instance. `durable` says whether the backend keeps anchors (the
/// memory backend and the trait default do not).
#[cfg(test)]
pub(crate) mod anchor_contract {
    use super::StorageBackend;
    use macp_core::session::Session;

    async fn seed(backend: &dyn StorageBackend, id: &str) {
        backend.create_session_storage(id).await.unwrap();
        let session = Session::builder(id, "macp.mode.decision.v1", "alice")
            .ttl_ms(60_000)
            .build();
        backend.save_session(&session).await.unwrap();
    }

    /// Store, load, replace, delete-with-the-session, path-like ids refused,
    /// and the anchor kept apart from the session, its log and its receipt.
    pub(crate) async fn store_load_delete(backend: &dyn StorageBackend, id: &str, durable: bool) {
        seed(backend, id).await;
        let anchor =
            br#"{"v":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#;
        assert_eq!(backend.load_trace_anchor(id).await.unwrap(), None);
        backend.store_trace_anchor(id, anchor).await.unwrap();
        if !durable {
            assert_eq!(backend.load_trace_anchor(id).await.unwrap(), None);
            backend.delete_session(id).await.unwrap();
            assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 0);
            return;
        }
        assert_eq!(
            backend.load_trace_anchor(id).await.unwrap().as_deref(),
            Some(&anchor[..])
        );
        // Opaque bytes, replaced by a later write.
        backend
            .store_trace_anchor(id, b"\x00\xffopaque")
            .await
            .unwrap();
        assert_eq!(
            backend.load_trace_anchor(id).await.unwrap().as_deref(),
            Some(&b"\x00\xffopaque"[..])
        );
        // Not session state: the session, its log and its receipt slot are
        // exactly what they were.
        let session = backend.load_session(id).await.unwrap().unwrap();
        assert_eq!(session.session_id, id);
        assert!(backend.load_log(id).await.unwrap().is_empty());
        assert_eq!(backend.load_receipt(id).await.unwrap(), None);
        let ids = backend.list_session_ids().await.unwrap();
        assert_eq!(ids, vec![id.to_string()]);
        // A live session's anchor is not an orphan.
        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 0);
        assert!(backend.load_trace_anchor(id).await.unwrap().is_some());
        // Deleted with its session.
        backend.delete_session(id).await.unwrap();
        assert_eq!(backend.load_trace_anchor(id).await.unwrap(), None);
        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 0);
        // Path-like (or NUL-bearing) ids are refused, never written.
        for bad in ["", ".", "..", "a/b", "../x", "a\0anchor"] {
            assert!(
                backend.store_trace_anchor(bad, anchor).await.is_err(),
                "store {bad:?}"
            );
            assert!(
                backend.load_trace_anchor(bad).await.is_err(),
                "load {bad:?}"
            );
        }
    }
}

/// The receipt-store contract every durable backend must meet, run by each
/// backend's own tests against a fresh instance.
#[cfg(test)]
pub(crate) mod receipt_contract {
    use super::{LogEntry, ReceiptWrite, StorageBackend};
    use macp_core::session::Session;
    use std::sync::Arc;

    async fn seed(backend: &dyn StorageBackend, id: &str) {
        backend.create_session_storage(id).await.unwrap();
        let session = Session::builder(id, "macp.mode.decision.v1", "alice")
            .ttl_ms(60_000)
            .build();
        backend.save_session(&session).await.unwrap();
    }

    /// First writer wins; a stored receipt is never replaced; `delete_session`
    /// removes it; `replace_log` leaves it alone.
    pub(crate) async fn first_writer_wins(backend: &dyn StorageBackend, id: &str) {
        seed(backend, id).await;
        assert_eq!(backend.load_receipt(id).await.unwrap(), None);
        assert_eq!(
            backend.store_receipt_if_absent(id, b"first").await.unwrap(),
            ReceiptWrite::Stored
        );
        assert_eq!(
            backend
                .store_receipt_if_absent(id, b"second")
                .await
                .unwrap(),
            ReceiptWrite::Existing(b"first".to_vec())
        );
        assert_eq!(backend.load_receipt(id).await.unwrap().unwrap(), b"first");
        let checkpoint: LogEntry = serde_json::from_value(serde_json::json!({
            "message_id": "cp", "received_at_ms": 1, "sender": "",
            "message_type": "Checkpoint", "raw_payload": [], "entry_kind": "Checkpoint",
        }))
        .unwrap();
        if backend.replace_log(id, &[checkpoint]).await.is_ok() {
            assert_eq!(backend.load_receipt(id).await.unwrap().unwrap(), b"first");
        }
        backend.delete_session(id).await.unwrap();
        assert_eq!(backend.load_receipt(id).await.unwrap(), None);
        assert!(backend.receipts_durable());
    }

    /// N concurrent writers of distinct bytes: exactly one `Stored`, and
    /// every other writer is handed the winner's bytes.
    pub(crate) async fn one_winner_under_concurrency(backend: Arc<dyn StorageBackend>, id: &str) {
        seed(backend.as_ref(), id).await;
        let tasks: Vec<_> = (0..16)
            .map(|i| {
                let backend = backend.clone();
                let id = id.to_string();
                tokio::spawn(async move {
                    let bytes = format!("receipt-{i}").into_bytes();
                    (
                        bytes.clone(),
                        backend.store_receipt_if_absent(&id, &bytes).await.unwrap(),
                    )
                })
            })
            .collect();
        let mut winners = Vec::new();
        let mut handed = Vec::new();
        for t in tasks {
            match t.await.unwrap() {
                (mine, ReceiptWrite::Stored) => winners.push(mine),
                (_, ReceiptWrite::Existing(theirs)) => handed.push(theirs),
            }
        }
        assert_eq!(winners.len(), 1, "exactly one writer stores");
        assert!(handed.iter().all(|h| *h == winners[0]));
        assert_eq!(backend.load_receipt(id).await.unwrap().unwrap(), winners[0]);
    }
}
