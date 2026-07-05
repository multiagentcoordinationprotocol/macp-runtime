use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use macp_core::session::Session;
use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, WriteBatchWithTransaction, DB};
use std::io;
use std::path::Path;

use super::StorageBackend;

const CF_SESSIONS: &str = "sessions";
const CF_LOGS: &str = "logs";

pub struct RocksDbBackend {
    db: DB,
}

impl RocksDbBackend {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_sessions = ColumnFamilyDescriptor::new(CF_SESSIONS, Options::default());
        let cf_logs = ColumnFamilyDescriptor::new(CF_LOGS, Options::default());

        let db = DB::open_cf_descriptors(&opts, path, vec![cf_sessions, cf_logs])
            .map_err(io::Error::other)?;

        Ok(Self { db })
    }

    /// Composite key for log entries: `{session_id}\x00{seq:08}`
    fn log_key(session_id: &str, seq: u64) -> Vec<u8> {
        format!("{session_id}\x00{seq:08}").into_bytes()
    }

    /// Prefix for iterating all log entries of a session: `{session_id}\x00`
    fn log_prefix(session_id: &str) -> Vec<u8> {
        format!("{session_id}\x00").into_bytes()
    }

    /// Find the next sequence number for a session's log entries.
    fn next_seq(&self, session_id: &str) -> io::Result<u64> {
        let cf = self
            .db
            .cf_handle(CF_LOGS)
            .ok_or_else(|| io::Error::other("missing logs CF"))?;

        let prefix = Self::log_prefix(session_id);

        // Build the upper bound: increment the last byte before \x00
        // so the iterator only covers keys with this prefix.
        let mut upper_bound = prefix.clone();
        // Replace trailing \x00 with \x01 to create an exclusive upper bound
        if let Some(last) = upper_bound.last_mut() {
            *last = 0x01;
        }

        let mut read_opts = rocksdb::ReadOptions::default();
        read_opts.set_iterate_upper_bound(upper_bound);

        let mut iter = self.db.iterator_cf_opt(&cf, read_opts, IteratorMode::End);

        if let Some(item) = iter.next() {
            let (key, _) = item.map_err(io::Error::other)?;
            if key.starts_with(&prefix) {
                let key_str = String::from_utf8_lossy(&key);
                if let Some(seq_str) = key_str.rsplit('\x00').next() {
                    if let Ok(seq) = seq_str.parse::<u64>() {
                        return Ok(seq + 1);
                    }
                }
            }
        }
        Ok(0)
    }
}

#[async_trait::async_trait]
impl StorageBackend for RocksDbBackend {
    async fn create_session_storage(&self, _session_id: &str) -> io::Result<()> {
        // Column families are created at DB open time; nothing to do per-session.
        Ok(())
    }

    async fn save_session(&self, session: &Session) -> io::Result<()> {
        let cf = self
            .db
            .cf_handle(CF_SESSIONS)
            .ok_or_else(|| io::Error::other("missing sessions CF"))?;
        let persisted = PersistedSession::from(session);
        let bytes = serde_json::to_vec(&persisted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.db
            .put_cf(&cf, session.session_id.as_bytes(), &bytes)
            .map_err(io::Error::other)
    }

    async fn load_session(&self, session_id: &str) -> io::Result<Option<Session>> {
        let cf = self
            .db
            .cf_handle(CF_SESSIONS)
            .ok_or_else(|| io::Error::other("missing sessions CF"))?;
        match self
            .db
            .get_cf(&cf, session_id.as_bytes())
            .map_err(io::Error::other)?
        {
            Some(bytes) => {
                let persisted: PersistedSession = serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(Session::from(persisted)))
            }
            None => Ok(None),
        }
    }

    async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
        let ids = self.list_session_ids().await?;
        let mut sessions = Vec::new();
        for id in ids {
            if let Some(s) = self.load_session(&id).await? {
                sessions.push(s);
            }
        }
        Ok(sessions)
    }

    async fn delete_session(&self, session_id: &str) -> io::Result<()> {
        let sessions_cf = self
            .db
            .cf_handle(CF_SESSIONS)
            .ok_or_else(|| io::Error::other("missing sessions CF"))?;
        let logs_cf = self
            .db
            .cf_handle(CF_LOGS)
            .ok_or_else(|| io::Error::other("missing logs CF"))?;

        let mut batch = WriteBatchWithTransaction::<false>::default();

        // Delete session record
        batch.delete_cf(&sessions_cf, session_id.as_bytes());

        // Delete all log entries with this session's prefix
        let prefix = Self::log_prefix(session_id);
        let iter = self.db.prefix_iterator_cf(&logs_cf, &prefix);
        for item in iter {
            let (key, _) = item.map_err(io::Error::other)?;
            if !key.starts_with(&prefix) {
                break;
            }
            batch.delete_cf(&logs_cf, &key);
        }

        self.db.write(batch).map_err(io::Error::other)
    }

    async fn list_session_ids(&self) -> io::Result<Vec<String>> {
        let cf = self
            .db
            .cf_handle(CF_SESSIONS)
            .ok_or_else(|| io::Error::other("missing sessions CF"))?;
        let mut ids = Vec::new();
        let iter = self.db.iterator_cf(&cf, IteratorMode::Start);
        for item in iter {
            let (key, _) = item.map_err(io::Error::other)?;
            ids.push(String::from_utf8_lossy(&key).to_string());
        }
        Ok(ids)
    }

    async fn append_log_entry(&self, session_id: &str, entry: &LogEntry) -> io::Result<()> {
        let cf = self
            .db
            .cf_handle(CF_LOGS)
            .ok_or_else(|| io::Error::other("missing logs CF"))?;
        let seq = self.next_seq(session_id)?;
        let key = Self::log_key(session_id, seq);
        let bytes =
            serde_json::to_vec(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        // The log append is the runtime's COMMIT POINT: an acked message must
        // survive a crash. Default WriteOptions leave the write in the WAL's
        // OS buffer (sync=false) — fsync the WAL before acknowledging, giving
        // this backend the same durability contract as the file backend.
        // (Session snapshots stay async: the log is the source of truth and
        // snapshots are reconstructed via replay.)
        let mut write_opts = rocksdb::WriteOptions::default();
        write_opts.set_sync(true);
        self.db
            .put_cf_opt(&cf, &key, &bytes, &write_opts)
            .map_err(io::Error::other)
    }

    async fn load_log(&self, session_id: &str) -> io::Result<Vec<LogEntry>> {
        let cf = self
            .db
            .cf_handle(CF_LOGS)
            .ok_or_else(|| io::Error::other("missing logs CF"))?;
        let prefix = Self::log_prefix(session_id);
        let mut entries = Vec::new();
        let iter = self.db.prefix_iterator_cf(&cf, &prefix);
        for item in iter {
            let (key, value) = item.map_err(io::Error::other)?;
            if !key.starts_with(&prefix) {
                break;
            }
            // Corrupt-entry parity with the file backend: skip the entry with
            // a warning instead of failing the whole session load. One bad
            // record must not take down every other entry in the log
            // (`MACP_STRICT_RECOVERY` gating happens at the recovery layer).
            match serde_json::from_slice::<LogEntry>(&value) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!(
                        session_id,
                        key = %String::from_utf8_lossy(&key),
                        error = %e,
                        "skipping corrupt log entry"
                    );
                }
            }
        }
        Ok(entries)
    }

    async fn replace_log(&self, session_id: &str, entries: &[LogEntry]) -> io::Result<()> {
        let cf = self
            .db
            .cf_handle(CF_LOGS)
            .ok_or_else(|| io::Error::other("missing logs CF"))?;

        let mut batch = WriteBatchWithTransaction::<false>::default();

        // Delete all existing log entries
        let prefix = Self::log_prefix(session_id);
        let iter = self.db.prefix_iterator_cf(&cf, &prefix);
        for item in iter {
            let (key, _) = item.map_err(io::Error::other)?;
            if !key.starts_with(&prefix) {
                break;
            }
            batch.delete_cf(&cf, &key);
        }

        // Insert new entries
        for (i, entry) in entries.iter().enumerate() {
            let key = Self::log_key(session_id, i as u64);
            let bytes = serde_json::to_vec(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            batch.put_cf(&cf, &key, &bytes);
        }

        self.db.write(batch).map_err(io::Error::other)
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
    async fn rocksdb_session_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        backend.create_session_storage("s1").await.unwrap();
        backend.save_session(&sample_session("s1")).await.unwrap();

        let loaded = backend.load_session("s1").await.unwrap().unwrap();
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.ttl_ms, 60_000);
        assert!(loaded.seen_message_ids.contains("m1"));
    }

    #[tokio::test]
    async fn rocksdb_log_append_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        backend.create_session_storage("s1").await.unwrap();
        for id in ["m1", "m2", "m3"] {
            backend
                .append_log_entry("s1", &sample_entry(id))
                .await
                .unwrap();
        }

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].message_id, "m1");
        assert_eq!(log[1].message_id, "m2");
        assert_eq!(log[2].message_id, "m3");
    }

    #[tokio::test]
    async fn rocksdb_list_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        for id in ["s1", "s2"] {
            backend.save_session(&sample_session(id)).await.unwrap();
            backend
                .append_log_entry(id, &sample_entry("m1"))
                .await
                .unwrap();
        }

        let mut ids = backend.list_session_ids().await.unwrap();
        ids.sort();
        assert_eq!(ids, vec!["s1", "s2"]);

        backend.delete_session("s1").await.unwrap();
        assert!(backend.load_session("s1").await.unwrap().is_none());
        assert!(backend.load_log("s1").await.unwrap().is_empty());

        // Idempotent delete
        backend.delete_session("s1").await.unwrap();

        let ids = backend.list_session_ids().await.unwrap();
        assert_eq!(ids, vec!["s2"]);
    }

    #[tokio::test]
    async fn rocksdb_replace_log() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        for i in 0..5 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{i}")))
                .await
                .unwrap();
        }
        assert_eq!(backend.load_log("s1").await.unwrap().len(), 5);

        let replacement = vec![sample_entry("checkpoint")];
        backend.replace_log("s1", &replacement).await.unwrap();

        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message_id, "checkpoint");
    }

    #[tokio::test]
    async fn rocksdb_load_all_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        for id in ["s1", "s2", "s3"] {
            backend.save_session(&sample_session(id)).await.unwrap();
        }

        let mut sessions = backend.load_all_sessions().await.unwrap();
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(sessions.len(), 3);
        assert_eq!(sessions[0].session_id, "s1");
    }

    #[tokio::test]
    async fn rocksdb_logs_isolated_between_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path().join("db")).unwrap();

        backend
            .append_log_entry("s1", &sample_entry("a"))
            .await
            .unwrap();
        backend
            .append_log_entry("s2", &sample_entry("b"))
            .await
            .unwrap();

        let log1 = backend.load_log("s1").await.unwrap();
        assert_eq!(log1.len(), 1);
        assert_eq!(log1[0].message_id, "a");

        let log2 = backend.load_log("s2").await.unwrap();
        assert_eq!(log2.len(), 1);
        assert_eq!(log2[0].message_id, "b");
    }

    /// Corrupt-entry parity with the file backend: one bad record must not
    /// fail the whole session load (it is skipped with a warning).
    #[tokio::test]
    async fn rocksdb_load_log_skips_corrupt_entry() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        backend.create_session_storage("s1").await.unwrap();
        backend
            .append_log_entry("s1", &sample_entry("m1"))
            .await
            .unwrap();

        // Inject a corrupt record at the next sequence position directly.
        let cf = backend.db.cf_handle(CF_LOGS).unwrap();
        let key = RocksDbBackend::log_key("s1", 1);
        backend.db.put_cf(&cf, &key, b"not json").unwrap();

        backend
            .append_log_entry("s1", &sample_entry("m2"))
            .await
            .unwrap();

        let entries = backend.load_log("s1").await.unwrap();
        assert_eq!(entries.len(), 2, "corrupt entry skipped, valid ones kept");
        assert_eq!(entries[0].message_id, "m1");
        assert_eq!(entries[1].message_id, "m2");
    }
}
