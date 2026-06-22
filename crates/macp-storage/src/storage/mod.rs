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
}
