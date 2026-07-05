//! Pluggable ingress policy engine (E3, master plan §4.6).
//!
//! This is the identity-aware, async authorization surface for external
//! engines (OPA, Cedar, org-specific services). It is deliberately distinct
//! from [`macp_core::policy::PolicyEvaluator`]:
//!
//! - `PolicyEvaluator` governs **commitment evaluation** and must be a pure,
//!   deterministic function of bound rules + accepted history (RFC-MACP-0012
//!   §6.3) — it replays.
//! - `PolicyEngine` governs **ingress**: whether an authenticated identity may
//!   start a session, send a message, or observe a session. Rejected traffic
//!   never enters accepted history, so replay only ever sees engine-approved
//!   envelopes — an async, non-deterministic external engine here cannot
//!   diverge replay, by the same reasoning that keeps authentication outside
//!   the replay boundary (RFC-MACP-0003).
//!
//! Failure semantics are **deny-on-error**: an engine that cannot answer is a
//! denial, never an allow.

use crate::security::AuthIdentity;
use macp_core::policy::PolicyDecision;
use macp_core::session::Session;
use macp_pb::pb::Envelope;

/// Decision points an external engine may govern at ingress.
#[async_trait::async_trait]
pub trait PolicyEngine: Send + Sync {
    /// May `identity` start a session in `mode`? Runs after authentication
    /// and the security layer's own checks, before the kernel accepts the
    /// SessionStart.
    async fn evaluate_session_start(
        &self,
        identity: &AuthIdentity,
        mode: &str,
        env: &Envelope,
    ) -> PolicyDecision;

    /// May `identity` send this session-scoped envelope? Runs after mode
    /// binding is known, before kernel acceptance.
    async fn evaluate_message(
        &self,
        identity: &AuthIdentity,
        session: &Session,
        env: &Envelope,
    ) -> PolicyDecision;

    /// May `identity` observe this session (GetSession / StreamSession
    /// subscribe)? Purely a read gate; never replayed.
    async fn evaluate_session_access(
        &self,
        identity: &AuthIdentity,
        session: &Session,
    ) -> PolicyDecision;
}

/// Convert an engine decision into a transport error, fail closed.
pub fn require_allow(decision: PolicyDecision, what: &str) -> Result<(), tonic::Status> {
    match decision {
        PolicyDecision::Allow { .. } => Ok(()),
        PolicyDecision::Deny { reasons } => Err(tonic::Status::permission_denied(format!(
            "policy engine denied {what}: {}",
            reasons.join("; ")
        ))),
        other => Err(tonic::Status::permission_denied(format!(
            "policy engine returned unrecognized decision for {what} (fail closed): {other:?}"
        ))),
    }
}
