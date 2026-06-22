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
        use macp_core::session::SessionState;
        use std::collections::HashSet;

        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry: 61_000,
            ttl_ms: 60_000,
            started_at_unix_ms: 1_000,
            resolution: None,
            mode: "macp.mode.decision.v1".into(),
            mode_state: vec![],
            participants: vec!["alice".into()],
            seen_message_ids: HashSet::new(),
            intent: "".into(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: "pol-1".into(),
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            initiator_sender: "alice".into(),
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: None,
        }
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
