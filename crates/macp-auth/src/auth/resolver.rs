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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_error_display_covers_all_variants() {
        assert_eq!(
            AuthError::InvalidCredential("bad token".to_string()).to_string(),
            "invalid credential: bad token"
        );
        assert_eq!(AuthError::Expired.to_string(), "credential expired");
        assert_eq!(
            AuthError::MissingClaim("sub".to_string()).to_string(),
            "missing required claim: sub"
        );
        assert_eq!(
            AuthError::FetchFailed("connection refused".to_string()).to_string(),
            "key fetch failed: connection refused"
        );
    }

    #[test]
    fn resolved_identity_converts_to_auth_identity_preserving_fields() {
        let modes: HashSet<String> = ["macp.mode.decision.v1".to_string()].into_iter().collect();
        let resolved = ResolvedIdentity {
            sender: "agent://alice".to_string(),
            allowed_modes: Some(modes.clone()),
            can_start_sessions: false,
            max_open_sessions: Some(3),
            can_manage_mode_registry: true,
            is_observer: true,
            resolver: "jwt_bearer".to_string(),
        };

        let identity: AuthIdentity = resolved.into();
        assert_eq!(identity.sender, "agent://alice");
        assert_eq!(identity.allowed_modes, Some(modes));
        assert!(!identity.can_start_sessions);
        assert_eq!(identity.max_open_sessions, Some(3));
        assert!(identity.can_manage_mode_registry);
        assert!(identity.is_observer);
    }
}
