//! The `process` span and the Envelope metrics (`span.macp.process`,
//! `macp.envelope.processed`, `macp.envelope.process.duration`).
//!
//! One [`ProcessSpan`] covers one received Envelope, "from receipt to the
//! accept/reject decision": it starts in the server as soon as the Envelope
//! exists (before shape validation, authentication and the kernel) and
//! finishes when the decision is known. It is an INTERNAL span named
//! `process {mode}` ([`super::labels::ModeLabel::process_span_name`]), a
//! child of the gRPC SERVER span on `Send` and of the stream-open SERVER span
//! on `StreamSession`.
//!
//! Recorded: identifiers and types only, per the spec's privacy defaults.
//! Never a payload, a `reason`, an extension value, or `MacpError`'s message:
//! a rejection records the registry code as `error.type` and leaves the
//! status UNSET; only an implementation failure (`StorageFailed`, or a span
//! dropped unfinished by a panic or a cancelled future) sets ERROR, with the
//! code as the description. Every client-supplied string is truncated to
//! [`MAX_ATTRIBUTE_BYTES`] on a char boundary.
//!
//! The metrics are recorded on every finished Envelope whether or not
//! telemetry is active (the Prometheus rendering is always on); the span is
//! [`Span::none`] when it is not. Nothing here touches the Envelope, the
//! session or its history, so acceptance and replay are unaffected.

use std::time::Instant;

use tracing::field::Empty;
use tracing::Span;

use super::labels::EnvelopeLabels;
use super::{Telemetry, TARGET};
use crate::error::MacpError;
use crate::metrics::RuntimeMetrics;
use crate::pb::Envelope;
use crate::session::Session;

/// OpenTelemetry's token for a value outside the known set (spec #239).
pub const OTHER: &str = "_OTHER";

/// The `macp.operation.name` of the span, and its `tracing` name.
pub const OPERATION: &str = "process";

/// Upper bound on a client-supplied string attribute, in bytes.
pub const MAX_ATTRIBUTE_BYTES: usize = 256;

/// The `otel.status_description` of an implementation failure.
const INTERNAL_ERROR: &str = "INTERNAL_ERROR";

/// `s` cut to at most [`MAX_ATTRIBUTE_BYTES`] bytes, never inside a UTF-8
/// sequence.
pub fn truncate(s: &str) -> &str {
    if s.len() <= MAX_ATTRIBUTE_BYTES {
        return s;
    }
    let mut end = MAX_ATTRIBUTE_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The decision on one Envelope.
#[derive(Clone, Copy, Debug)]
pub enum Outcome<'a> {
    Accepted,
    Duplicate,
    Rejected(&'a MacpError),
}

impl Outcome<'_> {
    /// The `macp.envelope.outcome` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Accepted => "accepted",
            Outcome::Duplicate => "duplicate",
            Outcome::Rejected(_) => "rejected",
        }
    }

    /// The `error.type` value: the registry code of a rejection.
    pub fn error_type(&self) -> Option<&'static str> {
        match self {
            Outcome::Rejected(e) => Some(e.error_code()),
            _ => None,
        }
    }

    /// Whether the span's status is ERROR: only for an implementation
    /// failure, never for a protocol rejection.
    pub fn is_failure(&self) -> bool {
        matches!(self, Outcome::Rejected(MacpError::StorageFailed))
    }
}

/// The `process` span of one Envelope, plus its start instant.
///
/// Call [`ProcessSpan::finish`] on every decision. Dropped unfinished (a
/// panic, a cancelled future), it marks the span ERROR with
/// `INTERNAL_ERROR` and `error.type = _OTHER`, and records no outcome and no
/// metric: the outcome is unknown.
#[derive(Debug)]
pub struct ProcessSpan {
    span: Span,
    started: Instant,
    labels: EnvelopeLabels,
    finished: bool,
}

impl ProcessSpan {
    /// Start the span for `env`. `parent` is the explicit parent (the
    /// stream-open SERVER span); `None` uses the current span (the `Send`
    /// SERVER span). Inactive telemetry gives [`Span::none`].
    pub fn start(
        telemetry: &Telemetry,
        env: &Envelope,
        labels: EnvelopeLabels,
        parent: Option<&Span>,
    ) -> Self {
        let span = if telemetry.is_enabled() {
            make_span(env, &labels, parent)
        } else {
            Span::none()
        };
        Self {
            span,
            started: Instant::now(),
            labels,
            finished: false,
        }
    }

    /// The span, to instrument the work it covers.
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Record the **authenticated** sender (never the Envelope's
    /// self-asserted one).
    pub fn record_sender(&self, sender: &str) {
        if !sender.is_empty() {
            self.span.record("macp.sender.id", truncate(sender));
        }
    }

    /// Link the span to its session's anchor, unless it *is* the anchor
    /// (the accepted SessionStart's own span). One link at most: the caller
    /// links once, just before [`Self::finish`].
    #[cfg(feature = "otel")]
    pub fn link_anchor(&self, anchor: opentelemetry::trace::SpanContext) {
        use opentelemetry::trace::TraceContextExt;
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        if self.span.is_none() {
            return;
        }
        let own = self.span.context().span().span_context().span_id();
        if own != anchor.span_id() {
            self.span.add_link(anchor);
        }
    }

    /// Record the decision on the span and in the metrics.
    pub fn finish(mut self, metrics: &RuntimeMetrics, outcome: Outcome<'_>) {
        self.finished = true;
        self.span.record("macp.envelope.outcome", outcome.as_str());
        if let Some(code) = outcome.error_type() {
            self.span.record("error.type", code);
        }
        if outcome.is_failure() {
            // The description alone sets ERROR with it (tracing-opentelemetry
            // 0.33 `record_str`); the code only, never the message.
            self.span.record("otel.status_description", INTERNAL_ERROR);
        }
        metrics.record_envelope(
            self.labels.mode.metric_value(),
            &self.labels.message_type,
            outcome.as_str(),
            self.started.elapsed().as_secs_f64(),
        );
    }
}

impl Drop for ProcessSpan {
    fn drop(&mut self) {
        if !self.finished {
            // The semconv requires `error.type` on a failed operation; the
            // failure is unknown, so OpenTelemetry's `_OTHER`.
            self.span.record("error.type", OTHER);
            self.span.record("otel.status_description", INTERNAL_ERROR);
        }
    }
}

fn make_span(env: &Envelope, labels: &EnvelopeLabels, parent: Option<&Span>) -> Span {
    let parent = match parent {
        Some(p) => p.id(),
        None => Span::current().id(),
    };
    let name = labels.mode.process_span_name();
    let span = tracing::info_span!(
        target: TARGET,
        parent: parent,
        "process",
        otel.name = name.as_str(),
        otel.kind = "internal",
        otel.status_code = Empty,
        otel.status_description = Empty,
        macp.operation.name = OPERATION,
        macp.message.id = truncate(&env.message_id),
        macp.message.type = truncate(&env.message_type),
        macp.version = Empty,
        macp.session.id = Empty,
        macp.mode.id = Empty,
        macp.sender.id = Empty,
        macp.mode.version = Empty,
        macp.configuration.version = Empty,
        macp.policy.id = Empty,
        macp.envelope.outcome = Empty,
        error.type = Empty,
    );
    record_non_empty(&span, "macp.version", &env.macp_version);
    record_non_empty(&span, "macp.session.id", &env.session_id);
    // As received (truncated), even when the name says `_OTHER`.
    record_non_empty(&span, "macp.mode.id", &env.mode);
    span
}

fn record_non_empty(span: &Span, field: &'static str, value: &str) {
    if !value.is_empty() {
        span.record(field, truncate(value));
    }
}

/// Record the session-bound versions and policy on `span` when it is a
/// `process` span: `macp.mode.version`, `macp.configuration.version` and
/// `macp.policy.id` (the resolved policy's id, else the session's non-empty
/// `policy_version`). A no-op on any other span, including a disabled one.
pub fn record_session_attrs(span: &Span, session: &Session) {
    let is_process = span
        .metadata()
        .is_some_and(|m| m.target() == TARGET && m.name() == OPERATION);
    if !is_process {
        return;
    }
    record_non_empty(span, "macp.mode.version", &session.mode_version);
    record_non_empty(
        span,
        "macp.configuration.version",
        &session.configuration_version,
    );
    let policy = session
        .policy_definition
        .as_ref()
        .map(|d| d.policy_id.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or(&session.policy_version);
    record_non_empty(span, "macp.policy.id", policy);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings_and_never_splits_utf8() {
        assert_eq!(truncate("abc"), "abc");
        let exact = "a".repeat(MAX_ATTRIBUTE_BYTES);
        assert_eq!(truncate(&exact), exact);
        let long = "b".repeat(MAX_ATTRIBUTE_BYTES + 10);
        assert_eq!(truncate(&long).len(), MAX_ATTRIBUTE_BYTES);
        // 255 ASCII bytes then a 3-byte char straddling the limit.
        let straddle = format!("{}\u{20ac}tail", "c".repeat(MAX_ATTRIBUTE_BYTES - 1));
        let cut = truncate(&straddle);
        assert_eq!(cut.len(), MAX_ATTRIBUTE_BYTES - 1);
        assert!(cut.chars().all(|c| c == 'c'));
        // Multi-byte all the way: the cut lands on a boundary.
        let wide = "\u{1F600}".repeat(100);
        let cut = truncate(&wide);
        assert!(cut.len() <= MAX_ATTRIBUTE_BYTES && cut.len().is_multiple_of(4));
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }

    #[test]
    fn outcome_mapping() {
        assert_eq!(Outcome::Accepted.as_str(), "accepted");
        assert_eq!(Outcome::Duplicate.as_str(), "duplicate");
        let forbidden = MacpError::Forbidden;
        let r = Outcome::Rejected(&forbidden);
        assert_eq!(r.as_str(), "rejected");
        assert_eq!(r.error_type(), Some("FORBIDDEN"));
        assert!(!r.is_failure(), "a rejection is not a failure");
        assert_eq!(Outcome::Accepted.error_type(), None);
        assert_eq!(Outcome::Duplicate.error_type(), None);
        let storage = MacpError::StorageFailed;
        let s = Outcome::Rejected(&storage);
        assert_eq!(s.error_type(), Some("INTERNAL_ERROR"));
        assert!(s.is_failure());
    }

    fn envelope() -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.decision.v1".into(),
            message_type: "Vote".into(),
            message_id: "m1".into(),
            session_id: "s1".into(),
            ..Default::default()
        }
    }

    fn labels() -> EnvelopeLabels {
        EnvelopeLabels::new(
            "macp.mode.decision.v1",
            "Vote",
            Some(&["Vote".into()].into()),
        )
    }

    /// Inactive telemetry gives a disabled span, and still records the
    /// metrics (the Prometheus rendering is always on).
    #[test]
    fn inactive_span_is_none_and_still_counts() {
        let metrics = RuntimeMetrics::new();
        let p = ProcessSpan::start(&Telemetry::disabled(), &envelope(), labels(), None);
        assert!(p.span().is_none());
        p.finish(&metrics, Outcome::Accepted);
        let mut out = String::new();
        metrics.semconv_prometheus_lines(&mut out);
        assert!(out.contains(
            "macp_envelope_processed_total{macp_mode_id=\"macp.mode.decision.v1\",\
             macp_message_type=\"Vote\",macp_envelope_outcome=\"accepted\"} 1"
        ));
    }

    /// Dropped unfinished: no metric point.
    #[test]
    fn unfinished_span_records_no_metric() {
        let metrics = RuntimeMetrics::new();
        drop(ProcessSpan::start(
            &Telemetry::disabled(),
            &envelope(),
            labels(),
            None,
        ));
        let mut out = String::new();
        metrics.semconv_prometheus_lines(&mut out);
        assert!(out.is_empty(), "{out}");
    }
}
