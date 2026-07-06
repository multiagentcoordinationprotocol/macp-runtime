use crate::log_store::LogEntry;
use crate::registry::PersistedSession;
use macp_core::session::Session;
use redis::AsyncCommands;
use std::io;

use super::StorageBackend;

pub struct RedisBackend {
    conn: redis::aio::MultiplexedConnection,
    prefix: String,
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
        })
    }

    fn session_key(&self, session_id: &str) -> String {
        format!("{}:session:{}", self.prefix, session_id)
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
            .srem(self.index_key(), session_id)
            .exec_async(&mut conn)
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
        RedisBackend::connect(&url, &prefix).await.ok()
    }

    async fn cleanup(backend: &RedisBackend) {
        // Clean up all test keys
        let mut conn = backend.conn.clone();
        let ids: Vec<String> = conn.smembers(backend.index_key()).await.unwrap_or_default();
        for id in &ids {
            let _ = redis::pipe()
                .del(backend.session_key(id))
                .del(backend.log_key(id))
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
}
