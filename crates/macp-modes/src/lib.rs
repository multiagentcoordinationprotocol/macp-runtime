//! `macp-modes` — the MACP coordination mode implementations.
//!
//! Contains the [`mode`] layer (the `Mode` trait and the decision, proposal,
//! task, handoff, quorum, multi_round, and passthrough modes) and the
//! [`mode_registry`]. Modes evaluate governance through an injected
//! [`macp_core::PolicyEvaluator`]; this crate has no dependency on any concrete
//! policy engine, so a consumer can drive MACP coordination with its own
//! evaluator. Transport-free — no tonic, storage, or async runtime beyond the
//! registry's change-notification channel.

pub mod mode;
pub mod mode_registry;
pub mod step;
