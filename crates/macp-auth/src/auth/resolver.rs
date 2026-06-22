use crate::security::AuthIdentity;
use std::collections::HashSet;
use tonic::metadata::MetadataMap;

#[derive(Debug)]
pub enum AuthError {
    InvalidCredential(String),
    Expired,
    MissingClaim(String),
    FetchFailed(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::InvalidCredential(msg) => write!(f, "invalid credential: {msg}"),
            AuthError::Expired => write!(f, "credential expired"),
            AuthError::MissingClaim(claim) => write!(f, "missing required claim: {claim}"),
            AuthError::FetchFailed(msg) => write!(f, "key fetch failed: {msg}"),
        }
    }
}

impl std::error::Error for AuthError {}

#[derive(Clone, Debug)]
pub struct ResolvedIdentity {
    pub sender: String,
    pub allowed_modes: Option<HashSet<String>>,
    pub can_start_sessions: bool,
    pub max_open_sessions: Option<usize>,
    pub can_manage_mode_registry: bool,
    pub is_observer: bool,
    pub resolver: String,
}

impl From<ResolvedIdentity> for AuthIdentity {
    fn from(resolved: ResolvedIdentity) -> Self {
        AuthIdentity {
            sender: resolved.sender,
            allowed_modes: resolved.allowed_modes,
            can_start_sessions: resolved.can_start_sessions,
            max_open_sessions: resolved.max_open_sessions,
            can_manage_mode_registry: resolved.can_manage_mode_registry,
            is_observer: resolved.is_observer,
        }
    }
}

/// Trait for pluggable auth resolvers.
///
/// Each resolver examines gRPC metadata and returns:
/// - `Ok(Some(identity))` — positive verification, chain stops
/// - `Ok(None)` — not my credential type, chain continues
/// - `Err(e)` — credential is mine but invalid, chain stops with error
#[async_trait::async_trait]
pub trait AuthResolver: Send + Sync {
    fn name(&self) -> &str;

    async fn resolve(&self, metadata: &MetadataMap) -> Result<Option<ResolvedIdentity>, AuthError>;
}
