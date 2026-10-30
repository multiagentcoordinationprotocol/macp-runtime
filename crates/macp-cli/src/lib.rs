//! The `macp` command line, as a library: each command's logic lives here so
//! it can be tested without spawning the binary.
//!
//! - [`receipt`]: RFC-MACP-0013 Appendix A.4 decision-receipt verification
//!   (`macp verify`), with [`jcs`] for the receipt body. Both are
//!   `macp_core::receipt`, re-exported here under their 0.10 paths.
//! - [`stack`]: the self-host stack from the repo's `deploy/`, embedded at
//!   build time (`macp up`, `macp down`, `macp status`).
//! - [`replay`]: a recorded session replayed alone or under a candidate
//!   policy, from a data directory, a log file or a runtime (`macp replay`).
//! - [`tail`]: a session's accepted envelopes over a passive `StreamSession`
//!   (`macp session tail`).
//!
//! `macp conform` is `macp_conform::cli`, unchanged.

pub use macp_core::receipt::jcs;
pub mod receipt {
    //! `macp_core::receipt`, re-exported (RFC-MACP-0013 §13, Appendix A).
    pub use macp_core::receipt::*;
}
pub mod replay;
pub mod stack;
pub mod tail;
