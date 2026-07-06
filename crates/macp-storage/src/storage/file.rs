use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use macp_core::session::Session;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tokio::fs as tfs;
use tokio::io::AsyncWriteExt;

use super::StorageBackend;

pub struct FileBackend {
    base_dir: PathBuf,
}

impl FileBackend {
    pub fn new(base_dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(base_dir.join("sessions"))?;
        Ok(Self { base_dir })
    }

    fn session_dir(&self, session_id: &str) -> PathBuf {
        self.base_dir.join("sessions").join(session_id)
    }

    pub(crate) fn session_file(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("session.json")
    }

    pub(crate) fn log_file(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("log.jsonl")
    }

    async fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
        let tmp_path = path.with_extension("json.tmp");
        // Crash-atomic write: fsync the tmp file BEFORE the rename (otherwise
        // the rename can be durable while the data is not, leaving a
        // zero-length or partial file after power loss), then fsync the
        // parent directory so the rename itself is durable.
        {
            let mut file = tfs::File::create(&tmp_path).await?;
            tokio::io::AsyncWriteExt::write_all(&mut file, data).await?;
            file.sync_data().await?;
        }
        tfs::rename(&tmp_path, path).await?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = tfs::File::open(parent).await {
                let _ = dir.sync_data().await;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl StorageBackend for FileBackend {
    async fn create_session_storage(&self, session_id: &str) -> io::Result<()> {
        tfs::create_dir_all(self.session_dir(session_id)).await
    }

    async fn save_session(&self, session: &Session) -> io::Result<()> {
        let persisted = PersistedSession::from(session);
        let bytes = serde_json::to_vec_pretty(&persisted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Self::atomic_write(&self.session_file(&session.session_id), &bytes).await
    }

    async fn load_session(&self, session_id: &str) -> io::Result<Option<Session>> {
        let path = self.session_file(session_id);
        if tfs::metadata(&path).await.is_err() {
            return Ok(None);
        }
        let bytes = tfs::read(&path).await?;
        let persisted: PersistedSession = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(Session::from(persisted)))
    }

    async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
        let ids = self.list_session_ids().await?;
        let mut sessions = Vec::new();
        for id in ids {
            match self.load_session(&id).await {
                Ok(Some(s)) => sessions.push(s),
                Ok(None) => {}
                Err(e) => {
                    eprintln!("warning: failed to load session {id}: {e}; skipping");
                }
            }
        }
        Ok(sessions)
    }

    async fn delete_session(&self, session_id: &str) -> io::Result<()> {
        let dir = self.session_dir(session_id);
        if tfs::metadata(&dir).await.is_ok() {
            tfs::remove_dir_all(&dir).await?;
        }
        Ok(())
    }

    async fn list_session_ids(&self) -> io::Result<Vec<String>> {
        let sessions_dir = self.base_dir.join("sessions");
        if tfs::metadata(&sessions_dir).await.is_err() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        let mut entries = tfs::read_dir(&sessions_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
        Ok(ids)
    }

    async fn append_log_entry(&self, session_id: &str, entry: &LogEntry) -> io::Result<()> {
        let path = self.log_file(session_id);
        let mut line = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        let mut file = tfs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        file.write_all(line.as_bytes()).await?;
        file.sync_data().await?;
        Ok(())
    }

    async fn load_log(&self, session_id: &str) -> io::Result<Vec<LogEntry>> {
        let path = self.log_file(session_id);
        if tfs::metadata(&path).await.is_err() {
            return Ok(vec![]);
        }
        let content = tfs::read_to_string(&path).await?;
        let mut entries = Vec::new();
        for (line_num, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LogEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    eprintln!(
                        "warning: failed to parse log entry at {}:{}: {e}; skipping",
                        path.display(),
                        line_num + 1
                    );
                }
            }
        }
        Ok(entries)
    }

    async fn replace_log(&self, session_id: &str, entries: &[LogEntry]) -> io::Result<()> {
        let path = self.log_file(session_id);
        let mut data = String::new();
        for entry in entries {
            let line = serde_json::to_string(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            data.push_str(&line);
            data.push('\n');
        }
        let tmp_path = path.with_extension("jsonl.tmp");
        tfs::write(&tmp_path, data.as_bytes()).await?;
        tfs::rename(&tmp_path, &path).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_store::EntryKind;

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
    async fn file_backend_session_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();

        let session = sample_session("s1");
        backend.create_session_storage("s1").await.unwrap();
        backend.save_session(&session).await.unwrap();

        let loaded = backend.load_session("s1").await.unwrap().unwrap();
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.ttl_ms, 60_000);
        assert_eq!(loaded.mode_version, "1.0.0");
        assert!(loaded.seen_message_ids.contains("m1"));
        assert_eq!(loaded.participants, vec!["alice", "bob"]);
    }

    #[tokio::test]
    async fn file_backend_log_append_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();

        backend.create_session_storage("s1").await.unwrap();
        backend
            .append_log_entry("s1", &sample_entry("m1"))
            .await
            .unwrap();
        backend
            .append_log_entry("s1", &sample_entry("m2"))
            .await
            .unwrap();
        backend
            .append_log_entry("s1", &sample_entry("m3"))
            .await
            .unwrap();

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].message_id, "m1");
        assert_eq!(log[1].message_id, "m2");
        assert_eq!(log[2].message_id, "m3");
    }

    #[tokio::test]
    async fn file_backend_load_all_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();

        for id in ["s1", "s2", "s3"] {
            backend.create_session_storage(id).await.unwrap();
            backend.save_session(&sample_session(id)).await.unwrap();
        }

        let mut sessions = backend.load_all_sessions().await.unwrap();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].session_id, "s1");
        assert_eq!(sessions[1].session_id, "s2");
        assert_eq!(sessions[2].session_id, "s3");
    }

    #[tokio::test]
    async fn append_only_no_full_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        for i in 0..100 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{}", i)))
                .await
                .unwrap();
        }

        let content = fs::read_to_string(backend.log_file("s1")).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 100);

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 100);
    }

    #[tokio::test]
    async fn write_ordering_log_before_session() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        backend
            .append_log_entry("s1", &sample_entry("m1"))
            .await
            .unwrap();

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message_id, "m1");

        assert!(backend.load_session("s1").await.unwrap().is_none());

        backend.save_session(&sample_session("s1")).await.unwrap();
        assert!(backend.load_session("s1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_session_removes_directory() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();

        backend.create_session_storage("s1").await.unwrap();
        backend.save_session(&sample_session("s1")).await.unwrap();
        backend
            .append_log_entry("s1", &sample_entry("m1"))
            .await
            .unwrap();

        assert!(backend.load_session("s1").await.unwrap().is_some());

        backend.delete_session("s1").await.unwrap();
        assert!(backend.load_session("s1").await.unwrap().is_none());
        assert!(backend.load_log("s1").await.unwrap().is_empty());

        // Idempotent
        backend.delete_session("s1").await.unwrap();
    }

    #[tokio::test]
    async fn list_session_ids_returns_directories() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();

        for id in ["s1", "s2", "s3"] {
            backend.create_session_storage(id).await.unwrap();
        }

        let mut ids = backend.list_session_ids().await.unwrap();
        ids.sort();
        assert_eq!(ids, vec!["s1", "s2", "s3"]);
    }

    #[tokio::test]
    async fn replace_log_atomically_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileBackend::new(dir.path().to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        for i in 0..10 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{i}")))
                .await
                .unwrap();
        }
        assert_eq!(backend.load_log("s1").await.unwrap().len(), 10);

        let replacement = vec![sample_entry("checkpoint")];
        backend.replace_log("s1", &replacement).await.unwrap();

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message_id, "checkpoint");
    }

    #[tokio::test]
    async fn ttl_ms_backward_compat_deserialization() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let backend = FileBackend::new(base.to_path_buf()).unwrap();
        backend.create_session_storage("s1").await.unwrap();

        let json = serde_json::json!({
            "session_id": "s1",
            "state": "Open",
            "ttl_expiry": 61000,
            "started_at_unix_ms": 1000,
            "resolution": null,
            "mode": "macp.mode.decision.v1",
            "mode_state": [],
            "participants": ["alice"],
            "seen_message_ids": [],
            "intent": "",
            "mode_version": "1.0.0",
            "configuration_version": "cfg",
            "policy_version": "pol",
            "context": [],
            "roots": [],
            "initiator_sender": "alice"
        });
        fs::write(
            backend.session_file("s1"),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();

        let loaded = backend.load_session("s1").await.unwrap().unwrap();
        assert_eq!(loaded.ttl_ms, 60_000);
    }
}
