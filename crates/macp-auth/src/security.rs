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

/// Effective `ListSessions` page size when the client sends `page_size = 0`.
/// Overridable with `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE`.
pub const DEFAULT_LIST_SESSIONS_PAGE_SIZE: usize = 100;

/// Hard cap on `ListSessions` page size; a larger client-requested `page_size`
/// is clamped down to it. Overridable with `MACP_LIST_SESSIONS_MAX_PAGE_SIZE`.
pub const MAX_LIST_SESSIONS_PAGE_SIZE: usize = 1000;

/// Raw (unparsed) values of the two `ListSessions` page-size env vars.
///
/// A named struct rather than two positional `Option<String>` parameters
/// precisely because the two arguments are the same type: with positional
/// arguments, transposing them at the call site compiles, and the resulting
/// misconfiguration (`MAX` feeding the default and vice versa) is invisible to
/// every test that only drives the resolver. Naming the fields forces the call
/// site to write the field name next to the env-var name it reads.
struct RawPageSizeEnv {
    default_raw: Option<String>,
    max_raw: Option<String>,
}

#[derive(Default)]
struct RateBucket {
    start_events: Mutex<HashMap<String, VecDeque<Instant>>>,
    message_events: Mutex<HashMap<String, VecDeque<Instant>>>,
    /// Requests since the last full stale-sweep of each map. Full sweeps are
    /// amortized (every `SWEEP_EVERY` requests) so no single request pays a
    /// scan proportional to total sender cardinality, while the maps still
    /// get fully cleaned on a bounded cadence.
    start_sweep_counter: std::sync::atomic::AtomicU64,
    message_sweep_counter: std::sync::atomic::AtomicU64,
}

#[derive(Clone)]
pub struct SecurityLayer {
    identities: Arc<HashMap<String, AuthIdentity>>,
    rate_bucket: Arc<RateBucket>,
    auth_chain: Option<Arc<crate::auth::AuthResolverChain>>,
    pub max_payload_bytes: usize,
    /// Effective `ListSessions` page size when the client sends `page_size = 0`.
    pub list_sessions_default_page_size: usize,
    /// Hard cap applied to a client-requested `ListSessions` page size.
    pub list_sessions_max_page_size: usize,
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
            // Deliberately the PRODUCTION defaults, not `usize::MAX`. The two
            // rate limits below are unlimited because that is the safe test
            // default, but an unlimited page size would make "the default cap
            // was applied" assertions pass vacuously in unit tests while the
            // same code path capped requests over the wire.
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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

    /// Dev-mode authenticate: accepts any bearer token as a FULLY-PRIVILEGED
    /// identity (can start sessions, can manage the mode registry).
    ///
    /// Reached whenever no auth is configured — both by `dev_mode()` in tests
    /// AND by `from_env()` when the operator sets no tokens/issuer. Startup
    /// therefore refuses to run without configured auth unless
    /// `MACP_ALLOW_INSECURE=1` (see `has_configured_auth` and `src/main.rs`);
    /// an operator who forgets auth env vars must not silently run an
    /// any-token-is-admin server.
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

    /// Resolves the two `ListSessions` page-size limits from raw env-var
    /// values. Split out of `from_env` so the parse/clamp behavior is unit
    /// testable without mutating the process environment, which races with
    /// concurrently running tests under cargo's test thread pool.
    ///
    /// Like the other numeric vars this layer reads, it is silent on bad
    /// input: an unparseable value — and `0`, which is just as much an
    /// operator error — falls back to the compiled-in default rather than
    /// producing a degenerate page size.
    ///
    /// Being silent here is only safe for the `macp-runtime` **binary**, which
    /// runs `validate_env_config` in `src/main.rs` first and refuses to start
    /// on either kind of bad input. A library embedder calling
    /// [`SecurityLayer::from_env`] directly gets no such validation, so this
    /// fallback — not a startup abort — is what it actually sees.
    fn resolve_list_sessions_page_sizes(raw: RawPageSizeEnv) -> (usize, usize) {
        let RawPageSizeEnv {
            default_raw,
            max_raw,
        } = raw;
        // `filter(|&v| v > 0)` rather than a trailing `.max(1)`: `0` is the
        // same class of operator error as `"abc"` and gets the same treatment
        // (fall back to the compiled-in value) instead of silently becoming a
        // one-item page. The limits are still never zero.
        let max = max_raw
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(MAX_LIST_SESSIONS_PAGE_SIZE);
        let default_from_env = default_raw
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0);
        // Provenance, not just the value: the clamp warning must not name
        // `MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE` when nothing set it. Note this
        // tracks where the value in hand actually came from, so an unparseable
        // or zero default counts as built-in — that operator is told about the
        // max they *did* set, and `validate_env_config` reports the bad default
        // separately.
        let mut default = default_from_env.unwrap_or(DEFAULT_LIST_SESSIONS_PAGE_SIZE);
        if default > max {
            tracing::warn!(
                effective_default = default,
                default_source = Self::page_size_default_source(default_from_env.is_some()),
                max,
                "{}",
                Self::clamp_warning_message(default_from_env.is_some())
            );
            default = max;
        }
        (default, max)
    }

    /// Where the effective `ListSessions` default page size came from, as a
    /// log field.
    fn page_size_default_source(default_from_env: bool) -> &'static str {
        if default_from_env {
            "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE"
        } else {
            "built-in"
        }
    }

    /// Wording for the clamp warning emitted by
    /// [`SecurityLayer::resolve_list_sessions_page_sizes`].
    ///
    /// Split out purely so the *choice* between the two messages is unit
    /// testable: this crate has no test subscriber, so the emitted line itself
    /// cannot be asserted without a new dev-dependency.
    ///
    /// The built-in wording exists because the reachable-for-the-binary case
    /// (see `from_env_clamps_default_page_size_to_max`) is the one where the
    /// operator set only the max. Naming a variable they never configured
    /// sends them grepping for it.
    fn clamp_warning_message(default_from_env: bool) -> &'static str {
        if default_from_env {
            "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE exceeds MACP_LIST_SESSIONS_MAX_PAGE_SIZE; clamping the default down to the max"
        } else {
            "the effective ListSessions default page size is the built-in one and exceeds MACP_LIST_SESSIONS_MAX_PAGE_SIZE; clamping the default down to the max"
        }
    }

    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let max_payload_bytes = std::env::var("MACP_MAX_PAYLOAD_BYTES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1_048_576);

        // The field names below are the only thing tying each env var to the
        // parameter it feeds; keep them next to their `std::env::var` call.
        // Unit tests here can only exercise the resolver, so they cannot catch
        // a transposition at this call site — the end-to-end coverage that
        // pins the binding lives with the `ListSessions` handler, in
        // `integration_tests/tests/tier1_protocol/test_list_sessions_pagination.rs`
        // (`default_page_size_applied_when_page_size_is_zero` and
        // `page_size_above_max_is_clamped` set the two vars to distinct values
        // and assert the row counts each one produces).
        let (list_sessions_default_page_size, list_sessions_max_page_size) =
            Self::resolve_list_sessions_page_sizes(RawPageSizeEnv {
                default_raw: std::env::var("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE").ok(),
                max_raw: std::env::var("MACP_LIST_SESSIONS_MAX_PAGE_SIZE").ok(),
            });

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
            // Asymmetric algorithms only by default. HS256 in a default
            // allowlist is a latent confusion risk: if the JWKS ever contains
            // an `oct` key, symmetric tokens become verifiable. Operators who
            // genuinely use shared-secret JWTs must opt in explicitly.
            let algorithms = std::env::var("MACP_AUTH_JWT_ALGS")
                .ok()
                .map(|raw| {
                    raw.split(',')
                        .filter_map(|a| match a.trim().to_uppercase().as_str() {
                            "RS256" => Some(jsonwebtoken::Algorithm::RS256),
                            "ES256" => Some(jsonwebtoken::Algorithm::ES256),
                            "HS256" => Some(jsonwebtoken::Algorithm::HS256),
                            other => {
                                tracing::warn!(alg = other, "ignoring unsupported JWT algorithm");
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|algs| !algs.is_empty())
                .unwrap_or_else(|| {
                    vec![
                        jsonwebtoken::Algorithm::RS256,
                        jsonwebtoken::Algorithm::ES256,
                    ]
                });
            let config = crate::auth::resolvers::jwt_bearer::JwtConfig {
                issuer,
                audience,
                algorithms,
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
            list_sessions_default_page_size,
            list_sessions_max_page_size,
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

    pub async fn authenticate_metadata(
        &self,
        metadata: &MetadataMap,
    ) -> Result<AuthIdentity, MacpError> {
        // Production path: use the auth resolver chain. Fully async — the
        // previous block_in_place/block_on bridge parked a worker thread for
        // the whole JWKS fetch and panicked on a current-thread runtime.
        if let Some(chain) = &self.auth_chain {
            return chain.authenticate(metadata).await;
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
        sweep_counter: &std::sync::atomic::AtomicU64,
        sender: &str,
        config: &RateLimitConfig,
    ) -> Result<(), MacpError> {
        let now = Instant::now();
        let mut guard = bucket.lock().await;

        // Amortized stale-sender sweep. A per-request full scan is O(total
        // senders) — and sender cardinality is attacker-controllable via
        // distinct authenticated identities — so the full sweep runs only
        // every SWEEP_EVERY requests. Between sweeps a request touches only
        // its own deque. The map therefore stays bounded (a full clean every
        // SWEEP_EVERY requests) without any request paying the whole scan
        // more than 1/SWEEP_EVERY of the time.
        const SWEEP_EVERY: u64 = 128;
        let tick = sweep_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if tick.is_multiple_of(SWEEP_EVERY) {
            guard.retain(|_, deque| {
                deque
                    .back()
                    .map(|last| now.duration_since(*last) <= config.window)
                    .unwrap_or(false)
            });
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

    /// Whether any real authentication is configured (static tokens and/or a
    /// JWT resolver). When false, `authenticate_metadata` falls through to
    /// the any-token-is-admin dev path — callers gate startup on this.
    pub fn has_configured_auth(&self) -> bool {
        self.auth_chain.is_some()
    }

    pub async fn enforce_rate_limit(
        &self,
        sender: &str,
        is_session_start: bool,
    ) -> Result<(), MacpError> {
        if is_session_start {
            Self::check_bucket(
                &self.rate_bucket.start_events,
                &self.rate_bucket.start_sweep_counter,
                sender,
                &self.session_start_rate,
            )
            .await
        } else {
            Self::check_bucket(
                &self.rate_bucket.message_events,
                &self.rate_bucket.message_sweep_counter,
                sender,
                &self.message_rate,
            )
            .await
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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

    #[tokio::test]
    async fn dev_mode_requires_dev_header() {
        let layer = SecurityLayer::dev_mode();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn dev_mode_rejects_dev_sender_header() {
        let layer = SecurityLayer::dev_mode();
        let mut meta = MetadataMap::new();
        meta.insert("x-macp-agent-id", "agent://dev-bot".parse().unwrap());
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
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
    // 2b. ListSessions page-size limits (D5)
    // ---------------------------------------------------------------
    //
    // These tests never call `std::env::set_var`. Cargo runs unit tests
    // multi-threaded in a single process, so mutating the process environment
    // would race with any concurrently running test that reads it. The
    // existing precedent in this file (`from_env_defaults_without_env_vars`
    // above) sidesteps that by asserting on a directly constructed layer
    // rather than on `from_env`; the same discipline is applied here by
    // driving the pure resolver `from_env` delegates to.

    /// Test helper mirroring the shape `from_env` builds, so each test case
    /// still names which raw value is which.
    fn raw_page_sizes(default_raw: Option<&str>, max_raw: Option<&str>) -> RawPageSizeEnv {
        RawPageSizeEnv {
            default_raw: default_raw.map(str::to_owned),
            max_raw: max_raw.map(str::to_owned),
        }
    }

    #[test]
    fn from_env_page_size_defaults_without_env_vars() {
        // Neither variable present -> the production defaults.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(RawPageSizeEnv {
                default_raw: None,
                max_raw: None,
            }),
            (DEFAULT_LIST_SESSIONS_PAGE_SIZE, MAX_LIST_SESSIONS_PAGE_SIZE)
        );
        assert_eq!(DEFAULT_LIST_SESSIONS_PAGE_SIZE, 100);
        assert_eq!(MAX_LIST_SESSIONS_PAGE_SIZE, 1000);

        // And `from_env` really does carry those values through — but only
        // assert it when the ambient environment leaves the vars it reads
        // unset, so a developer who exports them does not get a spurious
        // failure. `from_env` is fallible on the auth vars too (it reads a
        // token file / parses JSON / builds a JWT resolver), so those are part
        // of the same guard.
        if [
            "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE",
            "MACP_LIST_SESSIONS_MAX_PAGE_SIZE",
            "MACP_AUTH_TOKENS_FILE",
            "MACP_AUTH_TOKENS_JSON",
            "MACP_AUTH_ISSUER",
        ]
        .iter()
        .all(|v| std::env::var_os(v).is_none())
        {
            let layer = SecurityLayer::from_env().expect("from_env with no auth configured");
            assert_eq!(layer.list_sessions_default_page_size, 100);
            assert_eq!(layer.list_sessions_max_page_size, 1000);
        }
    }

    #[test]
    fn dev_mode_uses_production_page_size_defaults() {
        let layer = SecurityLayer::dev_mode();
        assert_eq!(layer.list_sessions_default_page_size, 100);
        assert_eq!(layer.list_sessions_max_page_size, 1000);
        // Unlike the rate limits, these must NOT be unlimited. No existing cap
        // assertion depends on these two values — `src/server.rs`'s unit tests
        // pin their own via `page_size_security(..)`, and the Tier 1 coverage in
        // `integration_tests/tests/tier1_protocol/test_list_sessions_pagination.rs`
        // pins its own via env. The assertion is here because `dev_mode` is a
        // `pub` constructor: anything that builds a server from it must still
        // page for real, so a future test written against it cannot pass
        // vacuously on an unbounded page.
        assert_ne!(layer.list_sessions_default_page_size, usize::MAX);
        assert_ne!(layer.list_sessions_max_page_size, usize::MAX);
    }

    #[test]
    fn from_env_clamps_default_page_size_to_max() {
        // Default above the (defaulted) max is clamped down; a `tracing::warn!`
        // fires on the clamp (not asserted here — this crate has no test
        // subscriber and adding one is not worth a dev-dependency).
        //
        // For the `macp-runtime` binary this branch is unreachable *when the
        // default is explicitly set*: `validate_env_config` aborts startup on
        // an explicit default above the effective max, whether or not the max
        // itself was set. It is still reached by the binary when only the max
        // is set, below the built-in default — `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=50`
        // alone passes validation (the cross-field check only fires on an
        // explicit default) and then clamps 100 down to 50. That case is
        // covered by `clamps_builtin_default_against_a_smaller_explicit_max`.
        // The clamp also stays as defense in depth for library embedders that
        // call `from_env` without any validation.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("5000"), None)),
            (1000, 1000)
        );
        // Also clamped against an explicitly configured, smaller max.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(
                Some("2000"),
                Some("50")
            )),
            (50, 50)
        );
        // A default at or below the max is left alone.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(
                Some("50"),
                Some("200")
            )),
            (50, 200)
        );
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(
                Some("200"),
                Some("200")
            )),
            (200, 200)
        );
    }

    /// The one clamp case the `macp-runtime` binary can actually reach, and
    /// the one every other clamp test misses: only the max is set, below the
    /// built-in default. `validate_env_config`'s cross-field check fires only
    /// on an explicitly set default, so `MACP_LIST_SESSIONS_MAX_PAGE_SIZE=50`
    /// alone starts the server and lands here.
    ///
    /// Without this, making the clamp conditional on an explicitly supplied
    /// default survives the whole suite while shipping `default=100 > max=50`.
    #[test]
    fn clamps_builtin_default_against_a_smaller_explicit_max() {
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(None, Some("50"))),
            (50, 50),
            "the built-in default must clamp to a smaller explicit max, not stay above it"
        );

        // The warning this path takes must not name a variable the operator
        // never set. Only the message *choice* is asserted, not the emitted
        // line: capturing `tracing` output would need a dev-dependency this
        // crate does not carry.
        let builtin = SecurityLayer::clamp_warning_message(false);
        let explicit = SecurityLayer::clamp_warning_message(true);
        assert!(
            !builtin.contains("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE"),
            "the built-in-default warning must not name an unset variable: {builtin}"
        );
        assert!(
            builtin.contains("built-in") && builtin.contains("MACP_LIST_SESSIONS_MAX_PAGE_SIZE"),
            "the built-in-default warning must say where the default came from and name the max: {builtin}"
        );
        assert!(
            explicit.contains("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE"),
            "an explicitly configured default must still be named: {explicit}"
        );
        assert_eq!(SecurityLayer::page_size_default_source(false), "built-in");
        assert_eq!(
            SecurityLayer::page_size_default_source(true),
            "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE"
        );
    }

    #[test]
    fn page_size_resolver_treats_zero_like_garbage() {
        // Unparseable values fall back to the defaults — this layer stays
        // silent on bad input. For the binary, `validate_env_config` in
        // `src/main.rs` refuses to start first; a library embedder calling
        // `from_env` directly sees exactly the fallbacks asserted here.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("abc"), Some(""))),
            (100, 1000)
        );
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(
                Some("-1"),
                Some("1e3")
            )),
            (100, 1000)
        );

        // `0` is the same class of operator error as `"abc"` and gets exactly
        // the same treatment: fall back to the compiled-in value. Notably it
        // does NOT floor to 1 — a one-item page is a far more surprising
        // outcome to attach to the more plausible typo.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("0"), None)),
            (100, 1000),
            "a default of 0 falls back to the compiled-in default, not to 1"
        );
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(None, Some("0"))),
            (100, 1000),
            "a max of 0 falls back to the compiled-in max and leaves the default alone"
        );
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("0"), Some("0"))),
            (100, 1000)
        );
        // Bad input on one side must not disturb a good value on the other.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("0"), Some("25"))),
            (25, 25),
            "the good max is honored, and the fallback default clamps to it"
        );
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(Some("7"), Some("0"))),
            (7, 1000),
            "the good default is honored against the compiled-in max"
        );

        // Whatever the input, neither limit is ever degenerate.
        for (default_raw, max_raw) in [
            (Some("0"), None),
            (None, Some("0")),
            (Some("0"), Some("0")),
            (Some("abc"), Some("0")),
        ] {
            let (default, max) = SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(
                default_raw,
                max_raw,
            ));
            assert!(
                default > 0 && max > 0,
                "{default_raw:?}/{max_raw:?} -> ({default}, {max})"
            );
        }

        // A max larger than the default raises only the ceiling.
        assert_eq!(
            SecurityLayer::resolve_list_sessions_page_sizes(raw_page_sizes(None, Some("100000"))),
            (100, 100_000)
        );
    }

    // ---------------------------------------------------------------
    // 3. Bearer token auth: loading tokens and authenticating
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn bearer_token_authentication_via_authorization_header() {
        let json = r#"[{"token":"tok-abc","sender":"agent://alice","allowed_modes":[],"can_start_sessions":true}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer tok-abc".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .await
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://alice");
        assert!(id.allowed_modes.is_none()); // empty vec -> None
        assert!(id.can_start_sessions);
    }

    #[tokio::test]
    async fn bearer_token_authentication_via_x_macp_token_header() {
        let json = r#"[{"token":"tok-xyz","sender":"agent://bob"}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("x-macp-token", "tok-xyz".parse().unwrap());

        let id = layer
            .authenticate_metadata(&meta)
            .await
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://bob");
    }

    #[tokio::test]
    async fn invalid_bearer_token_returns_unauthenticated() {
        let json = r#"[{"token":"tok-real","sender":"agent://alice"}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer tok-fake".parse().unwrap());

        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn no_token_when_auth_required_returns_unauthenticated() {
        let json = r#"[{"token":"tok-only","sender":"agent://sole"}]"#;
        let layer = layer_with_tokens(json);

        let meta = MetadataMap::new(); // no auth header at all
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn parse_identities_wrapped_format() {
        let json = r#"{"tokens":[{"token":"t1","sender":"agent://wrapped"}]}"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer t1".parse().unwrap());
        let id = layer
            .authenticate_metadata(&meta)
            .await
            .expect("should authenticate");
        assert_eq!(id.sender, "agent://wrapped");
    }

    #[tokio::test]
    async fn parse_identities_with_allowed_modes() {
        let json = r#"[{"token":"t-modes","sender":"agent://limited","allowed_modes":["macp.mode.decision.v1","macp.mode.task.v1"],"can_start_sessions":false,"max_open_sessions":5}]"#;
        let layer = layer_with_tokens(json);

        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer t-modes".parse().unwrap());
        let id = layer
            .authenticate_metadata(&meta)
            .await
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

    #[tokio::test]
    async fn authorization_header_takes_priority_over_x_macp_token() {
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
            .await
            .expect("should authenticate");
        // Authorization header should take priority
        assert_eq!(id.sender, "agent://bearer-user");
    }

    // ---------------------------------------------------------------
    // 4. Dev header extraction: x-macp-agent-id
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn dev_sender_header_rejected_without_chain() {
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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

        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn dev_sender_header_ignored_when_not_allowed() {
        // allow_dev_sender_header=false, no tokens
        let layer = SecurityLayer {
            identities: Arc::new(HashMap::new()),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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

        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn bearer_token_takes_priority_over_dev_header() {
        let json = r#"[{"token":"real-tok","sender":"agent://real"}]"#;
        let identities = SecurityLayer::parse_identities(json).unwrap();

        let layer = SecurityLayer {
            identities: Arc::new(identities),
            rate_bucket: Arc::new(RateBucket::default()),
            auth_chain: None,
            max_payload_bytes: 1_048_576,
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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
            .await
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

    #[tokio::test]
    async fn bearer_token_can_manage_mode_registry() {
        let json =
            r#"[{"token":"admin-tok","sender":"agent://admin","can_manage_mode_registry":true}]"#;
        let layer = layer_with_tokens(json);
        let mut meta = MetadataMap::new();
        meta.insert("authorization", "Bearer admin-tok".parse().unwrap());
        let id = layer.authenticate_metadata(&meta).await.unwrap();
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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
            list_sessions_default_page_size: DEFAULT_LIST_SESSIONS_PAGE_SIZE,
            list_sessions_max_page_size: MAX_LIST_SESSIONS_PAGE_SIZE,
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

    #[tokio::test]
    async fn no_anonymous_fallback_even_when_auth_not_required() {
        let layer = insecure_layer();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn no_anonymous_fallback_when_auth_required() {
        let json = r#"[{"token":"t","sender":"agent://real"}]"#;
        let layer = layer_with_tokens(json);

        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
        assert!(matches!(err, MacpError::Unauthenticated));
    }

    #[tokio::test]
    async fn dev_mode_no_fallback_with_empty_metadata() {
        // dev_mode: allow_dev_sender_header=true
        // With no headers at all, returns Unauthenticated (no anonymous fallback)
        let layer = SecurityLayer::dev_mode();
        let meta = MetadataMap::new();
        let err = layer.authenticate_metadata(&meta).await.unwrap_err();
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

    #[tokio::test]
    async fn token_file_end_to_end_via_layer() {
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
            .await
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

    /// The amortized sweep must actually bound the sender map: stale senders
    /// are fully removed when the periodic full sweep fires, so the map does
    /// not grow with total distinct-sender cardinality forever.
    #[tokio::test]
    async fn rate_bucket_sweep_removes_stale_senders() {
        let bucket = RateBucket::default();
        let config = RateLimitConfig {
            limit: 10,
            window: Duration::from_millis(1),
        };

        // Tick 0 sweeps the (empty) map; ticks 1..=49 add 49 distinct senders.
        for i in 0..50 {
            SecurityLayer::check_bucket(
                &bucket.start_events,
                &bucket.start_sweep_counter,
                &format!("agent://stale-{i}"),
                &config,
            )
            .await
            .unwrap();
        }
        assert!(bucket.start_events.lock().await.len() >= 49);

        // Let every recorded event age out of the window.
        tokio::time::sleep(Duration::from_millis(5)).await;

        // Drive the counter across the next sweep boundary (tick 128) with a
        // single fresh sender. After the sweep only the fresh sender remains.
        for _ in 0..80 {
            SecurityLayer::check_bucket(
                &bucket.start_events,
                &bucket.start_sweep_counter,
                "agent://fresh",
                &config,
            )
            .await
            .ok(); // fresh sender may hit its own limit; irrelevant here
        }
        let map = bucket.start_events.lock().await;
        assert!(
            map.len() <= 2,
            "stale senders must be swept; map still has {} entries",
            map.len()
        );
        assert!(map.contains_key("agent://fresh"));
    }
}
