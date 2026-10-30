//! Issue a `macp-receipt/1` decision receipt (RFC-MACP-0013 §13, Appendix A).
//!
//! [`build`] is pure: the caller supplies the resolving payload's wire bytes,
//! the issuer and the `issued_at` timestamp, so the same inputs and key always
//! yield the same bytes. The output is the JCS form of the whole receipt,
//! `attestations` included.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use macp_pb::pb::CommitmentPayload;
use serde_json::{json, Map, Value};

use super::jcs;
use super::keys::ReceiptSigner;
use super::verify::{project, FORMAT};
use crate::commitment_hash::{commitment_hash_of_wire, CannotVerify};

/// What a receipt is built from.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiptInputs<'a> {
    /// The session the receipt is for.
    pub session_id: &'a str,
    /// The session's mode.
    pub mode: &'a str,
    /// The resolving Envelope's payload bytes, verbatim.
    pub wire: &'a [u8],
    /// The stable issuer identifier (§13 item 9).
    pub issuer: &'a str,
    /// RFC 3339 UTC timestamp, e.g. `2026-10-24T12:00:00Z`.
    pub issued_at: &'a str,
    /// `semantics_rev`: implementation name and revision, when known.
    pub semantics_rev: Option<(&'a str, u32)>,
}

impl<'a> ReceiptInputs<'a> {
    /// Inputs without `semantics_rev`.
    pub fn new(
        session_id: &'a str,
        mode: &'a str,
        wire: &'a [u8],
        issuer: &'a str,
        issued_at: &'a str,
    ) -> Self {
        ReceiptInputs {
            session_id,
            mode,
            wire,
            issuer,
            issued_at,
            semantics_rev: None,
        }
    }

    /// Set `semantics_rev`.
    pub fn semantics_rev(mut self, implementation: &'a str, rev: u32) -> Self {
        self.semantics_rev = Some((implementation, rev));
        self
    }
}

/// Why no receipt could be built.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// The wire bytes are not a `CommitmentPayload` the hash label covers.
    NotHashable(CannotVerify),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::NotHashable(why) => write!(f, "no receipt: {why}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// The protected header for `kid`: exactly `{"alg":"EdDSA","kid":…}`.
pub fn jws_header(kid: &str) -> String {
    format!(
        "{{\"alg\":\"EdDSA\",\"kid\":{}}}",
        Value::String(kid.into())
    )
}

/// Build the receipt for `inputs`, signed when `signer` is given.
pub fn build(
    inputs: &ReceiptInputs<'_>,
    signer: Option<&ReceiptSigner>,
) -> Result<Vec<u8>, BuildError> {
    let hash = commitment_hash_of_wire(inputs.wire).map_err(BuildError::NotHashable)?;
    let payload = <CommitmentPayload as prost::Message>::decode(inputs.wire).map_err(|e| {
        BuildError::NotHashable(CannotVerify::Malformed {
            detail: e.to_string(),
        })
    })?;

    let mut body = Map::new();
    body.insert("receipt_format".into(), FORMAT.into());
    body.insert("session_id".into(), inputs.session_id.into());
    body.insert("mode".into(), inputs.mode.into());
    body.insert("commitment".into(), project(&payload));
    body.insert("commitment_hash".into(), hash.into());
    body.insert(
        "commitment_wire_b64".into(),
        STANDARD.encode(inputs.wire).into(),
    );
    if let Some((implementation, rev)) = inputs.semantics_rev {
        body.insert(
            "semantics_rev".into(),
            json!({ "implementation": implementation, "rev": rev }),
        );
    }
    body.insert("issuer".into(), inputs.issuer.into());
    body.insert("issued_at".into(), inputs.issued_at.into());

    let mut receipt = Value::Object(body);
    let attestations = match signer {
        None => json!([]),
        Some(signer) => {
            let body = jcs::canonicalize(&receipt)
                .expect("a receipt body holds only strings and integers");
            let header = URL_SAFE_NO_PAD.encode(jws_header(signer.kid()));
            let mut input = format!("{header}.").into_bytes();
            input.extend_from_slice(URL_SAFE_NO_PAD.encode(body).as_bytes());
            let sig = URL_SAFE_NO_PAD.encode(signer.sign(&input));
            json!([{ "type": "jws", "jws": format!("{header}..{sig}") }])
        }
    };
    receipt["attestations"] = attestations;
    Ok(jcs::canonicalize(&receipt).expect("a receipt holds only strings and integers"))
}
