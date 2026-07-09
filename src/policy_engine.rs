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

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    /// Minimal inline engine returning a fixed decision at every point.
    struct FixedEngine {
        decision: PolicyDecision,
    }

    #[async_trait::async_trait]
    impl PolicyEngine for FixedEngine {
        async fn evaluate_session_start(
            &self,
            _identity: &AuthIdentity,
            _mode: &str,
            _env: &Envelope,
        ) -> PolicyDecision {
            self.decision.clone()
        }

        async fn evaluate_message(
            &self,
            _identity: &AuthIdentity,
            _session: &Session,
            _env: &Envelope,
        ) -> PolicyDecision {
            self.decision.clone()
        }

        async fn evaluate_session_access(
            &self,
            _identity: &AuthIdentity,
            _session: &Session,
        ) -> PolicyDecision {
            self.decision.clone()
        }
    }

    fn identity(sender: &str) -> AuthIdentity {
        AuthIdentity {
            sender: sender.into(),
            allowed_modes: None,
            can_start_sessions: true,
            max_open_sessions: None,
            can_manage_mode_registry: false,
            is_observer: false,
        }
    }

    fn session() -> Session {
        Session::builder("s1", "macp.mode.decision.v1", "agent-a").build()
    }

    #[test]
    fn require_allow_maps_allow_to_ok() {
        assert!(require_allow(PolicyDecision::Allow { reasons: vec![] }, "send").is_ok());
        // Reasons on an Allow are advisory and must not affect the outcome.
        assert!(require_allow(
            PolicyDecision::Allow {
                reasons: vec!["matched rule r1".into()]
            },
            "session start",
        )
        .is_ok());
    }

    #[test]
    fn require_allow_maps_deny_to_permission_denied_with_reasons() {
        let err = require_allow(
            PolicyDecision::Deny {
                reasons: vec!["sender not on roster".into(), "mode locked".into()],
            },
            "session start",
        )
        .expect_err("deny must map to an error");
        assert_eq!(err.code(), Code::PermissionDenied);
        assert!(err.message().contains("policy engine denied session start"));
        assert!(
            err.message().contains("sender not on roster; mode locked"),
            "deny reasons must be joined into the message: {}",
            err.message()
        );
    }

    #[test]
    fn require_allow_deny_with_no_reasons_still_denies() {
        let err = require_allow(PolicyDecision::Deny { reasons: vec![] }, "message")
            .expect_err("deny must map to an error even without reasons");
        assert_eq!(err.code(), Code::PermissionDenied);
        assert!(err.message().contains("policy engine denied message"));
    }

    #[tokio::test]
    async fn allow_engine_decisions_pass_all_ingress_points() {
        let engine = FixedEngine {
            decision: PolicyDecision::Allow { reasons: vec![] },
        };
        let id = identity("agent-a");
        let sess = session();
        let env = Envelope::default();

        let d = engine
            .evaluate_session_start(&id, "macp.mode.decision.v1", &env)
            .await;
        assert!(require_allow(d, "session start").is_ok());
        let d = engine.evaluate_message(&id, &sess, &env).await;
        assert!(require_allow(d, "send").is_ok());
        let d = engine.evaluate_session_access(&id, &sess).await;
        assert!(require_allow(d, "session access").is_ok());
    }

    #[tokio::test]
    async fn deny_engine_decisions_fail_closed_at_all_ingress_points() {
        let engine = FixedEngine {
            decision: PolicyDecision::Deny {
                reasons: vec!["external engine said no".into()],
            },
        };
        let id = identity("agent-a");
        let sess = session();
        let env = Envelope::default();

        let d = engine
            .evaluate_session_start(&id, "macp.mode.decision.v1", &env)
            .await;
        let err = require_allow(d, "session start").expect_err("must deny");
        assert_eq!(err.code(), Code::PermissionDenied);

        let d = engine.evaluate_message(&id, &sess, &env).await;
        let err = require_allow(d, "send").expect_err("must deny");
        assert_eq!(err.code(), Code::PermissionDenied);
        assert!(err.message().contains("external engine said no"));

        let d = engine.evaluate_session_access(&id, &sess).await;
        let err = require_allow(d, "session access").expect_err("must deny");
        assert_eq!(err.code(), Code::PermissionDenied);
    }
}
