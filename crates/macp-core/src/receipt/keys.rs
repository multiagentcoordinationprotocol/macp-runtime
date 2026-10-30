//! Receipt signing keys and the published key set (RFC-MACP-0013 §13 item 8).
//!
//! [`ReceiptSigner`] holds the one Ed25519 key a runtime signs with.
//! [`KeySet`] is what `GetReceiptKeys` publishes: the public half of that key
//! plus any retired keys whose receipts are still served. Nothing here ever
//! formats key material: errors name the problem, not the bytes.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde_json::{json, Map, Value};

/// JWK members that carry private or symmetric key material (RFC 7518 §6).
/// A published key set holds none of them.
pub const PRIVATE_MEMBERS: [&str; 8] = ["d", "p", "q", "dp", "dq", "qi", "oth", "k"];

/// Why a key or key set was refused.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// The text is neither a PKCS#8 PEM nor a JSON JWK.
    Unrecognized,
    /// A PEM that does not decode as a PKCS#8 Ed25519 private key.
    BadPem,
    /// A JWK that is not `kty: OKP`, `crv: Ed25519`.
    NotEd25519,
    /// A required JWK member is missing (or not a string).
    MissingMember(&'static str),
    /// A coordinate that is not 32 bytes of base64url, or not a valid point.
    BadCoordinate(&'static str),
    /// A private JWK whose `x` is not the public key of its `d`.
    PublicKeyMismatch,
    /// The signer has no `kid`: a PEM carries none, and none was configured.
    MissingKid,
    /// The JWK's own `kid` and the configured one differ.
    KidConflict { jwk: String, configured: String },
    /// A published key carries a private member.
    PrivateMember { kid: String, member: &'static str },
    /// Two keys in the set share a `kid`.
    DuplicateKid(String),
    /// A JWKS document whose `keys` is missing or not an array.
    KeysNotArray,
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyError::Unrecognized => f.write_str("not a PKCS#8 PEM or a JWK"),
            KeyError::BadPem => f.write_str("not a PKCS#8 Ed25519 private key"),
            KeyError::NotEd25519 => f.write_str("not an Ed25519 key (kty OKP, crv Ed25519)"),
            KeyError::MissingMember(m) => write!(f, "the JWK has no \"{m}\""),
            KeyError::BadCoordinate(m) => write!(f, "the JWK's \"{m}\" is not an Ed25519 key"),
            KeyError::PublicKeyMismatch => f.write_str("the JWK's \"x\" does not match its \"d\""),
            KeyError::MissingKid => f.write_str("the signing key has no kid"),
            KeyError::KidConflict { jwk, configured } => write!(
                f,
                "the JWK's kid {jwk:?} differs from the configured kid {configured:?}"
            ),
            KeyError::PrivateMember { kid, member } => {
                write!(f, "key {kid:?} carries the private member \"{member}\"")
            }
            KeyError::DuplicateKid(kid) => write!(f, "kid {kid:?} names more than one key"),
            KeyError::KeysNotArray => f.write_str("\"keys\" is missing or not an array"),
        }
    }
}

impl std::error::Error for KeyError {}

/// The Ed25519 key a runtime signs receipts with, and its `kid`.
pub struct ReceiptSigner {
    key: SigningKey,
    kid: String,
}

impl std::fmt::Debug for ReceiptSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceiptSigner")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl ReceiptSigner {
    /// A signer from a raw 32-byte Ed25519 seed.
    pub fn from_seed(seed: &[u8; 32], kid: impl Into<String>) -> Result<Self, KeyError> {
        let kid = kid.into();
        if kid.is_empty() {
            return Err(KeyError::MissingKid);
        }
        Ok(ReceiptSigner {
            key: SigningKey::from_bytes(seed),
            kid,
        })
    }

    /// A signer from a PKCS#8 PEM (`openssl genpkey -algorithm ed25519`).
    pub fn from_pkcs8_pem(pem: &str, kid: &str) -> Result<Self, KeyError> {
        let key = SigningKey::from_pkcs8_pem(pem).map_err(|_| KeyError::BadPem)?;
        if kid.is_empty() {
            return Err(KeyError::MissingKid);
        }
        Ok(ReceiptSigner {
            key,
            kid: kid.into(),
        })
    }

    /// A signer from a private OKP JWK. Its own `kid` is used; when it has
    /// none, `configured_kid` is. Both present and different is an error.
    pub fn from_jwk(jwk: &Value, configured_kid: Option<&str>) -> Result<Self, KeyError> {
        if jwk.get("kty").and_then(Value::as_str) != Some("OKP")
            || jwk.get("crv").and_then(Value::as_str) != Some("Ed25519")
        {
            return Err(KeyError::NotEd25519);
        }
        let d = jwk
            .get("d")
            .and_then(Value::as_str)
            .ok_or(KeyError::MissingMember("d"))?;
        let seed = coordinate(d, "d")?;
        let configured_kid = configured_kid.filter(|k| !k.is_empty());
        let kid = match (jwk.get("kid").and_then(Value::as_str), configured_kid) {
            (Some(own), Some(conf)) if own != conf => {
                return Err(KeyError::KidConflict {
                    jwk: own.into(),
                    configured: conf.into(),
                })
            }
            (Some(own), _) => own,
            (None, Some(conf)) => conf,
            (None, None) => return Err(KeyError::MissingKid),
        };
        let signer = Self::from_seed(&seed, kid)?;
        if let Some(x) = jwk.get("x") {
            let x = x.as_str().ok_or(KeyError::BadCoordinate("x"))?;
            if coordinate(x, "x")? != signer.key.verifying_key().to_bytes() {
                return Err(KeyError::PublicKeyMismatch);
            }
        }
        Ok(signer)
    }

    /// A signer from key text: a PKCS#8 PEM (which needs `configured_kid`)
    /// or a private OKP JWK.
    pub fn load(text: &str, configured_kid: Option<&str>) -> Result<Self, KeyError> {
        let trimmed = text.trim_start();
        if trimmed.starts_with("-----BEGIN") {
            return Self::from_pkcs8_pem(text, configured_kid.unwrap_or_default());
        }
        match serde_json::from_str::<Value>(text) {
            Ok(jwk) if jwk.is_object() => Self::from_jwk(&jwk, configured_kid),
            _ => Err(KeyError::Unrecognized),
        }
    }

    /// The `kid` this signer puts in every JWS header.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public JWK: `{kty, crv, kid, x, alg, use}`.
    pub fn public_jwk(&self) -> Value {
        json!({
            "kty": "OKP",
            "crv": "Ed25519",
            "kid": self.kid,
            "x": URL_SAFE_NO_PAD.encode(self.key.verifying_key().to_bytes()),
            "alg": "EdDSA",
            "use": "sig",
        })
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.sign(message).to_bytes()
    }
}

fn coordinate(b64: &str, member: &'static str) -> Result<[u8; 32], KeyError> {
    URL_SAFE_NO_PAD
        .decode(b64)
        .ok()
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .ok_or(KeyError::BadCoordinate(member))
}

/// The key set `GetReceiptKeys` publishes: public Ed25519 JWKs with unique
/// `kid`s and no private members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeySet {
    keys: Vec<Map<String, Value>>,
}

impl KeySet {
    /// No keys: `{"keys":[]}`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retired public keys from a JWKS document. Refuses a key with a
    /// private member, a key that is not a valid Ed25519 public key, a key
    /// without `kid`, and a repeated `kid`.
    pub fn retired_from_jwks(doc: &Value) -> Result<Self, KeyError> {
        let keys = doc
            .get("keys")
            .and_then(Value::as_array)
            .ok_or(KeyError::KeysNotArray)?;
        let mut set = KeySet::new();
        for key in keys {
            let obj = key.as_object().ok_or(KeyError::MissingMember("kty"))?;
            let kid = obj
                .get("kid")
                .and_then(Value::as_str)
                .ok_or(KeyError::MissingMember("kid"))?;
            if let Some(member) = PRIVATE_MEMBERS.into_iter().find(|m| obj.contains_key(*m)) {
                return Err(KeyError::PrivateMember {
                    kid: kid.into(),
                    member,
                });
            }
            if !obj.contains_key("kty") {
                return Err(KeyError::MissingMember("kty"));
            }
            if obj.get("kty").and_then(Value::as_str) != Some("OKP")
                || obj.get("crv").and_then(Value::as_str) != Some("Ed25519")
            {
                return Err(KeyError::NotEd25519);
            }
            let x = obj
                .get("x")
                .and_then(Value::as_str)
                .ok_or(KeyError::MissingMember("x"))?;
            VerifyingKey::from_bytes(&coordinate(x, "x")?)
                .map_err(|_| KeyError::BadCoordinate("x"))?;
            if set.kid(kid).is_some() {
                return Err(KeyError::DuplicateKid(kid.into()));
            }
            set.keys.push(obj.clone());
        }
        Ok(set)
    }

    /// This set with `signer`'s public key first. A retired key with the
    /// same `kid` and the same `x` is dropped as a duplicate; one with the
    /// same `kid` and a different `x` is refused (a `kid` never names two keys).
    pub fn with_active(mut self, signer: &ReceiptSigner) -> Result<Self, KeyError> {
        let active = signer.public_jwk();
        if let Some(i) = self
            .keys
            .iter()
            .position(|k| k.get("kid") == active.get("kid"))
        {
            if self.keys[i].get("x") != active.get("x") {
                return Err(KeyError::DuplicateKid(signer.kid().into()));
            }
            self.keys.remove(i);
        }
        let Value::Object(active) = active else {
            unreachable!("public_jwk is an object")
        };
        self.keys.insert(0, active);
        Ok(self)
    }

    fn kid(&self, kid: &str) -> Option<&Map<String, Value>> {
        self.keys
            .iter()
            .find(|k| k.get("kid").and_then(Value::as_str) == Some(kid))
    }

    /// Number of keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// `true` when there are no keys.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The JWKS document, `{"keys":[…]}`.
    pub fn to_jwks(&self) -> Value {
        json!({ "keys": self.keys })
    }

    /// [`Self::to_jwks`] as JSON text.
    pub fn to_jwks_json(&self) -> String {
        self.to_jwks().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn debug_never_shows_the_key() {
        let s = ReceiptSigner::from_seed(&seed(7), "k1").unwrap();
        let dbg = format!("{s:?}");
        assert!(dbg.contains("k1"));
        assert!(!dbg.contains(&URL_SAFE_NO_PAD.encode(seed(7))));
    }

    #[test]
    fn jwk_round_trips_and_kids_reconcile() {
        let s = ReceiptSigner::from_seed(&seed(1), "a").unwrap();
        let mut jwk = s.public_jwk();
        jwk["d"] = URL_SAFE_NO_PAD.encode(seed(1)).into();
        let back = ReceiptSigner::from_jwk(&jwk, None).unwrap();
        assert_eq!(back.public_jwk(), s.public_jwk());
        assert_eq!(ReceiptSigner::from_jwk(&jwk, Some("a")).unwrap().kid(), "a");
        assert!(matches!(
            ReceiptSigner::from_jwk(&jwk, Some("b")),
            Err(KeyError::KidConflict { .. })
        ));
        jwk.as_object_mut().unwrap().remove("kid");
        assert_eq!(ReceiptSigner::from_jwk(&jwk, Some("b")).unwrap().kid(), "b");
        assert_eq!(
            ReceiptSigner::from_jwk(&jwk, None).unwrap_err(),
            KeyError::MissingKid
        );
        jwk["x"] = ReceiptSigner::from_seed(&seed(2), "z")
            .unwrap()
            .public_jwk()["x"]
            .clone();
        assert_eq!(
            ReceiptSigner::from_jwk(&jwk, Some("b")).unwrap_err(),
            KeyError::PublicKeyMismatch
        );
    }

    #[test]
    fn load_tells_pem_from_jwk() {
        assert_eq!(
            ReceiptSigner::load("hello", Some("k")).unwrap_err(),
            KeyError::Unrecognized
        );
        assert_eq!(
            ReceiptSigner::load(
                "-----BEGIN PRIVATE KEY-----\nAA==\n-----END PRIVATE KEY-----\n",
                Some("k")
            )
            .unwrap_err(),
            KeyError::BadPem
        );
        assert_eq!(
            ReceiptSigner::load(r#"{"kty":"RSA","kid":"r"}"#, None).unwrap_err(),
            KeyError::NotEd25519
        );
    }

    #[test]
    fn key_set_refuses_private_members_and_duplicate_kids() {
        let pubkey = ReceiptSigner::from_seed(&seed(3), "old")
            .unwrap()
            .public_jwk();
        for member in PRIVATE_MEMBERS {
            let mut k = pubkey.clone();
            k[member] = "AAAA".into();
            assert_eq!(
                KeySet::retired_from_jwks(&json!({ "keys": [k] })).unwrap_err(),
                KeyError::PrivateMember {
                    kid: "old".into(),
                    member
                },
                "{member}"
            );
        }
        assert_eq!(
            KeySet::retired_from_jwks(&json!({ "keys": [pubkey, pubkey] })).unwrap_err(),
            KeyError::DuplicateKid("old".into())
        );
        assert_eq!(
            KeySet::retired_from_jwks(&json!({})).unwrap_err(),
            KeyError::KeysNotArray
        );
    }

    #[test]
    fn active_key_leads_and_a_kid_never_names_two_keys() {
        let old = ReceiptSigner::from_seed(&seed(3), "old").unwrap();
        let new = ReceiptSigner::from_seed(&seed(4), "new").unwrap();
        let retired = KeySet::retired_from_jwks(&json!({ "keys": [old.public_jwk()] })).unwrap();
        let set = retired.clone().with_active(&new).unwrap();
        assert_eq!(
            set.to_jwks(),
            json!({ "keys": [new.public_jwk(), old.public_jwk()] })
        );
        // The active key also listed as retired: kept once.
        assert_eq!(retired.clone().with_active(&old).unwrap().len(), 1);
        // A different key reusing a retired kid: refused.
        let impostor = ReceiptSigner::from_seed(&seed(5), "old").unwrap();
        assert_eq!(
            retired.with_active(&impostor).unwrap_err(),
            KeyError::DuplicateKid("old".into())
        );
        assert_eq!(KeySet::new().to_jwks_json(), r#"{"keys":[]}"#);
    }
}
