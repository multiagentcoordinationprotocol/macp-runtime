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
    /// Mode version bound at SessionStart acceptance when the payload's own
    /// `mode_version` was empty (non-strict extension modes take the registered
    /// descriptor's version). Replay uses this recorded binding and never
    /// re-derives it from the live registry. `None` on legacy entries and on
    /// entries whose payload carried the version explicitly — legacy histories
    /// keep their original (empty-version) binding semantics.
    #[serde(default)]
    pub bound_mode_version: Option<String>,
    /// Session-semantics revision recorded on the SessionStart entry (see
    /// `macp_core::session::CURRENT_SEMANTICS_REV`). Legacy entries
    /// deserialize as 0; replay applies the recorded revision so old
    /// histories keep the acceptance-time behavior they were written under.
    #[serde(default)]
    pub semantics_rev: u32,
    /// Suspension cap resolved and bound at SessionStart acceptance
    /// (payload value, or the runtime default when the payload carried 0).
    /// Replay uses this recorded value, never live configuration
    /// (RFC-MACP-0003 §2). `None` on legacy entries — those sessions keep
    /// default-cap semantics.
    #[serde(default)]
    pub bound_max_suspend_ms: Option<i64>,
    /// On `Checkpoint` entries produced by log compaction: how many accepted
    /// (Incoming) envelope ordinals the compaction discarded. Ordinals of
    /// entries after the checkpoint continue from this base, keeping the
    /// passive-subscribe sequence stable across compaction and restart.
    /// `0` on all other entries and on legacy checkpoints.
    #[serde(default)]
    pub compacted_incoming_ordinals: u64,
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

    /// Returns accepted (Incoming) log entries strictly after `after_sequence`,
    /// paired with their **1-based accepted-envelope ordinal**.
    ///
    /// Sequence contract (RFC-MACP-0006 §3.2): the per-session sequence is the
    /// ordinal of accepted session envelopes — the first accepted envelope
    /// (SessionStart) is 1. `after_sequence` is EXCLUSIVE: `0` replays from
    /// the start, `n` resumes after the n-th accepted envelope. Internal and
    /// Checkpoint entries never consume ordinals, so client-visible sequences
    /// are contiguous and stable regardless of interleaved internal records.
    ///
    /// (The previous implementation compared against the raw combined log
    /// index inclusively — non-contiguous, shifting with internal entries,
    /// and off by one against the RFC's `after_sequence + 1` replay rule.)
    pub async fn get_incoming_after(
        &self,
        session_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<(u64, LogEntry)>, u64> {
        let guard = self.logs.read().await;
        let Some(entries) = guard.get(session_id) else {
            return Ok(Vec::new());
        };
        // Compaction may have replaced older entries with a checkpoint that
        // records how many accepted ordinals it discarded; remaining Incoming
        // entries continue from that base. A resume below the base asks for
        // history that no longer exists — surfaced as Err(base) so the
        // transport can return a clear error instead of silently skipping.
        let base: u64 = entries
            .iter()
            .filter(|e| e.entry_kind == EntryKind::Checkpoint)
            .map(|e| e.compacted_incoming_ordinals)
            .max()
            .unwrap_or(0);
        if after_sequence < base {
            return Err(base);
        }
        Ok(entries
            .iter()
            .filter(|e| e.entry_kind == EntryKind::Incoming)
            .enumerate()
            .map(|(i, e)| (base + (i + 1) as u64, e))
            .filter(|(ordinal, _)| *ordinal > after_sequence)
            .map(|(ordinal, e)| (ordinal, e.clone()))
            .collect())
    }

    /// Drop a session's in-memory log (eviction). The durable log remains in
    /// storage; a later restart replays it if needed.
    pub async fn remove_session_log(&self, session_id: &str) {
        let mut guard = self.logs.write().await;
        guard.remove(session_id);
    }

    /// Replace a session's in-memory log wholesale — used by compaction so
    /// memory and storage stay in step (previously only disk was rewritten,
    /// leaving divergent in-memory history until restart).
    pub async fn replace_session_log(&self, session_id: &str, entries: Vec<LogEntry>) {
        let mut guard = self.logs.write().await;
        guard.insert(session_id.to_string(), entries);
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
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
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
    async fn get_incoming_after_uses_accepted_ordinals_exclusive() {
        let store = LogStore::new();
        store.create_session_log("s1").await;
        store.append("s1", entry("m0", EntryKind::Incoming)).await; // ordinal 1
        store.append("s1", entry("m1", EntryKind::Internal)).await; // no ordinal
        store.append("s1", entry("m2", EntryKind::Incoming)).await; // ordinal 2
        store.append("s1", entry("m3", EntryKind::Incoming)).await; // ordinal 3
        store.append("s1", entry("m4", EntryKind::Checkpoint)).await; // no ordinal

        // after_sequence=0: from the start (RFC-0006 §3.2), all Incoming,
        // 1-based contiguous ordinals unaffected by interleaved internal
        // entries.
        let all = store.get_incoming_after("s1", 0).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!((all[0].0, all[0].1.message_id.as_str()), (1, "m0"));
        assert_eq!((all[1].0, all[1].1.message_id.as_str()), (2, "m2"));
        assert_eq!((all[2].0, all[2].1.message_id.as_str()), (3, "m3"));

        // after_sequence is EXCLUSIVE: a client that saw ordinal 2 resumes
        // with after=2 and receives only ordinal 3 (no re-delivery).
        let after2 = store.get_incoming_after("s1", 2).await.unwrap();
        assert_eq!(after2.len(), 1);
        assert_eq!(after2[0].0, 3);
        assert_eq!(after2[0].1.message_id, "m3");

        // nonexistent session returns empty
        let empty = store.get_incoming_after("nope", 0).await.unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn get_incoming_after_ordinals_survive_compaction() {
        let store = LogStore::new();
        store.create_session_log("s1").await;
        // A compaction checkpoint that discarded 5 accepted ordinals, then
        // two post-compaction accepted entries: their ordinals continue at 6.
        let mut cp = entry("cp", EntryKind::Checkpoint);
        cp.compacted_incoming_ordinals = 5;
        store.append("s1", cp).await;
        store.append("s1", entry("m6", EntryKind::Incoming)).await;
        store.append("s1", entry("m7", EntryKind::Incoming)).await;

        let after5 = store.get_incoming_after("s1", 5).await.unwrap();
        assert_eq!(after5.len(), 2);
        assert_eq!(after5[0].0, 6);
        assert_eq!(after5[1].0, 7);

        let after6 = store.get_incoming_after("s1", 6).await.unwrap();
        assert_eq!(after6.len(), 1);
        assert_eq!(after6[0].1.message_id, "m7");

        // Resuming below the compaction base is an error (history gone),
        // never a silent skip.
        assert!(matches!(store.get_incoming_after("s1", 3).await, Err(5)));
    }
}
