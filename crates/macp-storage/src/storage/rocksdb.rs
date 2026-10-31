use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use macp_core::session::Session;
use rocksdb::{ColumnFamilyDescriptor, IteratorMode, Options, WriteBatchWithTransaction, DB};
use std::io;
use std::path::Path;
use std::sync::Arc;

use super::{ReceiptWrite, StorageBackend};

const CF_SESSIONS: &str = "sessions";
const CF_LOGS: &str = "logs";
/// Decision receipts under `{session_id}`, and trace anchors under
/// `{session_id}\x00anchor` (no column family of its own: a binary that
/// predates anchors cannot open a DB holding an unknown CF, so a new CF
/// would make rollback fail).
///
/// INVARIANT: this CF is accessed by point operations only (`get_cf`,
/// `put_cf`, `delete_cf`), except `sweep_orphan_anchors_blocking`, which knows
/// the anchor key layout. A binary that predates anchors therefore never
/// sees an anchor key. Any new iterator over this CF must handle the
/// `\x00anchor` keys; `receipts_cf_is_point_ops_only` pins this.
const CF_RECEIPTS: &str = "receipts";

/// The suffix of a trace-anchor key in [`CF_RECEIPTS`]. Session ids never
/// hold NUL (`is_storage_safe_session_id`), so it cannot collide with a
/// receipt key.
const ANCHOR_SUFFIX: &[u8] = b"\x00anchor";

pub struct RocksDbBackend {
    /// Shared so a long scan can run on the blocking pool
    /// (`sweep_orphan_trace_anchors`).
    db: Arc<DB>,
    /// Serializes `store_receipt_if_absent`'s get-then-put, so first writer
    /// wins by this backend's own guarantee (RocksDB is single-process).
    receipt_lock: tokio::sync::Mutex<()>,
}

impl RocksDbBackend {
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_sessions = ColumnFamilyDescriptor::new(CF_SESSIONS, Options::default());
        let cf_logs = ColumnFamilyDescriptor::new(CF_LOGS, Options::default());
        let cf_receipts = ColumnFamilyDescriptor::new(CF_RECEIPTS, Options::default());

        let db = DB::open_cf_descriptors(&opts, path, vec![cf_sessions, cf_logs, cf_receipts])
            .map_err(io::Error::other)?;

        Ok(Self {
            db: Arc::new(db),
            receipt_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Composite key for log entries: `{session_id}\x00{seq:08}`
    fn log_key(session_id: &str, seq: u64) -> Vec<u8> {
        format!("{session_id}\x00{seq:08}").into_bytes()
    }

    /// Prefix for iterating all log entries of a session: `{session_id}\x00`
    fn log_prefix(session_id: &str) -> Vec<u8> {
        format!("{session_id}\x00").into_bytes()
    }

    /// The trace-anchor key of `session_id` in [`CF_RECEIPTS`].
    fn anchor_key(session_id: &str) -> Vec<u8> {
        let mut key = session_id.as_bytes().to_vec();
        key.extend_from_slice(ANCHOR_SUFFIX);
        key
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

        // Delete session record and its receipt
        batch.delete_cf(&sessions_cf, session_id.as_bytes());
        let receipts_cf = self
            .db
            .cf_handle(CF_RECEIPTS)
            .ok_or_else(|| io::Error::other("missing receipts CF"))?;
        batch.delete_cf(&receipts_cf, session_id.as_bytes());
        batch.delete_cf(&receipts_cf, Self::anchor_key(session_id));

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

    async fn load_receipt(&self, session_id: &str) -> io::Result<Option<Vec<u8>>> {
        let cf = self
            .db
            .cf_handle(CF_RECEIPTS)
            .ok_or_else(|| io::Error::other("missing receipts CF"))?;
        self.db
            .get_cf(&cf, session_id.as_bytes())
            .map_err(io::Error::other)
    }

    async fn store_receipt_if_absent(
        &self,
        session_id: &str,
        receipt: &[u8],
    ) -> io::Result<ReceiptWrite> {
        let _guard = self.receipt_lock.lock().await;
        if let Some(existing) = self.load_receipt(session_id).await? {
            return Ok(ReceiptWrite::Existing(existing));
        }
        let cf = self
            .db
            .cf_handle(CF_RECEIPTS)
            .ok_or_else(|| io::Error::other("missing receipts CF"))?;
        self.db
            .put_cf(&cf, session_id.as_bytes(), receipt)
            .map_err(io::Error::other)?;
        Ok(ReceiptWrite::Stored)
    }

    fn receipts_durable(&self) -> bool {
        true
    }

    async fn store_trace_anchor(&self, session_id: &str, anchor: &[u8]) -> io::Result<()> {
        super::check_anchor_session_id(session_id)?;
        let cf = self
            .db
            .cf_handle(CF_RECEIPTS)
            .ok_or_else(|| io::Error::other("missing receipts CF"))?;
        self.db
            .put_cf(&cf, Self::anchor_key(session_id), anchor)
            .map_err(io::Error::other)
    }

    async fn load_trace_anchor(&self, session_id: &str) -> io::Result<Option<Vec<u8>>> {
        super::check_anchor_session_id(session_id)?;
        let cf = self
            .db
            .cf_handle(CF_RECEIPTS)
            .ok_or_else(|| io::Error::other("missing receipts CF"))?;
        self.db
            .get_cf(&cf, Self::anchor_key(session_id))
            .map_err(io::Error::other)
    }

    /// A pre-anchor binary's `delete_session` deletes only the `{id}`
    /// receipt key, leaving `{id}\x00anchor` behind. The scan is synchronous
    /// RocksDB work proportional to the receipts CF, so it runs on the
    /// blocking pool, never on an async worker.
    async fn sweep_orphan_trace_anchors(&self) -> io::Result<usize> {
        let db = Arc::clone(&self.db);
        tokio::task::spawn_blocking(move || sweep_orphan_anchors_blocking(&db))
            .await
            .map_err(io::Error::other)?
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

/// Deletes per `WriteBatch` in [`sweep_orphan_anchors_blocking`].
const SWEEP_BATCH: usize = 1_000;

/// Scan the receipts CF and delete every anchor key whose session is absent
/// from the sessions CF, in batches of [`SWEEP_BATCH`]; receipt keys are
/// skipped untouched. Safe against a concurrent SessionStart: the runtime
/// stores a session's anchor only after its snapshot, so an anchor key the
/// scan sees always has its session visible to the check that follows.
fn sweep_orphan_anchors_blocking(db: &DB) -> io::Result<usize> {
    let receipts_cf = db
        .cf_handle(CF_RECEIPTS)
        .ok_or_else(|| io::Error::other("missing receipts CF"))?;
    let sessions_cf = db
        .cf_handle(CF_SESSIONS)
        .ok_or_else(|| io::Error::other("missing sessions CF"))?;
    let mut batch = WriteBatchWithTransaction::<false>::default();
    let mut orphans = 0usize;
    for item in db.iterator_cf(&receipts_cf, IteratorMode::Start) {
        let (key, _) = item.map_err(io::Error::other)?;
        let Some(session_id) = key.strip_suffix(ANCHOR_SUFFIX) else {
            continue;
        };
        if db
            .get_pinned_cf(&sessions_cf, session_id)
            .map_err(io::Error::other)?
            .is_none()
        {
            batch.delete_cf(&receipts_cf, &key);
            orphans += 1;
            if batch.len() >= SWEEP_BATCH {
                db.write(std::mem::take(&mut batch))
                    .map_err(io::Error::other)?;
            }
        }
    }
    if !batch.is_empty() {
        db.write(batch).map_err(io::Error::other)?;
    }
    Ok(orphans)
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
            bound_max_suspend_ms: None,
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

    #[tokio::test]
    async fn rocksdb_receipt_first_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        crate::storage::receipt_contract::first_writer_wins(&backend, "r1").await;
    }

    #[tokio::test]
    async fn rocksdb_receipt_one_winner_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let backend = std::sync::Arc::new(RocksDbBackend::open(dir.path()).unwrap());
        crate::storage::receipt_contract::one_winner_under_concurrency(backend, "r1").await;
    }

    #[tokio::test]
    async fn rocksdb_trace_anchor_contract() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        crate::storage::anchor_contract::store_load_delete(&backend, "a1", true).await;
    }

    /// The anchor shares the receipts CF but never the receipt's key, in
    /// either direction.
    #[tokio::test]
    async fn rocksdb_anchor_and_receipt_keys_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        backend.save_session(&sample_session("s1")).await.unwrap();
        backend.store_trace_anchor("s1", b"anchor").await.unwrap();
        assert_eq!(backend.load_receipt("s1").await.unwrap(), None);
        assert_eq!(
            backend
                .store_receipt_if_absent("s1", b"receipt")
                .await
                .unwrap(),
            ReceiptWrite::Stored
        );
        assert_eq!(
            backend.load_trace_anchor("s1").await.unwrap().as_deref(),
            Some(&b"anchor"[..])
        );
        assert_eq!(
            backend.load_receipt("s1").await.unwrap().as_deref(),
            Some(&b"receipt"[..])
        );
        // A receipt first, then an anchor: still independent.
        backend.save_session(&sample_session("s2")).await.unwrap();
        backend.store_receipt_if_absent("s2", b"r2").await.unwrap();
        assert_eq!(backend.load_trace_anchor("s2").await.unwrap(), None);
        backend.store_trace_anchor("s2", b"a2").await.unwrap();
        assert_eq!(
            backend.load_receipt("s2").await.unwrap().as_deref(),
            Some(&b"r2"[..])
        );
        // Deleting a session removes both of its keys and nothing else.
        backend.delete_session("s1").await.unwrap();
        assert_eq!(backend.load_trace_anchor("s1").await.unwrap(), None);
        assert_eq!(backend.load_receipt("s1").await.unwrap(), None);
        assert!(backend.load_trace_anchor("s2").await.unwrap().is_some());
        assert!(backend.load_receipt("s2").await.unwrap().is_some());
    }

    /// What a pre-anchor (0.11.0) `delete_session` does: the session, its
    /// log and its `{id}` receipt key, never `{id}\x00anchor`.
    fn pre_anchor_delete(backend: &RocksDbBackend, session_id: &str) {
        let mut batch = WriteBatchWithTransaction::<false>::default();
        batch.delete_cf(
            &backend.db.cf_handle(CF_SESSIONS).unwrap(),
            session_id.as_bytes(),
        );
        batch.delete_cf(
            &backend.db.cf_handle(CF_RECEIPTS).unwrap(),
            session_id.as_bytes(),
        );
        let logs = backend.db.cf_handle(CF_LOGS).unwrap();
        let prefix = RocksDbBackend::log_prefix(session_id);
        for item in backend.db.prefix_iterator_cf(&logs, &prefix) {
            let (key, _) = item.unwrap();
            if !key.starts_with(&prefix) {
                break;
            }
            batch.delete_cf(&logs, &key);
        }
        backend.db.write(batch).unwrap();
    }

    /// Orphans left by a pre-anchor GC are swept: only anchor keys whose
    /// session is gone, never a receipt key and never a live session's
    /// anchor. A second sweep finds nothing; nothing anchor-shaped remains.
    #[tokio::test]
    async fn rocksdb_sweep_removes_only_orphaned_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        for id in ["gone1", "gone2", "live"] {
            backend.save_session(&sample_session(id)).await.unwrap();
            backend
                .append_log_entry(id, &sample_entry("m1"))
                .await
                .unwrap();
            backend.store_trace_anchor(id, b"anchor").await.unwrap();
        }
        // A receipt whose session is gone (not an anchor: left alone).
        backend
            .store_receipt_if_absent("receipt-only", b"r")
            .await
            .unwrap();
        pre_anchor_delete(&backend, "gone1");
        pre_anchor_delete(&backend, "gone2");
        assert!(backend.load_trace_anchor("gone1").await.unwrap().is_some());

        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 2);
        assert_eq!(backend.load_trace_anchor("gone1").await.unwrap(), None);
        assert_eq!(backend.load_trace_anchor("gone2").await.unwrap(), None);
        assert!(backend.load_trace_anchor("live").await.unwrap().is_some());
        assert!(backend
            .load_receipt("receipt-only")
            .await
            .unwrap()
            .is_some());
        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 0);

        let cf = backend.db.cf_handle(CF_RECEIPTS).unwrap();
        let anchors: Vec<Vec<u8>> = backend
            .db
            .iterator_cf(&cf, IteratorMode::Start)
            .map(|item| item.unwrap().0.to_vec())
            .filter(|k| k.ends_with(ANCHOR_SUFFIX))
            .collect();
        assert_eq!(anchors, vec![RocksDbBackend::anchor_key("live")]);
    }

    /// Rollback to 0.11.0: this backend's read surface is byte-identical to
    /// the published `macp-storage` 0.11.0 one, and an anchor key changes
    /// none of it. The DB opens with the same three CFs.
    #[tokio::test]
    async fn rocksdb_anchor_is_invisible_to_the_pre_anchor_read_surface() {
        async fn surface(b: &RocksDbBackend) -> (Vec<String>, usize, Vec<Option<Vec<u8>>>) {
            let mut ids = b.list_session_ids().await.unwrap();
            ids.sort();
            let sessions = b.load_all_sessions().await.unwrap().len();
            let mut receipts = Vec::new();
            for id in &ids {
                receipts.push(b.load_receipt(id).await.unwrap());
            }
            (ids, sessions, receipts)
        }
        let (plain, anchored) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        {
            let plain_b = RocksDbBackend::open(plain.path()).unwrap();
            let anchored_b = RocksDbBackend::open(anchored.path()).unwrap();
            for b in [&plain_b, &anchored_b] {
                for id in ["s1", "s2"] {
                    b.save_session(&sample_session(id)).await.unwrap();
                }
                b.store_receipt_if_absent("s1", b"receipt").await.unwrap();
            }
            anchored_b.store_trace_anchor("s1", b"a1").await.unwrap();
            anchored_b.store_trace_anchor("s2", b"a2").await.unwrap();
            assert_eq!(surface(&plain_b).await, surface(&anchored_b).await);
        }
        let mut cfs = DB::list_cf(&Options::default(), anchored.path()).unwrap();
        cfs.sort();
        assert_eq!(cfs, ["default", CF_LOGS, CF_RECEIPTS, CF_SESSIONS]);
    }

    /// The receipts CF is point-ops only outside `sweep_orphan_anchors_blocking`
    /// (see [`CF_RECEIPTS`]): a new iterator over it must handle anchor keys.
    #[test]
    fn receipts_cf_is_point_ops_only() {
        let source = include_str!("rocksdb.rs");
        let code = &source[..source
            .find("#[cfg(test)]\nmod tests")
            .expect("the test module")];
        let start = code
            .find("fn sweep_orphan_anchors_blocking(")
            .expect("the sweep");
        let end = start + code[start..].find("\n}\n").expect("the sweep's end");
        let outside = format!("{}{}", &code[..start], &code[end..]);
        // Every function that takes a receipts-CF handle does point
        // operations only, whatever it names the handle.
        let mut receipt_fns = 0;
        for body in outside.split(" fn ").skip(1) {
            let Some(at) = body.find("cf_handle(CF_RECEIPTS)") else {
                continue;
            };
            receipt_fns += 1;
            // The handle's binding: `let <name> = self.db.cf_handle(..)`.
            let binding = body[..at].rsplit("let ").next().unwrap_or_default();
            let name: String = binding
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            assert!(!name.is_empty(), "receipts-CF handle binding");
            for line in body.lines().filter(|l| l.contains("iterator")) {
                assert!(
                    !line.contains(&format!("&{name}")),
                    "an iterator over the receipts CF outside the sweep: {line}"
                );
            }
        }
        // delete_session, load_receipt, store_receipt_if_absent,
        // store_trace_anchor, load_trace_anchor.
        assert_eq!(receipt_fns, 5, "receipts-CF functions");
    }

    /// More orphans than one batch: every one is deleted, across batches.
    #[tokio::test]
    async fn rocksdb_sweep_deletes_in_batches() {
        let dir = tempfile::tempdir().unwrap();
        let backend = RocksDbBackend::open(dir.path()).unwrap();
        let n = SWEEP_BATCH * 2 + 500;
        for i in 0..n {
            backend
                .store_trace_anchor(&format!("gone-{i}"), b"a")
                .await
                .unwrap();
        }
        backend.save_session(&sample_session("live")).await.unwrap();
        backend.store_trace_anchor("live", b"a").await.unwrap();
        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), n);
        assert_eq!(backend.sweep_orphan_trace_anchors().await.unwrap(), 0);
        assert!(backend.load_trace_anchor("live").await.unwrap().is_some());
        assert_eq!(backend.load_trace_anchor("gone-0").await.unwrap(), None);
    }
}
