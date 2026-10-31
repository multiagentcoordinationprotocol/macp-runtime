//! The session transition spans (`span.macp.resolve`, `.expire`, `.cancel`,
//! `.suspend`, `.resume`).
//!
//! One INTERNAL span per transition, named `{op} {mode}` (`{op} _OTHER` when
//! the session's mode is no longer registered, spec #239), carrying the
//! session and mode ids and the `SESSION_STATE_*` entered. `resolve` also
//! carries the recommended `macp.commitment.hash`, versions and policy.
//!
//! Parenting follows the spec: `expire` is always a **root** (no request is
//! behind it, even when a request observed it lazily); the others are
//! children of the current span (the resolving `process` span, or the
//! cancel/suspend/resume RPC's SERVER span). The caller links the span to the
//! session anchor.
//!
//! Identifiers and types only: never a `reason`, payload or resolution body.
//! Inactive telemetry gives [`Span::none`].

use tracing::field::Empty;
use tracing::Span;

use super::labels::ModeLabel;
use super::process::truncate;
use super::{Telemetry, TARGET};
use crate::session::{Session, SessionState};

/// A session transition (`macp.operation.name`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransitionOp {
    Resolve,
    Expire,
    Cancel,
    Suspend,
    Resume,
}

impl TransitionOp {
    /// The `macp.operation.name` value.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TransitionOp::Resolve => "resolve",
            TransitionOp::Expire => "expire",
            TransitionOp::Cancel => "cancel",
            TransitionOp::Suspend => "suspend",
            TransitionOp::Resume => "resume",
        }
    }

    /// Whether the span is a root: `expire` only.
    pub(crate) fn is_root(self) -> bool {
        self == TransitionOp::Expire
    }
}

/// The `SESSION_STATE_*` wire string of `state` (`macp.session.state`).
pub(crate) fn state_str(state: &SessionState) -> &'static str {
    use crate::pb::SessionState as Pb;
    match state {
        SessionState::Open => Pb::Open.as_str_name(),
        SessionState::Suspended => Pb::Suspended.as_str_name(),
        SessionState::Resolved => Pb::Resolved.as_str_name(),
        SessionState::Expired => Pb::Expired.as_str_name(),
        SessionState::Cancelled => Pb::Cancelled.as_str_name(),
    }
}

/// The span name: `{op} {mode}`, or `{op} _OTHER` for an unregistered mode.
pub(crate) fn span_name(op: TransitionOp, mode: &ModeLabel) -> String {
    match mode.metric_value() {
        Some(m) => format!("{} {m}", op.as_str()),
        // A session always has a mode; treat an empty one as out-of-set.
        None => format!("{} {}", op.as_str(), super::process::OTHER),
    }
}

/// Create the transition span for `session`, which has just entered its
/// current state. `commitment_hash` is recorded on `resolve` only.
pub(crate) fn transition_span(
    telemetry: &Telemetry,
    op: TransitionOp,
    session: &Session,
    mode: &ModeLabel,
    commitment_hash: Option<&str>,
) -> Span {
    if !telemetry.is_enabled() {
        return Span::none();
    }
    let parent = if op.is_root() {
        None
    } else {
        Span::current().id()
    };
    let name = span_name(op, mode);
    let span = tracing::info_span!(
        target: TARGET,
        parent: parent,
        "transition",
        otel.name = name.as_str(),
        otel.kind = "internal",
        macp.operation.name = op.as_str(),
        macp.session.id = truncate(&session.session_id),
        macp.mode.id = truncate(&session.mode),
        macp.session.state = state_str(&session.state),
        macp.commitment.hash = Empty,
        macp.mode.version = Empty,
        macp.configuration.version = Empty,
        macp.policy.id = Empty,
    );
    if op == TransitionOp::Resolve {
        if let Some(hash) = commitment_hash {
            span.record("macp.commitment.hash", hash);
        }
        record_non_empty(&span, "macp.mode.version", &session.mode_version);
        record_non_empty(
            &span,
            "macp.configuration.version",
            &session.configuration_version,
        );
        let policy = session
            .policy_definition
            .as_ref()
            .map(|d| d.policy_id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or(&session.policy_version);
        record_non_empty(&span, "macp.policy.id", policy);
    }
    span
}

fn record_non_empty(span: &Span, field: &'static str, value: &str) {
    if !value.is_empty() {
        span.record(field, truncate(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_and_roots() {
        let all = [
            (TransitionOp::Resolve, "resolve"),
            (TransitionOp::Expire, "expire"),
            (TransitionOp::Cancel, "cancel"),
            (TransitionOp::Suspend, "suspend"),
            (TransitionOp::Resume, "resume"),
        ];
        for (op, name) in all {
            assert_eq!(op.as_str(), name);
            assert_eq!(op.is_root(), op == TransitionOp::Expire);
        }
    }

    #[test]
    fn states_use_the_wire_strings() {
        assert_eq!(state_str(&SessionState::Open), "SESSION_STATE_OPEN");
        assert_eq!(
            state_str(&SessionState::Suspended),
            "SESSION_STATE_SUSPENDED"
        );
        assert_eq!(state_str(&SessionState::Resolved), "SESSION_STATE_RESOLVED");
        assert_eq!(state_str(&SessionState::Expired), "SESSION_STATE_EXPIRED");
        assert_eq!(
            state_str(&SessionState::Cancelled),
            "SESSION_STATE_CANCELLED"
        );
    }

    #[test]
    fn names_are_bounded() {
        let d = ModeLabel::Registered("macp.mode.decision.v1".into());
        assert_eq!(
            span_name(TransitionOp::Cancel, &d),
            "cancel macp.mode.decision.v1"
        );
        assert_eq!(
            span_name(TransitionOp::Expire, &ModeLabel::Other),
            "expire _OTHER"
        );
        assert_eq!(
            span_name(TransitionOp::Resolve, &ModeLabel::Empty),
            "resolve _OTHER"
        );
    }

    /// Inactive telemetry creates no span.
    #[test]
    fn inactive_is_none() {
        let s = Session::builder("s1", "macp.mode.decision.v1", "agent://a").build();
        let span = transition_span(
            &Telemetry::disabled(),
            TransitionOp::Expire,
            &s,
            &ModeLabel::Other,
            None,
        );
        assert!(span.is_none());
    }
}
