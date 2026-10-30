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
