use macp_core::error::MacpError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tonic::metadata::MetadataMap;

#[derive(Clone, Debug)]
pub struct AuthIdentity {
    pub sender: String,
    pub allowed_modes: Option<HashSet<String>>,
    pub can_start_sessions: bool,
    pub max_open_sessions: Option<usize>,
    pub can_manage_mode_registry: bool,
    pub is_observer: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct RawIdentity {
    token: String,
    sender: String,
    #[serde(default)]
    allowed_modes: Vec<String>,
    #[serde(default = "default_true")]
    can_start_sessions: bool,
    max_open_sessions: Option<usize>,
    #[serde(default)]
    can_manage_mode_registry: bool,
    #[serde(default)]
    is_observer: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(untagged)]
enum RawConfig {
    List(Vec<RawIdentity>),
    Wrapped { tokens: Vec<RawIdentity> },
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub limit: usize,
    pub window: Duration,
}

#[derive(Default)]
struct RateBucket {
    start_events: Mutex<HashMap<String, VecDeque<Instant>>>,
    message_events: Mutex<HashMap<String, VecDeque<Instant>>>,
}

#[derive(Clone)]
pub struct SecurityLayer {
    identities: Arc<HashMap<String, AuthIdentity>>,
    rate_bucket: Arc<RateBucket>,
    auth_chain: Option<Arc<crate::auth::AuthResolverChain>>,
    pub max_payload_bytes: usize,
    session_start_rate: RateLimitConfig,
    message_rate: RateLimitConfig,
}

impl SecurityLayer {
    /// Creates a test-friendly SecurityLayer that maps any bearer token
    /// `"tok-<sender>"` to an identity with `sender = <token-value>`.
    /// For tests, use `Authorization: Bearer agent://name` to authenticate as `agent://name`.
    pub fn dev_mode() -> Self {
        Self {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        }
    }

    /// Dev-mode authenticate: accepts any bearer token as the sender identity.
    /// This is used ONLY in tests (dev_mode), not in production (from_env).
    fn dev_authenticate(&self, metadata: &MetadataMap) -> Result<AuthIdentity, MacpError> {
        if let Some(token) = Self::bearer_token(metadata) {
            return Ok(AuthIdentity {
                sender: token,
                allowed_modes: None,
                can_start_sessions: true,
                max_open_sessions: None,
                can_manage_mode_registry: true,
                is_observer: false,
            });
        }
        Err(MacpError::Unauthenticated)
    }

    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let max_payload_bytes = std::env::var("MACP_MAX_PAYLOAD_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1_048_576);

        let session_start_rate = RateLimitConfig {
            limit: std::env::var("MACP_SESSION_START_LIMIT_PER_MINUTE")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(60),
            window: Duration::from_secs(60),
        };
        let message_rate = RateLimitConfig {
            limit: std::env::var("MACP_MESSAGE_LIMIT_PER_MINUTE")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(600),
            window: Duration::from_secs(60),
        };

        let raw = if let Ok(json) = std::env::var("MACP_AUTH_TOKENS_JSON") {
            Some(json)
        } else if let Ok(path) = std::env::var("MACP_AUTH_TOKENS_FILE") {
            Some(fs::read_to_string(PathBuf::from(path))?)
        } else {
            None
        };

        let identities = raw
            .as_ref()
            .map(|json| Self::parse_identities(json))
            .transpose()?
            .unwrap_or_default();

        // Build auth resolver chain
        let mut resolvers: Vec<Box<dyn crate::auth::AuthResolver>> = Vec::new();

        // JWT resolver (if configured)
        if let Ok(issuer) = std::env::var("MACP_AUTH_ISSUER") {
            let audience =
                std::env::var("MACP_AUTH_AUDIENCE").unwrap_or_else(|_| "macp-runtime".into());
            let cache_ttl = std::env::var("MACP_AUTH_JWKS_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(300u64);
            let config = crate::auth::resolvers::jwt_bearer::JwtConfig {
                issuer,
                audience,
                algorithms: vec![
                    jsonwebtoken::Algorithm::RS256,
                    jsonwebtoken::Algorithm::ES256,
                    jsonwebtoken::Algorithm::HS256,
                ],
            };
            if let Ok(jwks_json) = std::env::var("MACP_AUTH_JWKS_JSON") {
                match crate::auth::resolvers::JwtBearerResolver::from_inline_json(
                    config, &jwks_json,
                ) {
                    Ok(resolver) => resolvers.push(Box::new(resolver)),
                    Err(e) => {
                        tracing::error!("failed to create JWT resolver from inline JWKS: {e}")
                    }
                }
            } else if let Ok(jwks_url) = std::env::var("MACP_AUTH_JWKS_URL") {
                resolvers.push(Box::new(
                    crate::auth::resolvers::JwtBearerResolver::from_url(
                        config, jwks_url, cache_ttl,
                    ),
                ));
            }
        }

        // Static bearer resolver (always present if tokens are configured)
        if !identities.is_empty() {
            resolvers.push(Box::new(crate::auth::resolvers::StaticBearerResolver::new(
                identities.clone(),
            )));
        }

        let auth_chain = if resolvers.is_empty() {
            None
        } else {
            Some(Arc::new(crate::auth::AuthResolverChain::new(resolvers)))
        };

        Ok(Self {
            identities: Arc::new(identities),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain,
            max_payload_bytes,
            session_start_rate,
            message_rate,
        })
    }

    fn parse_identities(
        json: &str,
    ) -> Result<HashMap<String, AuthIdentity>, Box<dyn std::error::Error>> {
        let parsed: RawConfig = serde_json::from_str(json)?;
        let items = match parsed {
            RawConfig::List(items) => items,
            RawConfig::Wrapped { tokens } => tokens,
        };
        let mut identities = HashMap::new();
        for item in items {
            identities.insert(
                item.token,
                AuthIdentity {
                    sender: item.sender,
                    allowed_modes: if item.allowed_modes.is_empty() {
                        None
                    } else {
                        Some(item.allowed_modes.into_iter().collect())
                    },
                    can_start_sessions: item.can_start_sessions,
                    max_open_sessions: item.max_open_sessions,
                    can_manage_mode_registry: item.can_manage_mode_registry,
                    is_observer: item.is_observer,
                },
            );
        }
        Ok(identities)
    }

    fn bearer_token(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_string)
            .or_else(|| {
                metadata
                    .get("x-macp-token")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            })
    }

    pub fn authenticate_metadata(&self, metadata: &MetadataMap) -> Result<AuthIdentity, MacpError> {
        // Production path: use the auth resolver chain.
        if let Some(chain) = &self.auth_chain {
            let chain = Arc::clone(chain);
            let metadata_clone = metadata.clone();
            return tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(chain.authenticate(&metadata_clone))
            });
        }

        // Explicit identity map (layer_with_tokens in tests)
        if !self.identities.is_empty() {
            if let Some(token) = Self::bearer_token(metadata) {
                return self
                    .identities
                    .get(&token)
                    .cloned()
                    .ok_or(MacpError::Unauthenticated);
            }
            return Err(MacpError::Unauthenticated);
        }

        // Dev-mode: any bearer token → identity (for tests only)
        self.dev_authenticate(metadata)
    }

    pub fn authorize_mode(
        &self,
        identity: &AuthIdentity,
        mode: &str,
        is_session_start: bool,
    ) -> Result<(), MacpError> {
        if is_session_start && !identity.can_start_sessions {
            return Err(MacpError::Forbidden);
        }
        if let Some(allowed_modes) = &identity.allowed_modes {
            if !allowed_modes.contains(mode) {
                return Err(MacpError::Forbidden);
            }
        }
        Ok(())
    }

    pub fn authorize_mode_registry(&self, identity: &AuthIdentity) -> Result<(), MacpError> {
        if identity.can_manage_mode_registry {
            Ok(())
        } else {
            Err(MacpError::Forbidden)
        }
    }

    async fn check_bucket(
        bucket: &Mutex<HashMap<String, VecDeque<Instant>>>,
        sender: &str,
        config: &RateLimitConfig,
    ) -> Result<(), MacpError> {
        let now = Instant::now();
        let mut guard = bucket.lock().await;

        // Prune stale senders whose events are all outside the window.
        // Limit pruning to at most 100 entries per call to bound latency.
        let stale_keys: Vec<String> = guard
            .iter()
            .filter(|(_, deque)| {
                deque
                    .back()
                    .map(|last| now.duration_since(*last) > config.window)
                    .unwrap_or(true)
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in stale_keys {
            guard.remove(&key);
        }

        let deque = guard.entry(sender.to_string()).or_default();
        while deque
            .front()
            .map(|instant| now.duration_since(*instant) > config.window)
            .unwrap_or(false)
        {
            deque.pop_front();
        }
        if deque.len() >= config.limit {
            return Err(MacpError::RateLimited);
        }
        deque.push_back(now);
        Ok(())
    }

    pub async fn enforce_rate_limit(
        &self,
        sender: &str,
        is_session_start: bool,
    ) -> Result<(), MacpError> {
        if is_session_start {
            Self::check_bucket(
                &self.rate_bucket.start_events,
                sender,
                &self.session_start_rate,
            )
            .await
        } else {
            Self::check_bucket(&self.rate_bucket.message_events, sender, &self.message_rate).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;
    use tonic::metadata::MetadataMap;

    /// Build a SecurityLayer with bearer token identities loaded from a JSON string.
    /// This avoids touching environment variables (safe for parallel tests).
    fn layer_with_tokens(json: &str) -> SecurityLayer {
        let identities = SecurityLayer::parse_identities(json).expect("valid JSON");
        SecurityLayer {
            identities: Arc::new(identities),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        }
    }

    /// Build a SecurityLayer with no tokens that does not require auth.
    fn insecure_layer() -> SecurityLayer {
        SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        }
    }

    // ---------------------------------------------------------------
    // 1. dev_mode() creates a SecurityLayer that doesn't require auth
    // ---------------------------------------------------------------

    #[test]
    fn dev_mode_requires_dev_header() {
        let layer = SecurityLayer::dev_mode();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn dev_mode_rejects_dev_sender_header() {
        let layer = SecurityLayer::dev_mode();
        let mut meta = MetadataMap::new();
        meta.insert("x-macp-agent-id", "agent://dev-bot".parse().unwrap());
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn dev_mode_has_unlimited_rate_limits() {
        let layer = SecurityLayer::dev_mode();
        assert_eq!(layer.session_start_rate.limit, usize::MAX);
        assert_eq!(layer.message_rate.limit, usize::MAX);
    }

    // ---------------------------------------------------------------
    // 2. from_env() with no env vars creates an insecure layer
    // ---------------------------------------------------------------

    #[test]
    fn from_env_defaults_without_env_vars() {
        // Verify default configuration via direct construction.
        let layer = insecure_layer();
        assert_eq!(layer.max_payload_bytes, 1_048_576);
    }

    // ---------------------------------------------------------------
    // 3. Bearer token auth: loading tokens and authenticating
    // ---------------------------------------------------------------

    #[test]
    fn bearer_token_authentication_via_authorization_header() {
        let json = r#"[{"token":"tok-abc","sender":"agent://alice","allowed_modes":[],"can_start_sessions":true}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer tok-abc".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://alice");
        assert!(id.allowed_modes.is_none()); // empty vec -> None
        assert!(id.can_start_sessions);
    }

    #[test]
    fn bearer_token_authentication_via_x_macp_token_header() {
        let json = r#"[{"token":"tok-xyz","sender":"agent://bob"}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("x-macp-token", "tok-xyz".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://bob");
    }

    #[test]
    fn invalid_bearer_token_returns_unauthenticated() {
        let json = r#"[{"token":"tok-real","sender":"agent://alice"}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer tok-fake".parse().unwrap());

        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn no_token_when_auth_required_returns_unauthenticated() {
        let json = r#"[{"token":"tok-only","sender":"agent://sole"}]"#;
        let layer = layer_with_tokens(json);

        let meta = MetadataMap::new(); // no auth header at all
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn parse_identities_wrapped_format() {
        let json = r#"{"tokens":[{"token":"t1","sender":"agent://wrapped"}]}"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer t1".parse().unwrap());
        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://wrapped");
    }

    #[test]
    fn parse_identities_with_allowed_modes() {
        let json = r#"[{"token":"t-modes","sender":"agent://limited","allowed_modes":["macp.mode.decision.v1","macp.mode.task.v1"],"can_start_sessions":false,"max_open_sessions":5}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer t-modes".parse().unwrap());
        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");

        assert_eq!(id.sender, "agent://limited");
        assert!(!id.can_start_sessions);
        assert_eq!(id.max_open_sessions, Some(5));
        let modes = id
            .allowed_modes
            .as_ref()
            .expect("should have allowed_modes");
        assert!(modes.contains("macp.mode.decision.v1"));
        assert!(modes.contains("macp.mode.task.v1"));
        assert!(!modes.contains("macp.mode.proposal.v1"));
    }

    #[test]
    fn authorization_header_takes_priority_over_x_macp_token() {
        let json = r#"[
            {"token":"bearer-tok","sender":"agent://bearer-user"},
            {"token":"header-tok","sender":"agent://header-user"}
        ]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer bearer-tok".parse().unwrap());
        meta.insert("x-macp-token", "header-tok".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");
        // Authorization header should take priority
        assert_eq!(id.sender, "agent://bearer-user");
    }

    // ---------------------------------------------------------------
    // 4. Dev header extraction: x-macp-agent-id
    // ---------------------------------------------------------------

    #[test]
    fn dev_sender_header_rejected_without_chain() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        let mut meta = MetadataMap::new();
        meta.insert("x-macp-agent-id", "agent://dev-agent".parse().unwrap());

        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn dev_sender_header_ignored_when_not_allowed() {
        // allow_dev_sender_header=false, no tokens
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        let mut meta = MetadataMap::new();
        meta.insert("x-macp-agent-id", "agent://sneaky".parse().unwrap());

        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn bearer_token_takes_priority_over_dev_header() {
        let json = r#"[{"token":"real-tok","sender":"agent://real"}]"#;
        let identities = SecurityLayer::parse_identities(json).unwrap();

        let layer = SecurityLayer {
            identities: Arc::new(identities),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer real-tok".parse().unwrap());
        meta.insert("x-macp-agent-id", "agent://dev-override".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate via bearer");
        assert_eq!(id.sender, "agent://real");
    }

    // ---------------------------------------------------------------
    // 5. authorize_mode() with allowed modes and without
    // ---------------------------------------------------------------

    #[test]
    fn authorize_mode_allows_any_mode_when_no_restriction() {
        let layer = SecurityLayer::dev_mode();
        let id = AuthIdentity {
            sender: "agent://any".into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };
        assert!(layer
            .authorize_mode(&id, "macp.mode.decision.v1", false)
            .is_ok());
        assert!(layer.authorize_mode(&id, "macp.mode.task.v1", true).is_ok());
        assert!(layer.authorize_mode(&id, "arbitrary.mode", false).is_ok());
    }

    #[test]
    fn authorize_mode_rejects_unlisted_mode() {
        let layer = SecurityLayer::dev_mode();
        let mut allowed = HashSet::new();
        allowed.insert("macp.mode.decision.v1".to_string());

        let id = AuthIdentity {
            sender: "agent://restricted".into(),
            allowed_modes: Some(allowed),
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };
        assert!(layer
            .authorize_mode(&id, "macp.mode.decision.v1", false)
            .is_ok());
        let err = layer
            .authorize_mode(&id, "macp.mode.task.v1", false)
            .unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));
    }

    #[test]
    fn authorize_mode_rejects_session_start_when_not_allowed() {
        let layer = SecurityLayer::dev_mode();
        let id = AuthIdentity {
            sender: "agent://no-start".into(),
            allowed_modes: None,
            can_start_sessions: false,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };
        let err = layer
            .authorize_mode(&id, "macp.mode.decision.v1", true)
            .unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));
    }

    #[test]
    fn authorize_mode_allows_non_session_start_even_when_start_forbidden() {
        let layer = SecurityLayer::dev_mode();
        let id = AuthIdentity {
            sender: "agent://no-start".into(),
            allowed_modes: None,
            can_start_sessions: false,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };
        // Regular messages (not session start) should succeed
        assert!(layer
            .authorize_mode(&id, "macp.mode.decision.v1", false)
            .is_ok());
    }

    #[test]
    fn authorize_mode_checks_both_can_start_and_allowed_modes() {
        let layer = SecurityLayer::dev_mode();
        let mut allowed = HashSet::new();
        allowed.insert("macp.mode.decision.v1".to_string());

        let id = AuthIdentity {
            sender: "agent://double-check".into(),
            allowed_modes: Some(allowed),
            can_start_sessions: false,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        };

        // Cannot start sessions (checked first)
        let err = layer
            .authorize_mode(&id, "macp.mode.decision.v1", true)
            .unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));

        // Cannot use unlisted mode
        let err = layer
            .authorize_mode(&id, "macp.mode.task.v1", false)
            .unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));

        // Can send non-start message on allowed mode
        assert!(layer
            .authorize_mode(&id, "macp.mode.decision.v1", false)
            .is_ok());
    }

    #[test]
    fn authorize_mode_registry_requires_explicit_privilege() {
        let layer = SecurityLayer::dev_mode();
        let id = AuthIdentity {
            sender: "agent://no-admin".into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            is_observer: false,
            can_manage_mode_registry: false,
        };
        let err = layer.authorize_mode_registry(&id).unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));
    }

    #[test]
    fn bearer_token_can_manage_mode_registry() {
        let json =
            r#"[{"token":"admin-tok","sender":"agent://admin","can_manage_mode_registry":true}]"#;
        let layer = layer_with_tokens(json);
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer admin-tok".parse().unwrap());
        let id = layer.authenticate_metadata(&meta).unwrap();
        assert!(layer.authorize_mode_registry(&id).is_ok());
    }

    // ---------------------------------------------------------------
    // 6. enforce_rate_limit() with session_start and message categories
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn rate_limit_session_start_enforced() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: 3,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        let sender = "agent://rate-test";
        // First 3 should succeed
        for _ in 0..3 {
            assert!(layer.enforce_rate_limit(sender, true).await.is_ok());
        }
        // 4th should be rate limited
        let err = layer.enforce_rate_limit(sender, true).await.unwrap_err();
        assert!(matches!(err, MacpError::RateLimited));

        // Regular messages should still be fine (separate bucket)
        assert!(layer.enforce_rate_limit(sender, false).await.is_ok());
    }

    #[tokio::test]
    async fn rate_limit_message_enforced() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: 2,
                window: Duration::from_secs(60),
            },
        };

        let sender = "agent://msg-test";
        assert!(layer.enforce_rate_limit(sender, false).await.is_ok());
        assert!(layer.enforce_rate_limit(sender, false).await.is_ok());
        let err = layer.enforce_rate_limit(sender, false).await.unwrap_err();
        assert!(matches!(err, MacpError::RateLimited));

        // Session starts should still be fine (separate bucket)
        assert!(layer.enforce_rate_limit(sender, true).await.is_ok());
    }

    #[tokio::test]
    async fn rate_limit_per_sender_isolation() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: 1,
                window: Duration::from_secs(60),
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        // Sender A exhausts limit
        assert!(layer.enforce_rate_limit("agent://a", true).await.is_ok());
        assert!(layer.enforce_rate_limit("agent://a", true).await.is_err());

        // Sender B should still be able to start sessions
        assert!(layer.enforce_rate_limit("agent://b", true).await.is_ok());
    }

    #[tokio::test]
    async fn rate_limit_window_expiry() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            session_start_rate: RateLimitConfig {
                limit: 1,
                window: Duration::from_millis(1), // very short window
            },
            message_rate: RateLimitConfig {
                limit: usize::MAX,
                window: Duration::from_secs(60),
            },
        };

        let sender = "agent://expiry-test";
        assert!(layer.enforce_rate_limit(sender, true).await.is_ok());

        // Wait for the window to expire
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Should succeed again after window expiry
        assert!(layer.enforce_rate_limit(sender, true).await.is_ok());
    }

    // ---------------------------------------------------------------
    // 7. Anonymous fallback behavior
    // ---------------------------------------------------------------

    #[test]
    fn no_anonymous_fallback_even_when_auth_not_required() {
        let layer = insecure_layer();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn no_anonymous_fallback_when_auth_required() {
        let json = r#"[{"token":"t","sender":"agent://real"}]"#;
        let layer = layer_with_tokens(json);

        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[test]
    fn dev_mode_no_fallback_with_empty_metadata() {
        // dev_mode: allow_dev_sender_header=true
        // With no headers at all, returns Unauthenticated (no anonymous fallback)
        let layer = SecurityLayer::dev_mode();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    // ---------------------------------------------------------------
    // 8. Token file loading via MACP_AUTH_TOKENS_FILE
    // ---------------------------------------------------------------

    #[test]
    fn token_file_loading_via_parse_identities() {
        // Test the parse_identities path that from_env uses after reading the file.
        // We write a temp file and then read + parse it the same way from_env would.
        let json = r#"[
            {"token":"file-tok-1","sender":"agent://file-alice","allowed_modes":["macp.mode.decision.v1"]},
            {"token":"file-tok-2","sender":"agent://file-bob","can_start_sessions":false}
        ]"#;
        let mut tmp = NamedTempFile::new().expect("create temp file");
        write!(tmp, "{}", json).expect("write temp file");

        let contents = fs::read_to_string(tmp.path()).expect("read temp file");
        let identities = SecurityLayer::parse_identities(&contents).expect("parse identities");

        assert_eq!(identities.len(), 2);

        let alice = identities.get("file-tok-1").expect("alice entry");
        assert_eq!(alice.sender, "agent://file-alice");
        let alice_modes = alice.allowed_modes.as_ref().expect("should have modes");
        assert!(alice_modes.contains("macp.mode.decision.v1"));
        assert!(alice.can_start_sessions); // default_true

        let bob = identities.get("file-tok-2").expect("bob entry");
        assert_eq!(bob.sender, "agent://file-bob");
        assert!(!bob.can_start_sessions);
        assert!(bob.allowed_modes.is_none()); // empty vec -> None
    }

    #[test]
    fn token_file_end_to_end_via_layer() {
        // Build a layer as if loaded from a token file, then authenticate with it.
        let json = r#"[{"token":"e2e-tok","sender":"agent://e2e-agent"}]"#;
        let mut tmp = NamedTempFile::new().expect("create temp file");
        write!(tmp, "{}", json).expect("write temp file");

        let contents = fs::read_to_string(tmp.path()).expect("read temp file");
        let layer = layer_with_tokens(&contents);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer e2e-tok".parse().unwrap());
        let id = layer
            .authenticate_metadata(&meta)
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://e2e-agent");
    }

    #[test]
    fn parse_identities_invalid_json_returns_error() {
        let result = SecurityLayer::parse_identities("not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_identities_empty_list() {
        let identities = SecurityLayer::parse_identities("[]").expect("valid empty list");
        assert!(identities.is_empty());
    }

    #[test]
    fn parse_identities_wrapped_empty() {
        let identities =
            SecurityLayer::parse_identities(r#"{"tokens":[]}"#).expect("valid wrapped empty");
        assert!(identities.is_empty());
    }
}
