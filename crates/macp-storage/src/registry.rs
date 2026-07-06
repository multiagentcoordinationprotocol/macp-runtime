use macp_core::session::Session;
use std::collections::HashMap;
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
            .map(|(id, session)| (id, Arc::new(tokio::sync::Mutex::new(session.into()))))
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

    pub async fn insert_recovered_session(&self, session_id: String, session: Session) {
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
