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
