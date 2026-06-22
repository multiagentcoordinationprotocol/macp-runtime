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
