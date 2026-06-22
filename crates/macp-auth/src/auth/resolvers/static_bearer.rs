use crate::auth::resolver::{AuthError, AuthResolver, ResolvedIdentity};
use crate::security::AuthIdentity;
use std::collections::HashMap;
use tonic::metadata::MetadataMap;

/// Resolves opaque bearer tokens against a pre-loaded identity map.
/// This is the existing `MACP_AUTH_TOKENS_JSON` mechanism.
pub struct StaticBearerResolver {
    identities: HashMap<String, AuthIdentity>,
}

impl StaticBearerResolver {
    pub fn new(identities: HashMap<String, AuthIdentity>) -> Self {
        tracing::info!(
            count = identities.len(),
            "static bearer resolver initialized"
        );
        Self { identities }
    }

    fn extract_bearer(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string)
            .or_else(|| {
                metadata
                    .get("x-macp-token")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
            })
    }
}

#[async_trait::async_trait]
impl AuthResolver for StaticBearerResolver {
    fn name(&self) -> &str {
        "static_bearer"
    }

    async fn resolve(&self, metadata: &MetadataMap) -> Result<Option<ResolvedIdentity>, AuthError> {
        let token = match Self::extract_bearer(metadata) {
            Some(t) => t,
            None => return Ok(None),
        };

        // JWT-shaped tokens (contain dots) are not ours — defer to JWT resolver
        if token.contains('.') {
            return Ok(None);
        }

        match self.identities.get(&token) {
            Some(identity) => Ok(Some(ResolvedIdentity {
                sender: identity.sender.clone(),
                allowed_modes: identity.allowed_modes.clone(),
                can_start_sessions: identity.can_start_sessions,
                max_open_sessions: identity.max_open_sessions,
                can_manage_mode_registry: identity.can_manage_mode_registry,
                is_observer: identity.is_observer,
                resolver: "static_bearer".to_string(),
            })),
            None => Err(AuthError::InvalidCredential(
                "token not found in identity map".to_string(),
            )),
        }
    }
}
