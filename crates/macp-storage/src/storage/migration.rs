use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

const STORAGE_VERSION: u32 = 3;

#[derive(serde::Serialize, serde::Deserialize)]
struct StorageVersion {
    version: u32,
}

pub fn write_storage_version(base_dir: &Path) -> io::Result<()> {
    let sv = StorageVersion {
        version: STORAGE_VERSION,
    };
    let bytes = serde_json::to_vec_pretty(&sv)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    fs::write(base_dir.join("storage_version.json"), bytes)
}

pub fn read_storage_version(base_dir: &Path) -> io::Result<Option<u32>> {
    let path = base_dir.join("storage_version.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let sv: StorageVersion = serde_json::from_slice(&bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(sv.version))
}

pub fn migrate_if_needed(base_dir: &Path) -> io::Result<()> {
    let sessions_dir = base_dir.join("sessions");
    let legacy_sessions = base_dir.join("sessions.json");
    let legacy_logs = base_dir.join("logs.json");

    // Already at current version or fresh install (no legacy files, sessions dir exists)
    let current_version = read_storage_version(base_dir)?;
    if sessions_dir.exists() && !legacy_sessions.exists() && !legacy_logs.exists() {
        // v2 → v3: no-op data migration, just bump version.  New LogEntry fields
        // use #[serde(default)] so existing v2 JSONL lines deserialize fine.
        if current_version.unwrap_or(0) < STORAGE_VERSION {
            write_storage_version(base_dir)?;
        }
        return Ok(());
    }

    if !legacy_sessions.exists() && !legacy_logs.exists() && !sessions_dir.exists() {
        write_storage_version(base_dir)?;
        return Ok(());
    }

    // Already migrated from v1
    if sessions_dir.exists() && !legacy_sessions.exists() && !legacy_logs.exists() {
        write_storage_version(base_dir)?;
        return Ok(());
    }

    println!("Migrating legacy storage format to per-session directories...");

    // Load legacy sessions
    let sessions: HashMap<String, PersistedSession> = if legacy_sessions.exists() {
        let bytes = fs::read(&legacy_sessions)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse legacy sessions.json: {e}"),
            )
        })?
    } else {
        HashMap::new()
    };

    // Load legacy logs
    let logs: HashMap<String, Vec<LogEntry>> = if legacy_logs.exists() {
        let bytes = fs::read(&legacy_logs)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse legacy logs.json: {e}"),
            )
        })?
    } else {
        HashMap::new()
    };

    fs::create_dir_all(&sessions_dir)?;

    // Migrate each session
    for (session_id, persisted) in &sessions {
        let dir = sessions_dir.join(session_id);
        fs::create_dir_all(&dir)?;

        // Write session.json
        let session_bytes = serde_json::to_vec_pretty(persisted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(dir.join("session.json"), session_bytes)?;

        // Write log.jsonl
        if let Some(entries) = logs.get(session_id) {
            let mut log_data = String::new();
            for entry in entries {
                let line = serde_json::to_string(entry)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                log_data.push_str(&line);
                log_data.push('\n');
            }
            fs::write(dir.join("log.jsonl"), log_data)?;
        }
    }

    // Also migrate logs for sessions that only appear in logs (not in sessions.json)
    for (session_id, entries) in &logs {
        if sessions.contains_key(session_id) {
            continue;
        }
        let dir = sessions_dir.join(session_id);
        fs::create_dir_all(&dir)?;
        let mut log_data = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            log_data.push_str(&line);
            log_data.push('\n');
        }
        fs::write(dir.join("log.jsonl"), log_data)?;
    }

    // Backup old files instead of deleting
    if legacy_sessions.exists() {
        fs::rename(&legacy_sessions, base_dir.join("sessions.json.migrated"))?;
    }
    if legacy_logs.exists() {
        fs::rename(&legacy_logs, base_dir.join("logs.json.migrated"))?;
    }

    write_storage_version(base_dir)?;
    println!(
        "Migration complete: {} sessions, {} log sets migrated.",
        sessions.len(),
        logs.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::EntryKind;
    use crate::storage::StorageBackend;
    use macp_core::session::Session;
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
            compacted_incoming_ordinals: 0,
        }
    }

    #[tokio::test]
    async fn migration_from_legacy_format() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let session = sample_session("s1");
        let persisted = PersistedSession::from(&session);
        let sessions_map: HashMap<String, PersistedSession> =
            [("s1".into(), persisted)].into_iter().collect();
        fs::write(
            base.join("sessions.json"),
            serde_json::to_vec_pretty(&sessions_map).unwrap(),
        )
        .unwrap();

        let entries = vec![sample_entry("m1"), sample_entry("m2")];
        let logs_map: HashMap<String, Vec<LogEntry>> =
            [("s1".into(), entries)].into_iter().collect();
        fs::write(
            base.join("logs.json"),
            serde_json::to_vec_pretty(&logs_map).unwrap(),
        )
        .unwrap();

        migrate_if_needed(base).unwrap();

        assert!(base.join("sessions/s1/session.json").exists());
        assert!(base.join("sessions/s1/log.jsonl").exists());

        assert!(base.join("sessions.json.migrated").exists());
        assert!(base.join("logs.json.migrated").exists());
        assert!(!base.join("sessions.json").exists());
        assert!(!base.join("logs.json").exists());

        assert_eq!(read_storage_version(base).unwrap(), Some(STORAGE_VERSION));

        let backend = crate::storage::FileBackend::new(base.to_path_buf()).unwrap();
        let loaded = backend.load_session("s1").await.unwrap().unwrap();
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.ttl_ms, 60_000);

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 2);
    }
}
