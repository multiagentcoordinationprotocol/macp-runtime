use macp_core::session::Session;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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
        Self {
            session_id: session.session_id,
            state: session.state,
            ttl_expiry: session.ttl_expiry,
            ttl_ms,
            started_at_unix_ms: session.started_at_unix_ms,
            resolution: session.resolution,
            mode: session.mode,
            mode_state: session.mode_state,
            participants: session.participants,
            seen_message_ids: session.seen_message_ids.into_iter().collect(),
            intent: session.intent,
            mode_version: session.mode_version,
            configuration_version: session.configuration_version,
            policy_version: session.policy_version,
            context_id: session.context_id,
            extensions: session.extensions,
            roots: session
                .roots
                .into_iter()
                .map(|root| macp_pb::pb::Root {
                    uri: root.uri,
                    name: root.name,
                })
                .collect(),
            initiator_sender: session.initiator_sender,
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: session.policy_definition,
        }
    }
}

pub struct SessionRegistry {
    pub sessions: RwLock<HashMap<String, Session>>,
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

    fn load_sessions(path: &Path) -> std::io::Result<HashMap<String, Session>> {
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
            .map(|(id, session)| (id, session.into()))
            .collect())
    }

    fn persist_map(path: &Path, sessions: &HashMap<String, Session>) -> std::io::Result<()> {
        let persisted: HashMap<String, PersistedSession> = sessions
            .iter()
            .map(|(id, session)| (id.clone(), PersistedSession::from(session)))
            .collect();
        let bytes = serde_json::to_vec_pretty(&persisted)?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, bytes)?;
        fs::rename(&tmp_path, path)
    }

    pub(crate) async fn persist_locked(
        &self,
        sessions: &HashMap<String, Session>,
    ) -> std::io::Result<()> {
        if let Some(path) = &self.persistence_path {
            Self::persist_map(path, sessions)?;
        }
        Ok(())
    }

    pub async fn persist_snapshot(&self) -> std::io::Result<()> {
        let guard = self.sessions.read().await;
        self.persist_locked(&guard).await
    }

    pub async fn get_session(&self, session_id: &str) -> Option<Session> {
        let guard = self.sessions.read().await;
        guard.get(session_id).cloned()
    }

    pub async fn get_all_sessions(&self) -> Vec<Session> {
        let guard = self.sessions.read().await;
        guard.values().cloned().collect()
    }

    pub async fn insert_recovered_session(&self, session_id: String, session: Session) {
        let mut guard = self.sessions.write().await;
        guard.insert(session_id, session);
        let _ = self.persist_locked(&guard).await;
    }

    pub async fn count_open_sessions_for_initiator(&self, sender: &str) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let guard = self.sessions.read().await;
        guard
            .values()
            .filter(|session| {
                session.initiator_sender == sender
                    && session.state == macp_core::session::SessionState::Open
                    && now <= session.ttl_expiry
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::session::{Session, SessionState};
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_session(id: &str) -> Session {
        Session {
            session_id: id.into(),
            state: SessionState::Open,
            ttl_expiry: 10,
            ttl_ms: 9,
            started_at_unix_ms: 1,
            resolution: None,
            mode: "macp.mode.decision.v1".into(),
            mode_state: vec![1, 2, 3],
            participants: vec!["alice".into()],
            seen_message_ids: HashSet::from(["m1".into()]),
            intent: "intent".into(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg".into(),
            policy_version: "pol".into(),
            context_id: "test-ctx".to_string(),
            extensions: std::collections::HashMap::new(),
            roots: vec![macp_pb::pb::Root {
                uri: "root://1".into(),
                name: "r1".into(),
            }],
            initiator_sender: "alice".into(),
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: None,
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
