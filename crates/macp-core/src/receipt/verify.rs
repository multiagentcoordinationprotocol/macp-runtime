//! RFC-MACP-0013 Appendix A.4: verify a `macp-receipt/1` decision receipt.
//!
//! A verifier reports **integrity** (does `commitment` have the recorded
//! `commitment_hash`?) and, only when that holds, one outcome per
//! **attestation**, then an overall [`Verdict`]. Integrity needs no keys;
//! authenticity comes only from an attestation that a trusted key signed.
//!
//! Integrity reuses `macp_core::commitment_hash`, the runtime's own
//! implementation of the Section 3 projection and Section 4 algorithm, and
//! `verify_commitment_hash` for the optional `commitment_wire_b64` (Section
//! 6.1). The receipt body a `jws` attestation signs is canonicalized by
//! [`super::jcs`].
//!
//! The spec's 12 reference vectors (`schemas/conformance/receipt/`) are
//! vendored under the repo's `tests/vectors/receipt/` and run by this crate's
//! `receipt_vectors` test.

use std::collections::BTreeMap;

use crate::commitment_hash::{commitment_hash, verify_commitment_hash, VerifyOutcome};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use macp_pb::pb::{CommitmentPayload, CommitmentRef};
use serde::Serialize;
use serde_json::{Map, Value};

use super::keys::KeyError;

/// The one receipt format this verifier implements.
pub const FORMAT: &str = "macp-receipt/1";

/// A.4 step 2.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Integrity {
    Match,
    Mismatch,
    CannotVerify,
}

/// A.4 step 3, one per attestation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttestationOutcome {
    Valid,
    Invalid,
    /// The format is understood but no trusted key matches.
    UnknownKey,
    /// The format or `alg` is not implemented, or a vendor attestation.
    Unsupported,
}

/// A.4 step 4.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Integrity holds and at least one attestation is valid.
    Attested,
    /// Integrity holds; nobody this verifier trusts has vouched for it.
    IntegrityOnly,
    /// Integrity is mismatch or cannot-verify; attestations not evaluated.
    Unverified,
    /// `receipt_format` is not one this verifier implements.
    Refused,
}

impl Verdict {
    /// The vector file's spelling (`attested`, `integrity_only`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Attested => "attested",
            Verdict::IntegrityOnly => "integrity_only",
            Verdict::Unverified => "unverified",
            Verdict::Refused => "refused",
        }
    }
}

/// What [`verify`] found. Field names and values are the vector file's
/// `expected` shape, so a report compares directly against a vector.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    /// `None` only when the receipt is refused.
    pub integrity: Option<Integrity>,
    /// Empty unless integrity is match.
    pub attestations: Vec<AttestationOutcome>,
    pub verdict: Verdict,
}

/// Trusted Ed25519 keys, by `kid`, as JWK `x` values (base64url, no padding).
pub type TrustedKeys = BTreeMap<String, String>;

/// Verify `receipt` (A.4) against the keys this verifier trusts.
pub fn verify(receipt: &Value, trusted: &TrustedKeys) -> Verification {
    if receipt.get("receipt_format").and_then(Value::as_str) != Some(FORMAT) {
        return Verification {
            integrity: None,
            attestations: Vec::new(),
            verdict: Verdict::Refused,
        };
    }
    let integrity = integrity(receipt);
    if integrity != Integrity::Match {
        return Verification {
            integrity: Some(integrity),
            attestations: Vec::new(),
            verdict: Verdict::Unverified,
        };
    }
    let body = receipt_body(receipt);
    let attestations: Vec<_> = receipt
        .get("attestations")
        .and_then(Value::as_array)
        .map(|atts| {
            atts.iter()
                .map(|a| attestation(a, body.as_deref(), trusted))
                .collect()
        })
        .unwrap_or_default();
    let verdict = if attestations.contains(&AttestationOutcome::Valid) {
        Verdict::Attested
    } else {
        Verdict::IntegrityOnly
    };
    Verification {
        integrity: Some(Integrity::Match),
        attestations,
        verdict,
    }
}

fn integrity(receipt: &Value) -> Integrity {
    let Some(commitment) = receipt.get("commitment") else {
        return Integrity::CannotVerify;
    };
    let Some(payload) = payload_of(commitment) else {
        return Integrity::CannotVerify;
    };
    let claimed = receipt.get("commitment_hash").and_then(Value::as_str);
    if claimed != Some(commitment_hash(&payload).as_str()) {
        return Integrity::Mismatch;
    }
    let Some(wire) = receipt.get("commitment_wire_b64") else {
        return Integrity::Match;
    };
    let Some(wire) = wire.as_str().and_then(|w| STANDARD.decode(w).ok()) else {
        return Integrity::CannotVerify;
    };
    match verify_commitment_hash(&wire, claimed.unwrap_or_default()) {
        o if o.is_match() => {}
        VerifyOutcome::Mismatch { .. } => return Integrity::Mismatch,
        _ => return Integrity::CannotVerify,
    }
    // The wire hashes to the claim; it must also be the Commitment shown.
    match <CommitmentPayload as prost::Message>::decode(wire.as_slice()) {
        Ok(decoded) if project(&decoded) == *commitment => Integrity::Match,
        Ok(_) => Integrity::Mismatch,
        Err(_) => Integrity::CannotVerify,
    }
}

const STRING_FIELDS: [&str; 7] = [
    "commitment_id",
    "action",
    "authority_scope",
    "reason",
    "mode_version",
    "policy_version",
    "configuration_version",
];

/// The `CommitmentPayload` a Section 3 projection stands for, or `None` when
/// it is not one: a key outside the frozen field set, or a value of the
/// wrong JSON type. Absent scalars take their defaults, as in the projection.
fn payload_of(commitment: &Value) -> Option<CommitmentPayload> {
    let obj = commitment.as_object()?;
    let known =
        |k: &str| STRING_FIELDS.contains(&k) || k == "outcome_positive" || k == "supersedes";
    if !obj.keys().all(|k| known(k)) {
        return None;
    }
    let string = |o: &Map<String, Value>, k: &str| match o.get(k) {
        None => Some(String::new()),
        Some(v) => v.as_str().map(str::to_owned),
    };
    let supersedes = match obj.get("supersedes") {
        None => None,
        Some(s) => {
            let s = s.as_object()?;
            if !s
                .keys()
                .all(|k| k == "session_id" || k == "commitment_hash")
            {
                return None;
            }
            Some(CommitmentRef {
                session_id: string(s, "session_id")?,
                commitment_hash: string(s, "commitment_hash")?,
            })
        }
    };
    Some(CommitmentPayload {
        commitment_id: string(obj, "commitment_id")?,
        action: string(obj, "action")?,
        authority_scope: string(obj, "authority_scope")?,
        reason: string(obj, "reason")?,
        mode_version: string(obj, "mode_version")?,
        policy_version: string(obj, "policy_version")?,
        configuration_version: string(obj, "configuration_version")?,
        outcome_positive: match obj.get("outcome_positive") {
            None => false,
            Some(v) => v.as_bool()?,
        },
        supersedes,
    })
}

/// The Section 3 projection of a decoded payload: the `commitment` member of
/// a receipt. Every string member is present (empty when unset), as is
/// `outcome_positive`; `supersedes` only when set.
pub fn project(p: &CommitmentPayload) -> Value {
    let mut obj = serde_json::json!({
        "commitment_id": p.commitment_id,
        "action": p.action,
        "authority_scope": p.authority_scope,
        "reason": p.reason,
        "mode_version": p.mode_version,
        "policy_version": p.policy_version,
        "configuration_version": p.configuration_version,
        "outcome_positive": p.outcome_positive,
    });
    if let Some(s) = &p.supersedes {
        obj["supersedes"] = serde_json::json!({
            "session_id": s.session_id,
            "commitment_hash": s.commitment_hash,
        });
    }
    obj
}

/// A.2: the receipt without `attestations`, canonicalized with JCS. `None`
/// when the receipt holds a value JCS cannot represent.
fn receipt_body(receipt: &Value) -> Option<Vec<u8>> {
    let mut body = receipt.as_object()?.clone();
    body.remove("attestations");
    super::jcs::canonicalize(&Value::Object(body)).ok()
}

fn attestation(att: &Value, body: Option<&[u8]>, trusted: &TrustedKeys) -> AttestationOutcome {
    use AttestationOutcome::*;
    // A vendor attestation never counts here: binds=other is informative
    // only, and this verifier implements no vendor format.
    if att.get("type").and_then(Value::as_str) != Some("jws") {
        return Unsupported;
    }
    let jws = att.get("jws").and_then(Value::as_str).unwrap_or_default();
    let parts: Vec<&str> = jws.split('.').collect();
    let [header_b64, "", sig_b64] = parts.as_slice() else {
        return Invalid;
    };
    let header: Option<Value> = URL_SAFE_NO_PAD
        .decode(header_b64)
        .ok()
        .and_then(|h| serde_json::from_slice(&h).ok());
    let (Some(header), Ok(signature)) = (header, URL_SAFE_NO_PAD.decode(sig_b64)) else {
        return Invalid;
    };
    if !header.is_object() || header.get("alg").and_then(Value::as_str) != Some("EdDSA") {
        return Unsupported;
    }
    let Some(key) = header
        .get("kid")
        .and_then(Value::as_str)
        .and_then(|kid| trusted.get(kid))
    else {
        return UnknownKey;
    };
    let (Some(body), Some(key), Ok(signature)) =
        (body, verifying_key(key), Signature::from_slice(&signature))
    else {
        return Invalid;
    };
    let mut signing_input = format!("{header_b64}.").into_bytes();
    signing_input.extend_from_slice(URL_SAFE_NO_PAD.encode(body).as_bytes());
    if key.verify_strict(&signing_input, &signature).is_ok() {
        Valid
    } else {
        Invalid
    }
}

fn verifying_key(x: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(x).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Add the keys in a JWK (`{"kty":"OKP","crv":"Ed25519","kid":…,"x":…}`) or a
/// JWKS (`{"keys":[…]}`) document to `trusted`. Non-Ed25519 keys are skipped;
/// returns how many were added.
pub fn add_jwks(doc: &Value, trusted: &mut TrustedKeys) -> Result<usize, KeyError> {
    let keys = match doc.get("keys") {
        Some(Value::Array(keys)) => keys.iter().collect(),
        Some(_) => return Err(KeyError::KeysNotArray),
        None => vec![doc],
    };
    let mut added = 0;
    for key in keys {
        if key.get("kty").and_then(Value::as_str) != Some("OKP")
            || key.get("crv").and_then(Value::as_str) != Some("Ed25519")
        {
            continue;
        }
        let kid = key.get("kid").and_then(Value::as_str);
        let x = key.get("x").and_then(Value::as_str);
        let (Some(kid), Some(x)) = (kid, x) else {
            return Err(KeyError::MissingMember(if kid.is_none() {
                "kid"
            } else {
                "x"
            }));
        };
        trusted.insert(kid.into(), x.into());
        added += 1;
    }
    Ok(added)
}
