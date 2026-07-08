use crate::log_store::{EntryKind, LogEntry};
use crate::registry::PersistedSession;
use macp_core::session::Session;
use std::io;

use super::StorageBackend;

/// Compact a session's log into a single checkpoint entry.
///
/// This replaces all existing log entries with a single `Checkpoint` entry
/// containing the serialized session state. Should only be called on sessions
/// in terminal state (Resolved/Expired/Cancelled).
pub async fn compact_session_log(
    storage: &dyn StorageBackend,
    session_id: &str,
    session: &Session,
    discarded_incoming_ordinals: u64,
) -> io::Result<LogEntry> {
    let persisted = PersistedSession::from(session);
    let raw_payload = serde_json::to_vec(&persisted)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let now = chrono::Utc::now().timestamp_millis();
    let checkpoint = LogEntry {
        message_id: String::new(),
        received_at_ms: now,
        sender: "_runtime".into(),
        message_type: "Checkpoint".into(),
        raw_payload,
        entry_kind: EntryKind::Checkpoint,
        session_id: session_id.into(),
        mode: session.mode.clone(),
        macp_version: String::new(),
        timestamp_unix_ms: now,
        // Checkpoints carry the full serialized session (including its bound
        // mode_version), so no separate binding record is needed here.
        bound_mode_version: None,
        semantics_rev: 0,
        bound_max_suspend_ms: None,
        // Preserve the passive-subscribe sequence across compaction: the
        // checkpoint records how many accepted ordinals it replaced, so
        // post-compaction entries keep contiguous client-visible ordinals
        // and resumes below the base get an explicit error (RFC-0006 §3.2).
        compacted_incoming_ordinals: discarded_incoming_ordinals,
    };

    storage
        .replace_log(session_id, std::slice::from_ref(&checkpoint))
        .await?;
    Ok(checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileBackend;

    use std::collections::HashSet;

    fn sample_session(id: &str) -> Session {
        Session::builder(id, "macp.mode.decision.v1", "alice")
            .ttl_expiry(61_000)
            .ttl_ms(60_000)
            .started_at_unix_ms(1_000)
            .mode_state(vec![1, 2, 3])
            .participants(vec!["alice".into(), "bob".into()])
            .seen_message_ids(HashSet::from(["m1".into()]))
            .intent("test intent")
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .policy_version("pol-1")
            .context_id("test-ctx")
            .roots(vec![macp_pb::pb::Root {
                uri: "root://1".into(),
                name: "r1".into(),
            }])
            .build()
    }

    fn sample_entry(id: &str) -> LogEntry {
        LogEntry {
            message_id: id.into(),
            received_at_ms: 1_700_000_000_000,
            sender: "alice".into(),
            message_type: "Message".into(),
            raw_payload: vec![],
            entry_kind: EntryKind::Incoming,
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
    async fn compaction_replaces_log_with_single_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        for i in 0..5 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{i}")))
                .await
                .unwrap();
        }
        assert_eq!(backend.load_log("s1").await.unwrap().len(), 5);

        let session = sample_session("s1");
        let checkpoint = compact_session_log(&backend, "s1", &session, 5)
            .await
            .unwrap();

        assert_eq!(checkpoint.entry_kind, EntryKind::Checkpoint);
        assert_eq!(checkpoint.compacted_incoming_ordinals, 5);
        assert_eq!(checkpoint.session_id, "s1");
        assert_eq!(checkpoint.mode, "macp.mode.decision.v1");

        // The checkpoint payload must round-trip to the compacted session.
        let persisted: PersistedSession = serde_json::from_slice(&checkpoint.raw_payload).unwrap();
        assert_eq!(persisted.session_id, "s1");
        assert_eq!(persisted.mode, "macp.mode.decision.v1");
        assert_eq!(persisted.participants, vec!["alice", "bob"]);
        assert_eq!(persisted.ttl_ms, 60_000);

        // The durable log now contains exactly the checkpoint entry.
        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].entry_kind, EntryKind::Checkpoint);
        assert_eq!(log[0].compacted_incoming_ordinals, 5);
        let reloaded: PersistedSession = serde_json::from_slice(&log[0].raw_payload).unwrap();
        assert_eq!(reloaded.session_id, "s1");
    }

    #[tokio::test]
    async fn append_after_compaction_keeps_checkpoint_first() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        for i in 0..3 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{i}")))
                .await
                .unwrap();
        }

        let session = sample_session("s1");
        compact_session_log(&backend, "s1", &session, 3)
            .await
            .unwrap();

        backend
            .append_log_entry("s1", &sample_entry("post-compaction"))
            .await
            .unwrap();

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].entry_kind, EntryKind::Checkpoint);
        assert_eq!(log[1].entry_kind, EntryKind::Incoming);
        assert_eq!(log[1].message_id, "post-compaction");
    }
}
