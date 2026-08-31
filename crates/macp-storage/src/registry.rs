use macp_core::session::Session;
use std::collections::{BinaryHeap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PersistedRoot {
    pub uri: String,
    pub name: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct PersistedSession {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub session_id: String,
    pub state: macp_core::session::SessionState,
    pub ttl_expiry: i64,
    #[serde(default)]
    pub ttl_ms: i64,
    pub started_at_unix_ms: i64,
    pub resolution: Option<Vec<u8>>,
    pub mode: String,
    pub mode_state: Vec<u8>,
    pub participants: Vec<String>,
    pub seen_message_ids: Vec<String>,
    pub intent: String,
    pub mode_version: String,
    pub configuration_version: String,
    pub policy_version: String,
    #[serde(default)]
    pub context_id: String,
    #[serde(default)]
    pub extensions: HashMap<String, Vec<u8>>,
    pub roots: Vec<PersistedRoot>,
    pub initiator_sender: String,
    #[serde(default)]
    pub policy_definition: Option<macp_core::policy::PolicyDefinition>,
    #[serde(default)]
    pub suspended_at_ms: Option<i64>,
    #[serde(default)]
    pub accumulated_suspended_ms: i64,
    /// Session-semantics revision (see `macp_core::session::CURRENT_SEMANTICS_REV`).
    /// Legacy snapshots deserialize as 0 and keep legacy behavior.
    #[serde(default)]
    pub semantics_rev: u32,
    /// Suspension cap bound at SessionStart. Legacy snapshots deserialize as
    /// 0 (= default-cap semantics via `Session::effective_max_suspend_ms`).
    #[serde(default)]
    pub max_suspend_ms: i64,
}

fn default_schema_version() -> u32 {
    2
}

impl From<&Session> for PersistedSession {
    fn from(session: &Session) -> Self {
        Self {
            schema_version: 2,
            session_id: session.session_id.clone(),
            state: session.state.clone(),
            ttl_expiry: session.ttl_expiry,
            ttl_ms: session.ttl_ms,
            started_at_unix_ms: session.started_at_unix_ms,
            resolution: session.resolution.clone(),
            mode: session.mode.clone(),
            mode_state: session.mode_state.clone(),
            participants: session.participants.clone(),
            seen_message_ids: session.seen_message_ids.iter().cloned().collect(),
            intent: session.intent.clone(),
            mode_version: session.mode_version.clone(),
            configuration_version: session.configuration_version.clone(),
            policy_version: session.policy_version.clone(),
            context_id: session.context_id.clone(),
            extensions: session.extensions.clone(),
            roots: session
                .roots
                .iter()
                .map(|root| PersistedRoot {
                    uri: root.uri.clone(),
                    name: root.name.clone(),
                })
                .collect(),
            initiator_sender: session.initiator_sender.clone(),
            policy_definition: session.policy_definition.clone(),
            suspended_at_ms: session.suspended_at_ms,
            accumulated_suspended_ms: session.accumulated_suspended_ms,
            semantics_rev: session.semantics_rev,
            max_suspend_ms: session.max_suspend_ms,
        }
    }
}

impl From<PersistedSession> for Session {
    fn from(session: PersistedSession) -> Self {
        let ttl_ms = if session.ttl_ms > 0 {
            session.ttl_ms
        } else {
            // Backward compatibility: compute from absolute timestamps
            session
                .ttl_expiry
                .saturating_sub(session.started_at_unix_ms)
        };
        Session::builder(session.session_id, session.mode, session.initiator_sender)
            .state(session.state)
            .ttl_expiry(session.ttl_expiry)
            .ttl_ms(ttl_ms)
            .started_at_unix_ms(session.started_at_unix_ms)
            .resolution(session.resolution)
            .mode_state(session.mode_state)
            .participants(session.participants)
            .seen_message_ids(session.seen_message_ids.into_iter().collect())
            .intent(session.intent)
            .mode_version(session.mode_version)
            .configuration_version(session.configuration_version)
            .policy_version(session.policy_version)
            .context_id(session.context_id)
            .extensions(session.extensions)
            .roots(
                session
                    .roots
                    .into_iter()
                    .map(|root| macp_pb::pb::Root {
                        uri: root.uri,
                        name: root.name,
                    })
                    .collect(),
            )
            .policy_definition(session.policy_definition)
            .suspended_at_ms(session.suspended_at_ms)
            .accumulated_suspended_ms(session.accumulated_suspended_ms)
            .semantics_rev(session.semantics_rev)
            .max_suspend_ms(session.max_suspend_ms)
            .build()
    }
}

/// A registered session behind its own async mutex. The registry map lock is
/// held only for lookup/insert/remove; the per-session mutex serializes all
/// processing (validate + storage append + commit) for that session ONLY —
/// RFC-MACP-0001 §8.1 requires acceptance serialization within a session,
/// never across sessions. Lock ordering: map lock BEFORE session mutex, and
/// never hold the map lock while awaiting a session mutex — snapshot the
/// `Arc`s, drop the map guard, then lock.
pub type SharedSession = Arc<tokio::sync::Mutex<Session>>;

pub struct SessionRegistry {
    pub sessions: RwLock<HashMap<String, SharedSession>>,
    persistence_path: Option<PathBuf>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            persistence_path: None,
        }
    }

    pub fn with_persistence<P: AsRef<Path>>(dir: P) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let path = dir.join("sessions.json");
        let sessions = Self::load_sessions(&path)?;
        Ok(Self {
            sessions: RwLock::new(sessions),
            persistence_path: Some(path),
        })
    }

    fn load_sessions(path: &Path) -> std::io::Result<HashMap<String, SharedSession>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let bytes = fs::read(path)?;
        let persisted: HashMap<String, PersistedSession> = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: failed to deserialize sessions from {}: {e}; starting with empty state", path.display());
                HashMap::new()
            }
        };
        Ok(persisted
            .into_iter()
            .map(|(id, mut record)| {
                // The map key and the record's `session_id` are written as one
                // value, but a corrupt or hand-edited sessions.json can disagree.
                // That is the reachable form of the invariant asserted in
                // `insert_recovered_session`: paging orders by the key while it
                // emits the field, so a mismatch silently misorders ListSessions.
                // Repair rather than skip or abort — this runs at startup and
                // recovery must stay available (same lenient posture as the
                // deserialization fallback above). The key wins, since it is what
                // paging orders by and what `get_session` looks up.
                if record.session_id != id {
                    tracing::warn!(
                        map_key = %id,
                        record_session_id = %record.session_id,
                        path = %path.display(),
                        "persisted session key disagrees with its session_id; \
                         repairing to the map key"
                    );
                    record.session_id.clone_from(&id);
                }
                let session: Session = record.into();
                (id, Arc::new(tokio::sync::Mutex::new(session)))
            })
            .collect())
    }

    fn persist_map(
        path: &Path,
        sessions: &HashMap<String, PersistedSession>,
    ) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(sessions)?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, bytes)?;
        fs::rename(&tmp_path, path)
    }

    /// Snapshot every session (locking each briefly) and persist. Never holds
    /// the map lock across the per-session locks or the fs write.
    pub async fn persist_snapshot(&self) -> std::io::Result<()> {
        let Some(path) = self.persistence_path.clone() else {
            return Ok(());
        };
        let arcs: Vec<(String, SharedSession)> = {
            let guard = self.sessions.read().await;
            guard
                .iter()
                .map(|(id, arc)| (id.clone(), Arc::clone(arc)))
                .collect()
        };
        let mut persisted = HashMap::with_capacity(arcs.len());
        for (id, arc) in arcs {
            let session = arc.lock().await;
            persisted.insert(id, PersistedSession::from(&*session));
        }
        Self::persist_map(&path, &persisted)
    }

    /// Clone the shared handle for a session (brief map read; no session lock).
    pub async fn get_shared(&self, session_id: &str) -> Option<SharedSession> {
        let guard = self.sessions.read().await;
        guard.get(session_id).cloned()
    }

    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        let arc = self.get_shared(session_id).await?;
        let session = arc.lock().await;
        Some(session.clone())
    }

    pub async fn get_all_sessions(&self) -> Vec<Session> {
        let arcs: Vec<SharedSession> = {
            let guard = self.sessions.read().await;
            guard.values().cloned().collect()
        };
        let mut out = Vec::with_capacity(arcs.len());
        for arc in arcs {
            out.push(arc.lock().await.clone());
        }
        out
    }

    /// Session IDs strictly greater than `after`, ascending (byte order), at most
    /// `limit`. Keyset cursor primitive for ListSessions paging (see plan D1/D2).
    ///
    /// Holds only the map read lock, for one synchronous pass — no session mutex is
    /// taken and no `.await` happens under the guard, per the lock-ordering contract
    /// documented above (map lock BEFORE session mutex; never hold the map lock
    /// across an await).
    ///
    /// Each call is individually consistent, but a multi-page traversal is **not** a
    /// snapshot: the lock is released between pages, so concurrent mutation is
    /// visible mid-traversal. A session inserted at a key at or below the cursor is
    /// missed by the remainder of the traversal; one inserted above the cursor
    /// appears in a later page; one removed above the cursor is never emitted.
    /// Already-emitted IDs are stable — the cursor only moves forward — so no ID is
    /// ever returned twice. This is inherent to keyset paging; callers must not
    /// present a completed traversal as a point-in-time view of the registry.
    ///
    /// `limit` is caller-supplied and may be arbitrarily large (`usize::MAX` reads
    /// as "no limit"); allocation is bounded by the map, never by the limit.
    pub async fn session_ids_after(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        if limit == 0 {
            return Vec::new();
        }
        // The read guard must stay live through the clone below — the heap holds
        // borrows into the map — so this block clones the survivors before the
        // guard drops.
        {
            let guard = self.sessions.read().await;
            // Max-heap holding at most `limit` keys: keep the `limit` smallest
            // surviving keys in one pass, popping the current maximum whenever the
            // heap overflows. O(n log k) with exactly k clones, versus cloning and
            // sorting every key per page. The heap's *length* is capped by the
            // push/pop below; the pre-allocation is capped by the map size so a
            // huge `limit` can neither overflow nor over-allocate.
            let capacity = limit.saturating_add(1).min(guard.len().saturating_add(1));
            let mut heap: BinaryHeap<&String> = BinaryHeap::with_capacity(capacity);
            for key in guard.keys() {
                if after.is_none_or(|a| key.as_str() > a) {
                    heap.push(key);
                    if heap.len() > limit {
                        heap.pop();
                    }
                }
            }
            heap.into_sorted_vec().into_iter().cloned().collect()
        }
    }

    pub async fn insert_recovered_session(&self, session_id: String, session: Session) {
        // Documents the contract at this API boundary; it cannot fire for the
        // in-tree caller (`src/main.rs`), which passes one value twice, but
        // `sessions` is `pub`, so an external consumer can construct a mismatched
        // pair. `load_sessions`'s repair logic guards the legacy `sessions.json`
        // path used by external consumers of `with_persistence`, not this
        // runtime's own startup recovery — `src/main.rs` never calls
        // `with_persistence`. The in-tree recovery path is structurally safe
        // instead: `replay::replay_session()` forces the Session's id to the
        // directory-derived id passed in here, both on the checkpoint path
        // (`src/replay.rs:55`) and the full-replay path (`src/replay.rs:279`),
        // so a mismatch cannot arise there.
        debug_assert_eq!(
            session.session_id, session_id,
            "registry map key must equal Session::session_id — ListSessions paging \
             orders by the key but emits the field (plan D1)"
        );
        {
            let mut guard = self.sessions.write().await;
            guard.insert(session_id, Arc::new(tokio::sync::Mutex::new(session)));
        }
        let _ = self.persist_snapshot().await;
    }

    pub async fn count_open_sessions_for_initiator(&self, sender: &str) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let arcs: Vec<SharedSession> = {
            let guard = self.sessions.read().await;
            guard.values().cloned().collect()
        };
        let mut count = 0;
        for arc in arcs {
            // A session currently being processed is Open by definition —
            // count it (conservative for a rate limit) rather than await.
            let counts = match arc.try_lock() {
                Ok(session) => {
                    session.initiator_sender == sender
                        && session.state == macp_core::session::SessionState::Open
                        && now <= session.ttl_expiry
                }
                Err(_) => true,
            };
            if counts {
                count += 1;
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::session::{Session, SessionState};
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_session(id: &str) -> Session {
        Session::builder(id, "macp.mode.decision.v1", "alice")
            .ttl_expiry(10)
            .ttl_ms(9)
            .started_at_unix_ms(1)
            .mode_state(vec![1, 2, 3])
            .participants(vec!["alice".into()])
            .seen_message_ids(HashSet::from(["m1".into()]))
            .intent("intent")
            .mode_version("1.0.0")
            .configuration_version("cfg")
            .policy_version("pol")
            .context_id("test-ctx")
            .roots(vec![macp_pb::pb::Root {
                uri: "root://1".into(),
                name: "r1".into(),
            }])
            .build()
    }

    /// Register `ids` (each session's `session_id` equal to its map key, per the
    /// `insert_recovered_session` invariant).
    async fn registry_with(ids: &[String]) -> SessionRegistry {
        let registry = SessionRegistry::new();
        for id in ids {
            registry
                .insert_recovered_session(id.clone(), sample_session(id))
                .await;
        }
        registry
    }

    /// Obvious reference implementation: sort every key, drop everything at or
    /// below the cursor, take `limit`.
    fn sort_then_truncate_reference(
        ids: &[String],
        after: Option<&str>,
        limit: usize,
    ) -> Vec<String> {
        let mut sorted: Vec<String> = ids.to_vec();
        sorted.sort();
        sorted
            .into_iter()
            .filter(|id| after.is_none_or(|a| id.as_str() > a))
            .take(limit)
            .collect()
    }

    /// Deterministic pseudorandom IDs from a plain LCG (numerical-recipes
    /// constants) — reproducible across runs and platforms, and no `rand`
    /// dependency. Hex-formatted so byte order and the values are unrelated.
    fn deterministic_ids(count: usize) -> Vec<String> {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut ids = Vec::with_capacity(count);
        for i in 0..count {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            // `i` guarantees uniqueness even if the LCG were to repeat a value.
            ids.push(format!("sess-{:016x}-{i:04}", state >> 16));
        }
        ids
    }

    #[tokio::test]
    async fn session_ids_after_returns_ascending_ids() {
        let ids: Vec<String> = ["delta", "alpha", "charlie", "bravo"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let registry = registry_with(&ids).await;

        let page = registry.session_ids_after(None, 10).await;
        assert_eq!(page, vec!["alpha", "bravo", "charlie", "delta"]);

        // The k byte-wise-smallest, ascending.
        let page = registry.session_ids_after(None, 2).await;
        assert_eq!(page, vec!["alpha", "bravo"]);
    }

    #[tokio::test]
    async fn session_ids_after_respects_limit() {
        let ids: Vec<String> = (0..10).map(|i| format!("s{i:02}")).collect();
        let registry = registry_with(&ids).await;

        assert_eq!(registry.session_ids_after(None, 1).await, vec!["s00"]);
        // Contents, not just the count: a count check would also pass if the
        // method returned the three *largest* IDs.
        assert_eq!(
            registry.session_ids_after(None, 3).await,
            vec!["s00", "s01", "s02"]
        );
        // A limit larger than the map yields the whole map, not padding.
        assert_eq!(registry.session_ids_after(None, 100).await.len(), 10);
    }

    #[tokio::test]
    async fn session_ids_after_is_exclusive_of_cursor() {
        let ids: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let registry = registry_with(&ids).await;

        let page = registry.session_ids_after(Some("b"), 10).await;
        assert_eq!(page, vec!["c", "d"]);
        assert!(!page.contains(&"b".to_string()));
        assert!(page.iter().all(|id| id.as_str() > "b"));

        // Cursor equal to the largest key: nothing follows it.
        assert!(registry.session_ids_after(Some("d"), 10).await.is_empty());
        // Cursor greater than every key.
        assert!(registry.session_ids_after(Some("zzz"), 10).await.is_empty());
    }

    #[tokio::test]
    async fn session_ids_after_tolerates_absent_cursor() {
        let ids: Vec<String> = ["a", "c", "e"].iter().map(|s| s.to_string()).collect();
        let registry = registry_with(&ids).await;

        // "b" was never registered (or was deleted mid-traversal); paging must
        // resume from its position regardless.
        assert_eq!(
            registry.session_ids_after(Some("b"), 10).await,
            vec!["c", "e"]
        );
        // Identical to the result from a cursor that *is* present.
        assert_eq!(
            registry.session_ids_after(Some("b"), 10).await,
            registry.session_ids_after(Some("a"), 10).await
        );
        // The empty cursor is strictly less than every non-empty key, so here —
        // where no key is empty — it selects the whole map. It is not a universal
        // "before everything" sentinel: an empty key would be excluded, since the
        // comparison is strict.
        assert_eq!(
            registry.session_ids_after(Some(""), 10).await,
            vec!["a", "c", "e"]
        );
    }

    #[tokio::test]
    async fn session_ids_after_zero_limit_is_empty() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let registry = registry_with(&ids).await;

        assert!(registry.session_ids_after(None, 0).await.is_empty());
        assert!(registry.session_ids_after(Some("a"), 0).await.is_empty());

        // Empty registry, any limit.
        let empty = SessionRegistry::new();
        assert!(empty.session_ids_after(None, 0).await.is_empty());
        assert!(empty.session_ids_after(None, 10).await.is_empty());
        assert!(empty.session_ids_after(Some("a"), 10).await.is_empty());
    }

    /// A caller-supplied page size is untrusted: `usize::MAX` is the natural
    /// "no limit" sentinel, and any huge value must neither panic (debug
    /// overflow on `limit + 1`) nor pre-allocate proportionally to the limit
    /// rather than to the map.
    #[tokio::test]
    async fn session_ids_after_handles_huge_limits() {
        let ids: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let registry = registry_with(&ids).await;

        for limit in [usize::MAX, usize::MAX - 1, 10_000_000, 1 << 40] {
            assert_eq!(
                registry.session_ids_after(None, limit).await,
                vec!["a", "b", "c"],
                "limit={limit}"
            );
            assert_eq!(
                registry.session_ids_after(Some("a"), limit).await,
                vec!["b", "c"],
                "limit={limit}"
            );
        }

        // Empty registry, no-limit sentinel.
        let empty = SessionRegistry::new();
        assert!(empty.session_ids_after(None, usize::MAX).await.is_empty());
    }

    #[tokio::test]
    async fn session_ids_after_matches_sort_then_truncate_reference() {
        let ids = deterministic_ids(200);
        let registry = registry_with(&ids).await;

        let mut sorted = ids.clone();
        sorted.sort();

        let cursors: Vec<Option<String>> = std::iter::once(None)
            .chain(std::iter::once(Some(String::new())))
            .chain(std::iter::once(Some("sess-".to_string())))
            .chain(std::iter::once(Some("zzzz".to_string())))
            // Present cursors spread across the sorted key space...
            .chain(sorted.iter().step_by(17).cloned().map(Some))
            .chain(std::iter::once(Some(sorted.last().unwrap().clone())))
            // ...and absent ones derived from real keys by suffixing.
            .chain(sorted.iter().step_by(23).map(|k| Some(format!("{k}~"))))
            .collect();

        for cursor in &cursors {
            for limit in [1usize, 2, 7, 50, 199, 200, 201, 1000] {
                let got = registry.session_ids_after(cursor.as_deref(), limit).await;
                let want = sort_then_truncate_reference(&ids, cursor.as_deref(), limit);
                assert_eq!(got, want, "cursor={cursor:?} limit={limit}");
            }
        }
    }

    #[tokio::test]
    async fn session_ids_after_full_traversal_covers_every_id_once() {
        let ids = deterministic_ids(200);
        let registry = registry_with(&ids).await;

        for page_size in [1usize, 3, 7, 64, 199, 200, 500] {
            let mut collected: Vec<String> = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = registry
                    .session_ids_after(cursor.as_deref(), page_size)
                    .await;
                let short = page.len() < page_size;
                // The limit is honored per page — without this an unbounded
                // implementation returning one giant page would still satisfy
                // coverage and no-duplicates below.
                assert!(
                    page.len() <= page_size,
                    "page_size={page_size}: page of {} exceeds the limit",
                    page.len()
                );
                // Pages are ascending and strictly increase across the traversal.
                if let (Some(last), Some(first)) = (collected.last(), page.first()) {
                    assert!(first > last, "page_size={page_size}: page did not advance");
                }
                collected.extend(page.iter().cloned());
                cursor = page.last().cloned();
                if short {
                    break;
                }
            }

            let unique: HashSet<&String> = collected.iter().collect();
            // Count equal to set size rules out duplicates, which a set alone hides.
            assert_eq!(
                collected.len(),
                unique.len(),
                "page_size={page_size}: duplicate IDs across pages"
            );
            let expected: HashSet<&String> = ids.iter().collect();
            assert_eq!(unique, expected, "page_size={page_size}: coverage mismatch");
            assert_eq!(collected.len(), ids.len(), "page_size={page_size}");
        }
    }

    #[tokio::test]
    async fn expired_sessions_not_counted_against_limit() {
        let registry = SessionRegistry::new();
        let now = chrono::Utc::now().timestamp_millis();
        // Insert a session with TTL already expired
        let mut expired = sample_session("expired-s1");
        expired.initiator_sender = "agent://alice".into();
        expired.ttl_expiry = now - 1000; // expired 1 second ago
        expired.state = SessionState::Open; // still Open but TTL is past
        registry
            .insert_recovered_session("expired-s1".into(), expired)
            .await;

        // Should not count the expired-but-open session
        let count = registry
            .count_open_sessions_for_initiator("agent://alice")
            .await;
        assert_eq!(count, 0);

        // Insert a session that is still valid
        let mut active = sample_session("active-s1");
        active.initiator_sender = "agent://alice".into();
        active.ttl_expiry = now + 60_000; // expires in 60s
        active.state = SessionState::Open;
        registry
            .insert_recovered_session("active-s1".into(), active)
            .await;

        let count = registry
            .count_open_sessions_for_initiator("agent://alice")
            .await;
        assert_eq!(count, 1);
    }

    /// A corrupt or hand-edited `sessions.json` can pair map key "A" with a record
    /// whose `session_id` is "B". Paging orders by the key but emits the field, so
    /// loading that unrepaired would make ListSessions order by one ID and return
    /// another. `load_sessions` must repair to the key rather than skip or abort —
    /// this runs at startup, so recovery has to stay available.
    #[tokio::test]
    async fn load_sessions_repairs_key_field_mismatch() {
        let base = std::env::temp_dir().join(format!(
            "macp-registry-mismatch-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();

        let mut persisted = HashMap::new();
        persisted.insert(
            "A".to_string(),
            PersistedSession::from(&sample_session("B")),
        );
        SessionRegistry::persist_map(&base.join("sessions.json"), &persisted).unwrap();

        let reopened = SessionRegistry::with_persistence(&base).unwrap();

        // The key wins: the session is keyed at, and reports, "A".
        let session = reopened.get_session("A").await.unwrap();
        assert_eq!(session.session_id, "A");
        // The stale field value is not a lookup key.
        assert!(reopened.get_session("B").await.is_none());
        // The invariant this guard exists to protect: the paged listing orders by
        // the key and emits the same value it ordered by.
        assert_eq!(reopened.session_ids_after(None, 10).await, vec!["A"]);
    }

    #[tokio::test]
    async fn persistent_registry_round_trip() {
        let base = std::env::temp_dir().join(format!(
            "macp-registry-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let registry = SessionRegistry::with_persistence(&base).unwrap();
        registry
            .insert_recovered_session("s1".into(), sample_session("s1"))
            .await;

        let reopened = SessionRegistry::with_persistence(&base).unwrap();
        let session = reopened.get_session("s1").await.unwrap();
        assert_eq!(session.mode, "macp.mode.decision.v1");
        assert_eq!(session.mode_version, "1.0.0");
        assert!(session.seen_message_ids.contains("m1"));
    }
}
