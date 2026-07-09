use crate::log_store::LogEntry;
use macp_core::session::Session;
use std::fs;
use std::path::Path;

pub fn recover_session(session: &mut Session, log_entries: &[LogEntry]) {
    // Ensure all log entry message IDs are in the session's dedup set.
    // If the runtime crashed after writing a log entry but before persisting
    // the session snapshot, there will be entries in the log not reflected
    // in seen_message_ids.
    let mut recovered = 0usize;
    for entry in log_entries {
        if !entry.message_id.is_empty() && session.seen_message_ids.insert(entry.message_id.clone())
        {
            recovered += 1;
        }
    }
    if recovered > 0 {
        eprintln!(
            "recovery: session '{}' reconciled {} log entries into dedup state",
            session.session_id, recovered
        );
    }
}

pub fn cleanup_temp_files(base_dir: &Path) {
    let sessions_dir = base_dir.join("sessions");
    if !sessions_dir.exists() {
        return;
    }
    if let Ok(entries) = fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir = entry.path();
            if let Ok(files) = fs::read_dir(&dir) {
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("tmp") {
                        eprintln!("recovery: removing orphaned temp file {}", path.display());
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::EntryKind;

    use std::collections::HashSet;

    fn sample_session() -> Session {
        Session::builder("s1", "macp.mode.decision.v1", "alice")
            .ttl_expiry(61_000)
            .ttl_ms(60_000)
            .started_at_unix_ms(1_000)
            .participants(vec!["alice".into()])
            .seen_message_ids(HashSet::from(["m1".into()]))
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .policy_version("pol-1")
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

    #[test]
    fn crash_recovery_reconciles_dedup_state() {
        let mut session = sample_session();
        assert!(session.seen_message_ids.contains("m1"));
        assert!(!session.seen_message_ids.contains("m2"));
        assert!(!session.seen_message_ids.contains("m3"));

        let entries = vec![sample_entry("m1"), sample_entry("m2"), sample_entry("m3")];

        recover_session(&mut session, &entries);

        assert!(session.seen_message_ids.contains("m1"));
        assert!(session.seen_message_ids.contains("m2"));
        assert!(session.seen_message_ids.contains("m3"));
    }

    #[test]
    fn recovery_ignores_entries_with_empty_message_id() {
        let mut session = sample_session();
        let before = session.seen_message_ids.len();

        let entries = vec![sample_entry(""), sample_entry("m2")];
        recover_session(&mut session, &entries);

        assert!(!session.seen_message_ids.contains(""));
        assert!(session.seen_message_ids.contains("m2"));
        assert_eq!(session.seen_message_ids.len(), before + 1);
    }

    #[test]
    fn recovery_is_idempotent() {
        let mut session = sample_session();
        let entries = vec![sample_entry("m1"), sample_entry("m2"), sample_entry("m3")];

        recover_session(&mut session, &entries);
        let after_first = session.seen_message_ids.clone();

        recover_session(&mut session, &entries);
        assert_eq!(session.seen_message_ids, after_first);
        assert_eq!(session.seen_message_ids.len(), after_first.len());
    }

    #[test]
    fn cleanup_temp_files_removes_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let sessions_dir = base.join("sessions").join("s1");
        fs::create_dir_all(&sessions_dir).unwrap();

        fs::write(sessions_dir.join("session.json.tmp"), b"partial").unwrap();
        assert!(sessions_dir.join("session.json.tmp").exists());

        cleanup_temp_files(base);

        assert!(!sessions_dir.join("session.json.tmp").exists());
    }

    #[test]
    fn cleanup_temp_files_preserves_non_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let session_dir = base.join("sessions").join("s1");
        fs::create_dir_all(&session_dir).unwrap();

        fs::write(session_dir.join("session.json"), b"{}").unwrap();
        fs::write(session_dir.join("log.jsonl"), b"").unwrap();
        fs::write(session_dir.join("session.json.tmp"), b"partial").unwrap();

        cleanup_temp_files(base);

        assert!(session_dir.join("session.json").exists());
        assert!(session_dir.join("log.jsonl").exists());
        assert!(!session_dir.join("session.json.tmp").exists());
    }

    #[test]
    fn cleanup_temp_files_handles_missing_sessions_dir() {
        let dir = tempfile::tempdir().unwrap();
        // No sessions/ dir at all: must be a silent no-op, not a panic.
        cleanup_temp_files(dir.path());
    }
}
