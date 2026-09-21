//! `macp-core` — the transport-free coordination vocabulary of MACP.
//!
//! This crate holds the stable types every other MACP crate (and external
//! library consumers) build on, with no dependency on tonic, storage, or any
//! async runtime:
//!
//! - [`error::MacpError`] — the canonical error enum and RFC error codes
//! - [`session`] — the [`session::Session`] model and `SessionStart` validation
//! - [`mode::ModeResponse`] — the result a mode hands back to the kernel
//! - [`decision`] — the Decision mode's domain types (shared with policy)
//! - [`policy`] — [`policy::PolicyDefinition`], [`policy::PolicyDecision`],
//!   [`policy::PolicyError`], the shared [`policy::CommitmentRules`], and the
//!   [`policy::PolicyEvaluator`] trait that modes call through
//! - [`commitment_hash`] — the RFC-MACP-0013 canonical commitment hash

/// The MACP protocol version this runtime implements and negotiates
/// (`Envelope.macp_version`, `Initialize`'s `supported_protocol_versions`/
/// `selected_protocol_version`). Real, kernel-enforced API — unlike the
/// `#[doc(hidden)]` constants in [`session`], this one backs an actual
/// wire-level check, so it is a plain, stable `pub const`.
pub const MACP_VERSION: &str = "1.0";

pub mod commitment_hash;
pub mod decision;
pub mod error;
pub mod mode;
pub mod policy;
pub mod session;

// Flat re-exports for the most commonly used types.
pub use error::MacpError;
pub use mode::ModeResponse;
pub use policy::{PolicyDecision, PolicyDefinition, PolicyError, PolicyEvaluator};
pub use session::{Session, SessionState};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macp_version_value() {
        assert_eq!(MACP_VERSION, "1.0");
    }
}
