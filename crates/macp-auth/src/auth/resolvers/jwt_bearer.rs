use crate::auth::resolver::{AuthError, AuthResolver, ResolvedIdentity};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::metadata::MetadataMap;

#[derive(Debug, Clone, Deserialize)]
struct MACPClaims {
    sub: String,
    #[serde(default)]
    macp_scopes: Option<MACPScopes>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct MACPScopes {
    #[serde(default)]
    can_start_sessions: Option<bool>,
    #[serde(default)]
    can_manage_mode_registry: Option<bool>,
    #[serde(default)]
    is_observer: Option<bool>,
    #[serde(default)]
    allowed_modes: Option<Vec<String>>,
    #[serde(default)]
    max_open_sessions: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct JwtConfig {
    pub issuer: String,
    pub audience: String,
    pub algorithms: Vec<Algorithm>,
}

struct CachedKeys {
    keys: Vec<DecodingKey>,
    fetched_at: std::time::Instant,
}

pub struct JwtBearerResolver {
    config: JwtConfig,
    jwks_source: JwksSource,
    cached_keys: Arc<RwLock<Option<CachedKeys>>>,
    cache_ttl: std::time::Duration,
}

enum JwksSource {
    Inline(Vec<DecodingKey>),
    Url(String),
}

impl JwtBearerResolver {
    pub fn from_inline_json(config: JwtConfig, jwks_json: &str) -> Result<Self, String> {
        let jwks: serde_json::Value =
            serde_json::from_str(jwks_json).map_err(|e| format!("invalid JWKS JSON: {e}"))?;
        let keys = Self::parse_jwks(&jwks)?;
        tracing::info!(
            keys = keys.len(),
            issuer = %config.issuer,
            "JWT resolver initialized with inline JWKS"
        );
        Ok(Self {
            config,
            jwks_source: JwksSource::Inline(keys.clone()),
            cached_keys: Arc::new(RwLock::new(Some(CachedKeys {
                keys,
                fetched_at: std::time::Instant::now(),
            }))),
            cache_ttl: std::time::Duration::from_secs(u64::MAX),
        })
    }

    pub fn from_url(config: JwtConfig, url: String, cache_ttl_secs: u64) -> Self {
        tracing::info!(
            url = %url,
            issuer = %config.issuer,
            cache_ttl_secs,
            "JWT resolver initialized with JWKS URL"
        );
        Self {
            config,
            jwks_source: JwksSource::Url(url),
            cached_keys: Arc::new(RwLock::new(None)),
            cache_ttl: std::time::Duration::from_secs(cache_ttl_secs),
        }
    }

    fn extract_bearer(metadata: &MetadataMap) -> Option<String> {
        metadata
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string)
    }

    async fn get_keys(&self) -> Result<Vec<DecodingKey>, AuthError> {
        {
            let guard = self.cached_keys.read().await;
            if let Some(cached) = guard.as_ref() {
                if cached.fetched_at.elapsed() < self.cache_ttl {
                    return Ok(cached.keys.clone());
                }
            }
        }

        match &self.jwks_source {
            JwksSource::Inline(keys) => Ok(keys.clone()),
            JwksSource::Url(url) => {
                let keys = self.fetch_jwks(url).await?;
                let mut guard = self.cached_keys.write().await;
                *guard = Some(CachedKeys {
                    keys: keys.clone(),
                    fetched_at: std::time::Instant::now(),
                });
                Ok(keys)
            }
        }
    }

    async fn fetch_jwks(&self, url: &str) -> Result<Vec<DecodingKey>, AuthError> {
        let resp = reqwest::get(url)
            .await
            .map_err(|e| AuthError::FetchFailed(format!("JWKS fetch failed: {e}")))?;
        let jwks: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AuthError::FetchFailed(format!("JWKS parse failed: {e}")))?;
        Self::parse_jwks(&jwks).map_err(AuthError::FetchFailed)
    }

    fn parse_jwks(jwks: &serde_json::Value) -> Result<Vec<DecodingKey>, String> {
        let keys_arr = jwks
            .get("keys")
            .and_then(|k| k.as_array())
            .ok_or_else(|| "JWKS missing 'keys' array".to_string())?;

        let mut decoding_keys = Vec::new();
        for key in keys_arr {
            let kty = key.get("kty").and_then(|v| v.as_str()).unwrap_or("");
            match kty {
                "RSA" => {
                    let n = key.get("n").and_then(|v| v.as_str()).unwrap_or("");
                    let e = key.get("e").and_then(|v| v.as_str()).unwrap_or("");
                    if !n.is_empty() && !e.is_empty() {
                        if let Ok(dk) = DecodingKey::from_rsa_components(n, e) {
                            decoding_keys.push(dk);
                        }
                    }
                }
                "EC" => {
                    let x = key.get("x").and_then(|v| v.as_str()).unwrap_or("");
                    let y = key.get("y").and_then(|v| v.as_str()).unwrap_or("");
                    let crv = key.get("crv").and_then(|v| v.as_str()).unwrap_or("P-256");
                    if !x.is_empty() && !y.is_empty() {
                        if let Ok(dk) = DecodingKey::from_ec_components(x, y) {
                            let _ = crv;
                            decoding_keys.push(dk);
                        }
                    }
                }
                "oct" => {
                    if let Some(k_val) = key.get("k").and_then(|v| v.as_str()) {
                        decoding_keys.push(
                            DecodingKey::from_base64_secret(k_val)
                                .unwrap_or_else(|_| DecodingKey::from_secret(k_val.as_bytes())),
                        );
                    }
                }
                _ => {}
            }
        }

        if decoding_keys.is_empty() {
            return Err("no usable keys found in JWKS".to_string());
        }
        Ok(decoding_keys)
    }
}

#[async_trait::async_trait]
impl AuthResolver for JwtBearerResolver {
    fn name(&self) -> &str {
        "jwt_bearer"
    }

    async fn resolve(&self, metadata: &MetadataMap) -> Result<Option<ResolvedIdentity>, AuthError> {
        let token = match Self::extract_bearer(metadata) {
            Some(t) => t,
            None => return Ok(None),
        };

        // Only handle JWT-shaped tokens (contain dots)
        if !token.contains('.') {
            return Ok(None);
        }

        let keys = self.get_keys().await?;

        // Inspect the token header to pick a single algorithm to validate against.
        // jsonwebtoken 9 requires every algorithm in validation.algorithms to match
        // the DecodingKey's family, so a mixed list (RS256 + HS256) with one key
        // would always fail with InvalidAlgorithm. We still gate on the configured
        // allowlist — if the token's alg isn't configured, we reject it.
        let header = decode_header(&token)
            .map_err(|e| AuthError::InvalidCredential(format!("malformed JWT header: {e}")))?;
        if !self.config.algorithms.contains(&header.alg) {
            return Err(AuthError::InvalidCredential(format!(
                "JWT algorithm {:?} is not in the configured allowlist",
                header.alg
            )));
        }
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.algorithms = vec![header.alg];

        let mut last_err = None;
        for key in &keys {
            match decode::<MACPClaims>(&token, key, &validation) {
                Ok(token_data) => {
                    let claims = token_data.claims;
                    let scopes = claims.macp_scopes.unwrap_or_default();

                    return Ok(Some(ResolvedIdentity {
                        sender: claims.sub,
                        allowed_modes: scopes.allowed_modes.map(|m| m.into_iter().collect()),
                        can_start_sessions: scopes.can_start_sessions.unwrap_or(true),
                        max_open_sessions: scopes.max_open_sessions,
                        can_manage_mode_registry: scopes.can_manage_mode_registry.unwrap_or(false),
                        is_observer: scopes.is_observer.unwrap_or(false),
                        resolver: "jwt_bearer".to_string(),
                    }));
                }
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        }

        match last_err {
            Some(e) => {
                use jsonwebtoken::errors::ErrorKind;
                match e.kind() {
                    ErrorKind::ExpiredSignature => Err(AuthError::Expired),
                    ErrorKind::InvalidIssuer => {
                        Err(AuthError::InvalidCredential("invalid issuer".to_string()))
                    }
                    ErrorKind::InvalidAudience => {
                        Err(AuthError::InvalidCredential("invalid audience".to_string()))
                    }
                    _ => Err(AuthError::InvalidCredential(format!(
                        "JWT validation failed: {e}"
                    ))),
                }
            }
            None => Err(AuthError::InvalidCredential(
                "no keys available to validate JWT".to_string(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;

    const ISSUER: &str = "https://issuer.test";
    const AUDIENCE: &str = "macp-runtime";
    const SECRET: &[u8] = b"super-secret-symmetric-key-32-by";

    #[derive(Serialize)]
    struct TestClaims<'a> {
        sub: &'a str,
        iss: &'a str,
        aud: &'a str,
        exp: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        macp_scopes: Option<serde_json::Value>,
    }

    fn jwks_inline() -> String {
        let k = base64::engine::general_purpose::STANDARD.encode(SECRET);
        serde_json::json!({
            "keys": [
                { "kty": "oct", "alg": "HS256", "k": k }
            ]
        })
        .to_string()
    }

    fn config() -> JwtConfig {
        JwtConfig {
            issuer: ISSUER.to_string(),
            audience: AUDIENCE.to_string(),
            algorithms: vec![Algorithm::HS256],
        }
    }

    fn sign(claims: &TestClaims) -> String {
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("test-key".into());
        encode(&header, claims, &EncodingKey::from_secret(SECRET)).unwrap()
    }

    fn bearer(token: &str) -> MetadataMap {
        let mut m = MetadataMap::new();
        m.insert("authorization", format!("Bearer {token}").parse().unwrap());
        m
    }

    #[tokio::test]
    async fn valid_jwt_resolves_to_identity_with_scopes() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        let token = sign(&TestClaims {
            sub: "agent://alice",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: Some(serde_json::json!({
                "allowed_modes": ["macp.mode.decision.v1"],
                "can_start_sessions": true,
                "max_open_sessions": 5,
                "can_manage_mode_registry": false,
                "is_observer": false,
            })),
        });

        let id = resolver
            .resolve(&bearer(&token))
            .await
            .expect("ok")
            .expect("some");
        assert_eq!(id.sender, "agent://alice");
        assert_eq!(id.resolver, "jwt_bearer");
        assert!(id.can_start_sessions);
        assert_eq!(id.max_open_sessions, Some(5));
        assert!(!id.can_manage_mode_registry);
        assert!(!id.is_observer);
        let modes = id.allowed_modes.unwrap();
        assert!(modes.contains("macp.mode.decision.v1"));
    }

    #[tokio::test]
    async fn jwt_without_scopes_defaults_to_permissive_sender() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        let token = sign(&TestClaims {
            sub: "agent://bob",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: None,
        });
        let id = resolver.resolve(&bearer(&token)).await.unwrap().unwrap();
        assert_eq!(id.sender, "agent://bob");
        assert!(id.can_start_sessions); // default when unspecified
        assert!(id.allowed_modes.is_none());
        assert!(!id.is_observer);
    }

    #[tokio::test]
    async fn expired_jwt_returns_expired_error() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        // Exceed the default 60s leeway applied by jsonwebtoken's Validation.
        let token = sign(&TestClaims {
            sub: "agent://alice",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() - 600),
            macp_scopes: None,
        });
        let err = resolver.resolve(&bearer(&token)).await.unwrap_err();
        assert!(matches!(err, AuthError::Expired), "got {err:?}");
    }

    #[tokio::test]
    async fn wrong_issuer_rejected() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        let token = sign(&TestClaims {
            sub: "agent://alice",
            iss: "https://other.example",
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: None,
        });
        let err = resolver.resolve(&bearer(&token)).await.unwrap_err();
        assert!(
            matches!(err, AuthError::InvalidCredential(ref m) if m.contains("issuer")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn wrong_audience_rejected() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        let token = sign(&TestClaims {
            sub: "agent://alice",
            iss: ISSUER,
            aud: "other-audience",
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: None,
        });
        let err = resolver.resolve(&bearer(&token)).await.unwrap_err();
        assert!(
            matches!(err, AuthError::InvalidCredential(ref m) if m.contains("audience")),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn bad_signature_rejected() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        // Sign with a different key — signature won't verify.
        let claims = TestClaims {
            sub: "agent://alice",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: None,
        };
        let bad_token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"different-key-bytes-0123456789!!"),
        )
        .unwrap();
        let err = resolver.resolve(&bearer(&bad_token)).await.unwrap_err();
        assert!(
            matches!(err, AuthError::InvalidCredential(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn opaque_bearer_token_is_not_claimed() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        // No dots → not JWT-shaped → defer to next resolver.
        let outcome = resolver
            .resolve(&bearer("static-opaque-token"))
            .await
            .unwrap();
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn missing_authorization_header_is_not_claimed() {
        let resolver = JwtBearerResolver::from_inline_json(config(), &jwks_inline()).unwrap();
        let outcome = resolver.resolve(&MetadataMap::new()).await.unwrap();
        assert!(outcome.is_none());
    }

    #[tokio::test]
    async fn server_env_algorithms_accept_hs256_tokens() {
        // Reproduce the server's SecurityLayer::from_env() config: algorithms = RS256/ES256/HS256.
        let cfg = JwtConfig {
            issuer: ISSUER.to_string(),
            audience: AUDIENCE.to_string(),
            algorithms: vec![Algorithm::RS256, Algorithm::ES256, Algorithm::HS256],
        };
        let resolver = JwtBearerResolver::from_inline_json(cfg, &jwks_inline()).unwrap();
        let token = sign(&TestClaims {
            sub: "agent://alice",
            iss: ISSUER,
            aud: AUDIENCE,
            exp: (chrono::Utc::now().timestamp() + 300),
            macp_scopes: None,
        });
        let id = resolver
            .resolve(&bearer(&token))
            .await
            .expect("ok")
            .expect("some");
        assert_eq!(id.sender, "agent://alice");
    }
}
