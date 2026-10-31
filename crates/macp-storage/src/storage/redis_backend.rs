use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use macp_core::session::Session;
use redis::AsyncCommands;
use std::io;

use super::{ReceiptWrite, StorageBackend};

pub struct RedisBackend {
    conn: redis::aio::MultiplexedConnection,
    prefix: String,
    /// `MACP_SESSION_DISK_RETENTION_SECS`: when non-zero, trace-anchor keys
    /// get an expiry (see [`Self::anchor_expiry_secs`]).
    disk_retention_secs: u64,
}

impl RedisBackend {
    pub async fn connect(url: &str, prefix: &str) -> io::Result<Self> {
        let client = redis::Client::open(url).map_err(io::Error::other)?;
        let conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(io::Error::other)?;
        // Durability disclosure: RPUSH acks once the Redis server has the
        // command in memory — there is no WAIT and no fsync barrier, so with
        // default persistence (RDB snapshots) an acknowledged log append can
        // be lost if Redis crashes. This backend is suitable for cache-tier /
        // single-writer deployments; it does NOT provide the file backend's
        // acked-implies-durable contract. (Two runtimes sharing one Redis
        // would also corrupt each other — state authority is in-process.)
        tracing::warn!(
            "Redis storage backend does not guarantee durable acknowledgement; \
             acked messages may be lost on Redis crash (see docs/deployment.md)"
        );
        Ok(Self {
            conn,
            prefix: prefix.into(),
            disk_retention_secs: 0,
        })
    }

    /// Tell the backend the runtime's disk retention
    /// (`MACP_SESSION_DISK_RETENTION_SECS`). With retention > 0 sessions are
    /// garbage-collected, and a binary that predates trace anchors deletes
    /// a session without its anchor key; the key is therefore written with
    /// an expiry, so such an orphan expires by itself. With 0 (the default)
    /// nothing is ever collected, and the key has no expiry.
    pub fn with_disk_retention_secs(mut self, secs: u64) -> Self {
        self.disk_retention_secs = secs;
        self
    }

    /// The expiry of a trace-anchor key, or `None` for no expiry: the disk
    /// retention, plus the longest TTL a session may bind
    /// (`macp_core::session::MAX_TTL_MS`), plus one hour of slack. It never
    /// bounds a live session's lifetime, only how long an orphan survives;
    /// a session suspended past the slack loses only its span links.
    fn anchor_expiry_secs(&self) -> Option<u64> {
        (self.disk_retention_secs > 0).then(|| {
            self.disk_retention_secs
                .saturating_add((macp_core::session::MAX_TTL_MS / 1000) as u64)
                .saturating_add(3600)
        })
    }

    fn anchor_key(&self, session_id: &str) -> String {
        format!("{}:anchor:{}", self.prefix, session_id)
    }

    fn session_key(&self, session_id: &str) -> String {
        format!("{}:session:{}", self.prefix, session_id)
    }

    fn receipt_key(&self, session_id: &str) -> String {
        format!("{}:receipt:{}", self.prefix, session_id)
    }

    fn log_key(&self, session_id: &str) -> String {
        format!("{}:log:{}", self.prefix, session_id)
    }

    fn index_key(&self) -> String {
        format!("{}:sessions", self.prefix)
    }
}

#[async_trait::async_trait]
impl StorageBackend for RedisBackend {
    async fn create_session_storage(&self, session_id: &str) -> io::Result<()> {
        let mut conn = self.conn.clone();
        conn.sadd::<_, _, ()>(self.index_key(), session_id)
            .await
            .map_err(io::Error::other)
    }

    async fn save_session(&self, session: &Session) -> io::Result<()> {
        let mut conn = self.conn.clone();
        let persisted = PersistedSession::from(session);
        let bytes = serde_json::to_vec(&persisted)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        conn.set::<_, _, ()>(&self.session_key(&session.session_id), bytes)
            .await
            .map_err(io::Error::other)
    }

    async fn load_session(&self, session_id: &str) -> io::Result<Option<Session>> {
        let mut conn = self.conn.clone();
        let bytes: Option<Vec<u8>> = conn
            .get(self.session_key(session_id))
            .await
            .map_err(io::Error::other)?;
        match bytes {
            Some(b) => {
                let persisted: PersistedSession = serde_json::from_slice(&b)
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
        let mut conn = self.conn.clone();
        redis::pipe()
            .del(self.session_key(session_id))
            .del(self.log_key(session_id))
            .del(self.receipt_key(session_id))
            .del(self.anchor_key(session_id))
            .srem(self.index_key(), session_id)
            .exec_async(&mut conn)
            .await
            .map_err(io::Error::other)
    }

    async fn load_receipt(&self, session_id: &str) -> io::Result<Option<Vec<u8>>> {
        let mut conn = self.conn.clone();
        conn.get(self.receipt_key(session_id))
            .await
            .map_err(io::Error::other)
    }

    /// `SET NX`: first writer wins across every replica sharing this Redis.
    async fn store_receipt_if_absent(
        &self,
        session_id: &str,
        receipt: &[u8],
    ) -> io::Result<ReceiptWrite> {
        let mut conn = self.conn.clone();
        let stored: bool = conn
            .set_nx(self.receipt_key(session_id), receipt)
            .await
            .map_err(io::Error::other)?;
        if stored {
            return Ok(ReceiptWrite::Stored);
        }
        match self.load_receipt(session_id).await? {
            Some(existing) => Ok(ReceiptWrite::Existing(existing)),
            None => Err(io::Error::other(
                "receipt key vanished between SET NX and GET",
            )),
        }
    }

    fn receipts_durable(&self) -> bool {
        true
    }

    async fn store_trace_anchor(&self, session_id: &str, anchor: &[u8]) -> io::Result<()> {
        super::check_anchor_session_id(session_id)?;
        let mut conn = self.conn.clone();
        let key = self.anchor_key(session_id);
        match self.anchor_expiry_secs() {
            Some(secs) => conn.set_ex::<_, _, ()>(key, anchor, secs).await,
            None => conn.set::<_, _, ()>(key, anchor).await,
        }
        .map_err(io::Error::other)
    }

    async fn load_trace_anchor(&self, session_id: &str) -> io::Result<Option<Vec<u8>>> {
        super::check_anchor_session_id(session_id)?;
        let mut conn = self.conn.clone();
        conn.get(self.anchor_key(session_id))
            .await
            .map_err(io::Error::other)
    }

    async fn list_session_ids(&self) -> io::Result<Vec<String>> {
        let mut conn = self.conn.clone();
        conn.smembers(self.index_key())
            .await
            .map_err(io::Error::other)
    }

    async fn append_log_entry(&self, session_id: &str, entry: &LogEntry) -> io::Result<()> {
        let mut conn = self.conn.clone();
        let bytes =
            serde_json::to_vec(entry).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        conn.rpush::<_, _, ()>(self.log_key(session_id), bytes)
            .await
            .map_err(io::Error::other)
    }

    async fn load_log(&self, session_id: &str) -> io::Result<Vec<LogEntry>> {
        let mut conn = self.conn.clone();
        let items: Vec<Vec<u8>> = conn
            .lrange(self.log_key(session_id), 0, -1)
            .await
            .map_err(io::Error::other)?;
        let mut entries = Vec::with_capacity(items.len());
        for (idx, item) in items.iter().enumerate() {
            // Corrupt-entry parity with the file backend: skip + warn rather
            // than failing the entire session load on one bad record.
            match serde_json::from_slice::<LogEntry>(item) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!(session_id, idx, error = %e, "skipping corrupt log entry");
                }
            }
        }
        Ok(entries)
    }

    async fn replace_log(&self, session_id: &str, entries: &[LogEntry]) -> io::Result<()> {
        let mut conn = self.conn.clone();
        let key = self.log_key(session_id);

        // Atomic swap: DEL + RPUSH* inside one MULTI/EXEC transaction. The
        // previous sequential implementation could fail between the DEL and
        // the pushes, leaving a truncated (or empty) log — silent history
        // loss for the session.
        let mut pipe = redis::pipe();
        pipe.atomic();
        pipe.del(&key);
        for entry in entries {
            let bytes = serde_json::to_vec(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            pipe.rpush(&key, bytes);
        }
        pipe.exec_async(&mut conn).await.map_err(io::Error::other)
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
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    async fn make_backend() -> Option<RedisBackend> {
        let url = std::env::var("MACP_TEST_REDIS_URL").ok()?;
        let prefix = format!("macp_test_{}", uuid::Uuid::new_v4());
        // Silent-skip is only legitimate when the env var is UNSET. When it
        // is set (CI provides a service container), an unreachable Redis is
        // a broken environment and must FAIL — otherwise a dead container
        // green-lights every Redis test (the C1 guard from the master plan).
        match RedisBackend::connect(&url, &prefix).await {
            Ok(backend) => Some(backend),
            Err(e) => panic!(
                "MACP_TEST_REDIS_URL is set but Redis is unreachable ({e}) — \
                 refusing to silently skip"
            ),
        }
    }

    async fn cleanup(backend: &RedisBackend) {
        // Clean up all test keys
        let mut conn = backend.conn.clone();
        let ids: Vec<String> = conn.smembers(backend.index_key()).await.unwrap_or_default();
        for id in &ids {
            let _ = redis::pipe()
                .del(backend.session_key(id))
                .del(backend.log_key(id))
                .del(backend.receipt_key(id))
                .del(backend.anchor_key(id))
                .exec_async(&mut conn)
                .await;
        }
        let _: Result<(), _> = conn.del(backend.index_key()).await;
    }

    #[tokio::test]
    async fn redis_session_round_trip() {
        let Some(backend) = make_backend().await else {
            eprintln!("skipping redis test: MACP_TEST_REDIS_URL not set");
            return;
        };
        backend.create_session_storage("s1").await.unwrap();
        backend.save_session(&sample_session("s1")).await.unwrap();
        let loaded = backend.load_session("s1").await.unwrap().unwrap();
        assert_eq!(loaded.session_id, "s1");
        assert_eq!(loaded.ttl_ms, 60_000);
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_log_append_and_load() {
        let Some(backend) = make_backend().await else {
            return;
        };
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
        assert_eq!(log[2].message_id, "m3");
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_list_and_delete() {
        let Some(backend) = make_backend().await else {
            return;
        };
        for id in ["s1", "s2"] {
            backend.create_session_storage(id).await.unwrap();
            backend.save_session(&sample_session(id)).await.unwrap();
        }
        let mut ids = backend.list_session_ids().await.unwrap();
        ids.sort();
        assert_eq!(ids, vec!["s1", "s2"]);

        backend.delete_session("s1").await.unwrap();
        assert!(backend.load_session("s1").await.unwrap().is_none());

        // Idempotent
        backend.delete_session("s1").await.unwrap();
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_replace_log() {
        let Some(backend) = make_backend().await else {
            return;
        };
        backend.create_session_storage("s1").await.unwrap();
        for i in 0..5 {
            backend
                .append_log_entry("s1", &sample_entry(&format!("m{i}")))
                .await
                .unwrap();
        }
        let replacement = vec![sample_entry("checkpoint")];
        backend.replace_log("s1", &replacement).await.unwrap();
        let log = backend.load_log("s1").await.unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].message_id, "checkpoint");
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_receipt_first_writer_wins() {
        let Some(backend) = make_backend().await else {
            eprintln!("skipping redis test: MACP_TEST_REDIS_URL not set");
            return;
        };
        crate::storage::receipt_contract::first_writer_wins(&backend, "r1").await;
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_receipt_one_winner_under_concurrency() {
        let Some(backend) = make_backend().await else {
            eprintln!("skipping redis test: MACP_TEST_REDIS_URL not set");
            return;
        };
        let backend = std::sync::Arc::new(backend);
        crate::storage::receipt_contract::one_winner_under_concurrency(backend.clone(), "r1").await;
        cleanup(&backend).await;
    }

    #[tokio::test]
    async fn redis_trace_anchor_contract() {
        let Some(backend) = make_backend().await else {
            eprintln!("skipping redis test: MACP_TEST_REDIS_URL not set");
            return;
        };
        crate::storage::anchor_contract::store_load_delete(&backend, "a1", true).await;
        cleanup(&backend).await;
    }

    /// The anchor key expires only when sessions are garbage-collected
    /// (disk retention > 0): `TTL` is -1 (no expiry) with retention 0, and
    /// retention + the longest session TTL + 1 h otherwise.
    #[tokio::test]
    async fn redis_anchor_key_expires_only_with_disk_retention() {
        let Some(backend) = make_backend().await else {
            eprintln!("skipping redis test: MACP_TEST_REDIS_URL not set");
            return;
        };
        let ttl_of = |backend: &RedisBackend, id: &str| {
            let key = backend.anchor_key(id);
            let mut conn = backend.conn.clone();
            async move {
                redis::cmd("TTL")
                    .arg(key)
                    .query_async::<i64>(&mut conn)
                    .await
                    .unwrap()
            }
        };
        backend.create_session_storage("keep").await.unwrap();
        backend.store_trace_anchor("keep", b"a").await.unwrap();
        assert_eq!(
            ttl_of(&backend, "keep").await,
            -1,
            "no expiry at retention 0"
        );

        let backend = backend.with_disk_retention_secs(600);
        backend.create_session_storage("gc").await.unwrap();
        backend.store_trace_anchor("gc", b"a").await.unwrap();
        let expected = 600 + 86_400 + 3_600;
        let ttl = ttl_of(&backend, "gc").await;
        assert!(ttl > expected - 60 && ttl <= expected, "ttl {ttl}");
        cleanup(&backend).await;
    }
}
