//! The `macp` command line, as a library: each command's logic lives here so
//! it can be tested without spawning the binary.
//!
//! - [`receipt`]: RFC-MACP-0013 Appendix A.4 decision-receipt verification
//!   (`macp verify`), with [`jcs`] for the receipt body.
//! - [`stack`]: the self-host stack from the repo's `deploy/`, embedded at
//!   build time (`macp up`, `macp down`, `macp status`).
//! - [`replay`]: a recorded session replayed alone or under a candidate
//!   policy, from a data directory, a log file or a runtime (`macp replay`).
//! - [`tail`]: a session's accepted envelopes over a passive `StreamSession`
//!   (`macp session tail`).
//!
//! `macp conform` is `macp_conform::cli`, unchanged.

pub mod jcs;
pub mod receipt;
pub mod replay;
pub mod stack;
pub mod tail;
