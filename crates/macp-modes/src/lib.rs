//! `macp-modes` — the MACP coordination mode implementations.
//!
//! Contains the [`mode`] layer (the `Mode` trait and the decision, proposal,
//! task, handoff, quorum, multi_round, and passthrough modes) and the
//! [`mode_registry`]. Modes evaluate governance through an injected
//! [`macp_core::PolicyEvaluator`]; this crate has no dependency on any concrete
//! policy engine, so a consumer can drive MACP coordination with its own
//! evaluator. Transport-free — no tonic, storage, or async runtime beyond the
//! registry's change-notification channel.
//!
//! An embedder drives sessions without the server: [`step`] is the
//! acceptance path (SessionStart, per-message admission, the synthetic
//! implicit accept, expiry) and [`replay`] rebuilds a session from what a
//! durable embedder recorded — both the same code the `macp-runtime` kernel
//! runs.

pub mod mode;
pub mod mode_registry;
pub mod replay;
pub mod step;
