use crate::log_store::LogEntry;
use macp_core::session::Session;
use std::io;

use super::StorageBackend;

pub struct MemoryBackend;

#[async_trait::async_trait]
impl StorageBackend for MemoryBackend {
    async fn save_session(&self, _session: &Session) -> io::Result<()> {
        Ok(())
    }

    async fn load_session(&self, _session_id: &str) -> io::Result<Option<Session>> {
        Ok(None)
    }

    async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
        Ok(vec![])
    }

    async fn delete_session(&self, _session_id: &str) -> io::Result<()> {
        Ok(())
    }

    async fn list_session_ids(&self) -> io::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn append_log_entry(&self, _session_id: &str, _entry: &LogEntry) -> io::Result<()> {
        Ok(())
    }

    async fn load_log(&self, _session_id: &str) -> io::Result<Vec<LogEntry>> {
        Ok(vec![])
    }

    async fn create_session_storage(&self, _session_id: &str) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> Session {
        Session::builder("s1", "macp.mode.decision.v1", "alice")
            .ttl_expiry(61_000)
            .ttl_ms(60_000)
            .started_at_unix_ms(1_000)
            .participants(vec!["alice".into()])
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .policy_version("pol-1")
            .build()
    }

    #[tokio::test]
    async fn memory_backend_is_noop() {
        let backend = MemoryBackend;
        backend.create_session_storage("s1").await.unwrap();
        backend.save_session(&sample_session()).await.unwrap();
        assert!(backend.load_session("s1").await.unwrap().is_none());
        assert!(backend.load_all_sessions().await.unwrap().is_empty());
        assert!(backend.list_session_ids().await.unwrap().is_empty());
        backend.delete_session("s1").await.unwrap();
    }
}
