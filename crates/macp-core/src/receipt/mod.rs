//! Decision receipts, `macp-receipt/1` (RFC-MACP-0013 §13, Appendix A).
//! Behind the non-default `receipts` feature.
//!
//! - [`build`]: issue a receipt from the resolving payload's wire bytes,
//!   optionally signed with an Ed25519 [`ReceiptSigner`] as a detached-payload
//!   compact JWS.
//! - [`verify`]: the A.4 verifier — integrity, then one outcome per
//!   attestation, then a [`Verdict`].
//! - [`KeySet`]: the public key set `GetReceiptKeys` publishes.
//! - [`jcs`]: RFC 8785 canonicalization of the receipt body.

mod build;
pub mod jcs;
mod keys;
mod verify;

pub use build::{build, jws_header, BuildError, ReceiptInputs};
pub use keys::{KeyError, KeySet, ReceiptSigner, PRIVATE_MEMBERS};
pub use verify::{
    add_jwks, project, verify, AttestationOutcome, Integrity, TrustedKeys, Verdict, Verification,
    FORMAT,
};
