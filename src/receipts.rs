//! Decision-receipt issuance (RFC-MACP-0013 §13): configuration, eligibility
//! and the per-runtime issuer state behind `GetDecisionReceipt`.
//!
//! Issuance is lazy: a receipt is built on the first `GetDecisionReceipt` for
//! an eligible session, persisted first-writer-wins, and every later call
//! serves the stored bytes. Nothing here runs on the message path, in replay
//! or in recovery, so issuance can never fail, delay or alter a Commitment.

use std::collections::HashMap;
use std::sync::Arc;

use macp_core::receipt::{KeySet, ReceiptSigner};
use macp_core::session::{Session, SessionState};
use macp_core::MacpError;
use tokio::sync::{Mutex, RwLock};

/// The `issuer` when `MACP_RUNTIME_ID` is unset: stable across restarts
/// (§13 item 9), but shared by every deployment that leaves it unset.
pub const DEFAULT_RUNTIME_ID: &str = "macp-runtime";

/// The modes whose resolution is the resolving Commitment's payload bytes,
/// verbatim. Extension modes (including `ext.multi_round.v1` and every
/// passthrough mode, promoted or not) store a JSON summary instead and get no
/// receipt.
pub const RECEIPT_MODES: [&str; 5] = [
    "macp.mode.decision.v1",
    "macp.mode.proposal.v1",
    "macp.mode.task.v1",
    "macp.mode.handoff.v1",
    "macp.mode.quorum.v1",
];

/// The `implementation` this runtime puts in a receipt's `semantics_rev`.
pub const IMPLEMENTATION: &str = "macp-runtime";

const STRIPES: usize = 64;

/// Receipt configuration, read once at startup.
#[derive(Debug)]
pub struct ReceiptConfig {
    signer: Option<ReceiptSigner>,
    keys: KeySet,
    issuer: String,
    runtime_id_set: bool,
}

impl Default for ReceiptConfig {
    /// Unsigned, no published keys, the default issuer.
    fn default() -> Self {
        ReceiptConfig {
            signer: None,
            keys: KeySet::new(),
            issuer: format!("runtime:{DEFAULT_RUNTIME_ID}"),
            runtime_id_set: false,
        }
    }
}

/// A variable read as unset when absent or empty (compose passes `""`).
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl ReceiptConfig {
    /// A configuration from explicit parts.
    pub fn new(
        signer: Option<ReceiptSigner>,
        retired: KeySet,
        runtime_id: Option<&str>,
    ) -> Result<Self, String> {
        let keys = match &signer {
            Some(s) => retired
                .with_active(s)
                .map_err(|e| format!("MACP_RECEIPT_PUBLIC_KEYS_PATH: {e}"))?,
            None => retired,
        };
        let runtime_id = runtime_id.filter(|id| !id.is_empty());
        Ok(ReceiptConfig {
            signer,
            keys,
            issuer: format!("runtime:{}", runtime_id.unwrap_or(DEFAULT_RUNTIME_ID)),
            runtime_id_set: runtime_id.is_some(),
        })
    }

    /// Read `MACP_RECEIPT_SIGNING_KEY_PATH` / `MACP_RECEIPT_SIGNING_KEY`,
    /// `MACP_RECEIPT_SIGNING_KID`, `MACP_RECEIPT_PUBLIC_KEYS_PATH` and
    /// `MACP_RUNTIME_ID`. An error names the variable and never echoes key
    /// material.
    pub fn from_env() -> Result<Self, String> {
        let kid = var("MACP_RECEIPT_SIGNING_KID");
        let text = match (
            var("MACP_RECEIPT_SIGNING_KEY_PATH"),
            var("MACP_RECEIPT_SIGNING_KEY"),
        ) {
            (Some(_), Some(_)) => return Err(
                "MACP_RECEIPT_SIGNING_KEY_PATH and MACP_RECEIPT_SIGNING_KEY are both set; set one"
                    .into(),
            ),
            (Some(path), None) => Some((
                "MACP_RECEIPT_SIGNING_KEY_PATH",
                std::fs::read_to_string(&path).map_err(|e| {
                    format!("MACP_RECEIPT_SIGNING_KEY_PATH: cannot read {path}: {e}")
                })?,
            )),
            (None, Some(inline)) => Some(("MACP_RECEIPT_SIGNING_KEY", inline)),
            (None, None) => None,
        };
        let signer = match text {
            Some((name, text)) => Some(
                ReceiptSigner::load(&text, kid.as_deref()).map_err(|e| format!("{name}: {e}"))?,
            ),
            None if kid.is_some() => {
                return Err(
                    "MACP_RECEIPT_SIGNING_KID is set but no signing key is (MACP_RECEIPT_SIGNING_KEY_PATH or MACP_RECEIPT_SIGNING_KEY)"
                        .into(),
                )
            }
            None => None,
        };
        let retired = match var("MACP_RECEIPT_PUBLIC_KEYS_PATH") {
            Some(path) => {
                let text = std::fs::read_to_string(&path).map_err(|e| {
                    format!("MACP_RECEIPT_PUBLIC_KEYS_PATH: cannot read {path}: {e}")
                })?;
                let doc: serde_json::Value = serde_json::from_str(&text)
                    .map_err(|_| format!("MACP_RECEIPT_PUBLIC_KEYS_PATH: {path} is not JSON"))?;
                KeySet::retired_from_jwks(&doc)
                    .map_err(|e| format!("MACP_RECEIPT_PUBLIC_KEYS_PATH: {e}"))?
            }
            None => KeySet::new(),
        };
        Self::new(signer, retired, var("MACP_RUNTIME_ID").as_deref())
    }

    /// Whether receipts are signed.
    pub fn signed(&self) -> bool {
        self.signer.is_some()
    }

    /// The active signer's `kid`, when signing.
    pub fn kid(&self) -> Option<&str> {
        self.signer.as_ref().map(ReceiptSigner::kid)
    }

    /// The receipt `issuer`.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Whether `MACP_RUNTIME_ID` was set (the issuer is deployment-specific).
    pub fn runtime_id_set(&self) -> bool {
        self.runtime_id_set
    }

    /// The published key set.
    pub fn keys(&self) -> &KeySet {
        &self.keys
    }

    pub(crate) fn signer(&self) -> Option<&ReceiptSigner> {
        self.signer.as_ref()
    }
}

/// Per-runtime issuer state: configuration, the published JWKS text, a
/// striped lock serializing issuance per session, and a cache of served bytes.
pub(crate) struct ReceiptIssuer {
    pub(crate) config: ReceiptConfig,
    pub(crate) jwks_json: String,
    stripes: Vec<Mutex<()>>,
    cache: RwLock<HashMap<String, Arc<Vec<u8>>>>,
}

impl ReceiptIssuer {
    pub(crate) fn new(config: ReceiptConfig) -> Self {
        let jwks_json = config.keys().to_jwks_json();
        ReceiptIssuer {
            config,
            jwks_json,
            stripes: (0..STRIPES).map(|_| Mutex::new(())).collect(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// The lock every issuance and disk-GC delete of `session_id` holds.
    pub(crate) fn stripe(&self, session_id: &str) -> &Mutex<()> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        session_id.hash(&mut h);
        &self.stripes[(h.finish() as usize) % STRIPES]
    }

    pub(crate) async fn cached(&self, session_id: &str) -> Option<Arc<Vec<u8>>> {
        self.cache.read().await.get(session_id).cloned()
    }

    /// Cache `bytes` unless an entry exists; returns the entry kept.
    pub(crate) async fn cache(&self, session_id: &str, bytes: Vec<u8>) -> Arc<Vec<u8>> {
        self.cache
            .write()
            .await
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(bytes))
            .clone()
    }

    pub(crate) async fn forget(&self, session_ids: &[String]) {
        let mut cache = self.cache.write().await;
        for id in session_ids {
            cache.remove(id);
        }
    }
}

/// The bytes a receipt for `session` is built from, or `ReceiptUnavailable`.
pub(crate) fn receipt_source(session: &Session) -> Result<&[u8], MacpError> {
    if session.state != SessionState::Resolved || !RECEIPT_MODES.contains(&session.mode.as_str()) {
        return Err(MacpError::ReceiptUnavailable);
    }
    session
        .resolution
        .as_deref()
        .ok_or(MacpError::ReceiptUnavailable)
}

/// `issued_at`: whole-second RFC 3339 UTC.
pub(crate) fn rfc3339_seconds(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_at_is_whole_second_utc() {
        assert_eq!(rfc3339_seconds(1_761_307_200_999), "2025-10-24T12:00:00Z");
    }

    #[test]
    fn only_resolved_standard_sessions_have_a_source() {
        let mut s = Session::builder("s", "macp.mode.decision.v1", "a").build();
        assert!(matches!(
            receipt_source(&s),
            Err(MacpError::ReceiptUnavailable)
        ));
        s.state = SessionState::Resolved;
        assert!(matches!(
            receipt_source(&s),
            Err(MacpError::ReceiptUnavailable)
        ));
        s.resolution = Some(vec![1]);
        assert_eq!(receipt_source(&s).ok(), Some(&[1u8][..]));
        s.mode = "ext.multi_round.v1".into();
        assert!(matches!(
            receipt_source(&s),
            Err(MacpError::ReceiptUnavailable)
        ));
    }

    #[test]
    fn config_reconciles_retired_and_active_keys() {
        let a = ReceiptSigner::from_seed(&[1; 32], "a").unwrap();
        let old = ReceiptSigner::from_seed(&[2; 32], "old").unwrap();
        let retired =
            KeySet::retired_from_jwks(&serde_json::json!({"keys": [old.public_jwk()]})).unwrap();
        let c = ReceiptConfig::new(Some(a), retired.clone(), None).unwrap();
        assert!(c.signed());
        assert_eq!(c.keys().len(), 2);
        assert_eq!(c.issuer(), "runtime:macp-runtime");
        assert!(!c.runtime_id_set());
        // Unsigned with retired keys: the retired keys stay published.
        let c = ReceiptConfig::new(None, retired, Some("prod-eu")).unwrap();
        assert!(!c.signed());
        assert_eq!(c.keys().len(), 1);
        assert_eq!(c.issuer(), "runtime:prod-eu");
        let impostor = ReceiptSigner::from_seed(&[3; 32], "old").unwrap();
        let retired =
            KeySet::retired_from_jwks(&serde_json::json!({"keys": [old.public_jwk()]})).unwrap();
        assert!(ReceiptConfig::new(Some(impostor), retired, None)
            .unwrap_err()
            .starts_with("MACP_RECEIPT_PUBLIC_KEYS_PATH"));
    }
}
