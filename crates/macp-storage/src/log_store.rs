use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EntryKind {
    Incoming,
    Internal,
    Checkpoint,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LogEntry {
    pub message_id: String,
    pub received_at_ms: i64,
    pub sender: String,
    pub message_type: String,
    pub raw_payload: Vec<u8>,
    pub entry_kind: EntryKind,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub macp_version: String,
    /// Original envelope timestamp for replay determinism.
    #[serde(default)]
    pub timestamp_unix_ms: i64,
}

pub struct LogStore {
    logs: RwLock<HashMap<String, Vec<LogEntry>>>,
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LogStore {
    pub fn new() -> Self {
        Self {
            logs: RwLock::new(HashMap::new()),
        }
    }

    pub async fn create_session_log(&self, session_id: &str) {
        let mut guard = self.logs.write().await;
        guard.entry(session_id.to_string()).or_default();
    }

    pub async fn append(&self, session_id: &str, entry: LogEntry) {
        let mut guard = self.logs.write().await;
        guard.entry(session_id.to_string()).or_default().push(entry);
    }

    pub async fn get_log(&self, session_id: &str) -> Option<Vec<LogEntry>> {
        let guard = self.logs.read().await;
        guard.get(session_id).cloned()
    }

    /// Returns incoming log entries with 0-based index >= `after_sequence`.
    /// Only `Incoming` entries are returned (Internal/Checkpoint are filtered).
    /// RFC-MACP-0006-A1: used by StreamSession passive subscribe to replay
    /// accepted session history to late-joining agents.
    pub async fn get_incoming_after(
        &self,
        session_id: &str,
        after_sequence: u64,
    ) -> Vec<(u64, LogEntry)> {
        let guard = self.logs.read().await;
        guard
            .get(session_id)
            .map(|entries| {
                entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.entry_kind == EntryKind::Incoming)
                    .filter(|(idx, _)| *idx as u64 >= after_sequence)
                    .map(|(idx, e)| (idx as u64, e.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kind: EntryKind) -> LogEntry {
        LogEntry {
            message_id: id.into(),
            received_at_ms: 1_700_000_000_000,
            sender: "test".into(),
            message_type: "Message".into(),
            raw_payload: vec![],
            entry_kind: kind,
            session_id: String::new(),
            mode: String::new(),
            macp_version: String::new(),
            timestamp_unix_ms: 1_700_000_000_000,
        }
    }

    #[tokio::test]
    async fn create_append_get_round_trip() {
        let store = LogStore::new();
        store.create_session_log("s1").await;
        store.append("s1", entry("m1", EntryKind::Incoming)).await;
        store.append("s1", entry("m2", EntryKind::Incoming)).await;

        let log = store.get_log("s1").await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].message_id, "m1");
        assert_eq!(log[1].message_id, "m2");
    }

    #[tokio::test]
    async fn get_incoming_after_filters_by_sequence_and_kind() {
        let store = LogStore::new();
        store.create_session_log("s1").await;
        store.append("s1", entry("m0", EntryKind::Incoming)).await;
        store.append("s1", entry("m1", EntryKind::Internal)).await;
        store.append("s1", entry("m2", EntryKind::Incoming)).await;
        store.append("s1", entry("m3", EntryKind::Incoming)).await;
        store.append("s1", entry("m4", EntryKind::Checkpoint)).await;

        // after_sequence=0 returns all Incoming entries
        let all = store.get_incoming_after("s1", 0).await;
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].1.message_id, "m0");
        assert_eq!(all[1].1.message_id, "m2");
        assert_eq!(all[2].1.message_id, "m3");

        // after_sequence=2 skips index 0 and 1
        let after2 = store.get_incoming_after("s1", 2).await;
        assert_eq!(after2.len(), 2);
        assert_eq!(after2[0].0, 2); // index
        assert_eq!(after2[0].1.message_id, "m2");
        assert_eq!(after2[1].0, 3);
        assert_eq!(after2[1].1.message_id, "m3");

        // nonexistent session returns empty
        let empty = store.get_incoming_after("nope", 0).await;
        assert!(empty.is_empty());
    }
}
