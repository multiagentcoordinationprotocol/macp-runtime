use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use crate::telemetry::process::OTHER;

pub struct ModeMetrics {
    pub messages_accepted: AtomicU64,
    pub messages_rejected: AtomicU64,
    pub sessions_started: AtomicU64,
    pub sessions_resolved: AtomicU64,
    pub sessions_expired: AtomicU64,
    pub sessions_cancelled: AtomicU64,
    pub sessions_suspended: AtomicU64,
    pub sessions_resumed: AtomicU64,
    pub commitments_accepted: AtomicU64,
    pub commitments_rejected: AtomicU64,
}

impl ModeMetrics {
    pub fn new() -> Self {
        Self {
            messages_accepted: AtomicU64::new(0),
            messages_rejected: AtomicU64::new(0),
            sessions_started: AtomicU64::new(0),
            sessions_resolved: AtomicU64::new(0),
            sessions_expired: AtomicU64::new(0),
            sessions_cancelled: AtomicU64::new(0),
            sessions_suspended: AtomicU64::new(0),
            sessions_resumed: AtomicU64::new(0),
            commitments_accepted: AtomicU64::new(0),
            commitments_rejected: AtomicU64::new(0),
        }
    }
}

impl Default for ModeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of distinct mode names tracked in metrics.
/// Beyond this limit, metrics are aggregated into an "_overflow" bucket.
const MAX_MODE_CARDINALITY: usize = 1000;
const OVERFLOW_MODE: &str = "_overflow";

/// Bucket boundaries, in seconds, of `macp.envelope.process.duration`: the
/// same for the OTLP histogram and the Prometheus rendering.
pub const ENVELOPE_DURATION_BOUNDARIES: [f64; 14] = [
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Backstop on distinct `(mode, message type, outcome)` label sets of the
/// Envelope metrics. The `_OTHER` normalization (spec #239) already bounds
/// them by registered modes x their declared types x 3 outcomes; this cap
/// only matters because extension modes register at runtime (up to
/// `MAX_MODE_CARDINALITY`). Beyond it a new set folds into
/// `("_OTHER", "_OTHER", outcome)`.
pub const MAX_ENVELOPE_LABEL_SETS: usize = 2_000;

/// The label set of one Envelope-metrics series, already normalized by the
/// caller (`telemetry::labels`): `mode` is `None` for an empty mode.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct EnvelopeKey {
    mode: Option<String>,
    message_type: String,
    outcome: &'static str,
}

/// One Envelope-metrics series. The counter `macp.envelope.processed` is the
/// histogram's count: both are recorded together, once per Envelope.
#[derive(Default)]
struct EnvelopeSeries {
    count: AtomicU64,
    sum_nanos: AtomicU64,
    /// Non-cumulative per-boundary counts; the last slot is `+Inf`.
    buckets: [AtomicU64; ENVELOPE_DURATION_BOUNDARIES.len() + 1],
}

pub struct RuntimeMetrics {
    per_mode: RwLock<HashMap<String, Arc<ModeMetrics>>>,
    /// `macp.envelope.processed` / `macp.envelope.process.duration`.
    envelopes: RwLock<HashMap<EnvelopeKey, Arc<EnvelopeSeries>>>,
    /// The OpenTelemetry instruments, installed from an active `Telemetry`.
    #[cfg(feature = "otel")]
    instruments: std::sync::OnceLock<Arc<crate::telemetry::instruments::Instruments>>,
    /// Replay/snapshot divergences observed during startup recovery (D7).
    replay_mismatches: AtomicU64,
    /// Decision receipts issued and persisted, signed / unsigned.
    receipts_issued_signed: AtomicU64,
    receipts_issued_unsigned: AtomicU64,
    /// `GetDecisionReceipt` calls that failed in storage or could not build.
    receipt_failures: AtomicU64,
}

impl RuntimeMetrics {
    pub fn new() -> Self {
        Self {
            per_mode: RwLock::new(HashMap::new()),
            envelopes: RwLock::new(HashMap::new()),
            #[cfg(feature = "otel")]
            instruments: std::sync::OnceLock::new(),
            replay_mismatches: AtomicU64::new(0),
            receipts_issued_signed: AtomicU64::new(0),
            receipts_issued_unsigned: AtomicU64::new(0),
            receipt_failures: AtomicU64::new(0),
        }
    }

    pub fn record_session_start(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_started
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_message_accepted(&self, mode: &str) {
        self.get_or_create(mode)
            .messages_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_message_rejected(&self, mode: &str) {
        self.get_or_create(mode)
            .messages_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_resolved(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_resolved
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_expired(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_expired
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_cancelled(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_cancelled
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_suspended(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_suspended
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_session_resumed(&self, mode: &str) {
        self.get_or_create(mode)
            .sessions_resumed
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_commitment_accepted(&self, mode: &str) {
        self.get_or_create(mode)
            .commitments_accepted
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_commitment_rejected(&self, mode: &str) {
        self.get_or_create(mode)
            .commitments_rejected
            .fetch_add(1, Ordering::Relaxed);
    }

    fn get_or_create(&self, mode: &str) -> Arc<ModeMetrics> {
        {
            let guard = self.per_mode.read().unwrap_or_else(|e| e.into_inner());
            if let Some(metrics) = guard.get(mode) {
                return Arc::clone(metrics);
            }
        }

        let mut guard = self.per_mode.write().unwrap_or_else(|e| e.into_inner());
        // If at cardinality limit, aggregate into overflow bucket
        if guard.len() >= MAX_MODE_CARDINALITY && !guard.contains_key(mode) {
            return Arc::clone(
                guard
                    .entry(OVERFLOW_MODE.to_string())
                    .or_insert_with(|| Arc::new(ModeMetrics::default())),
            );
        }
        Arc::clone(
            guard
                .entry(mode.to_string())
                .or_insert_with(|| Arc::new(ModeMetrics::default())),
        )
    }

    /// Install the OpenTelemetry instruments the Envelope metrics also feed.
    /// The first installation wins; later calls are ignored.
    #[cfg(feature = "otel")]
    pub fn install_instruments(
        &self,
        instruments: Arc<crate::telemetry::instruments::Instruments>,
    ) {
        let _ = self.instruments.set(instruments);
    }

    /// Record one processed Envelope: `macp.envelope.processed` and
    /// `macp.envelope.process.duration`, on the Prometheus rendering always
    /// and on the OpenTelemetry instruments when installed.
    ///
    /// The labels must already be normalized (`telemetry::labels`): `mode`
    /// is the registered mode, `_OTHER`, or `None` for an empty mode, and
    /// `message_type` a supported type or `_OTHER`. Never an identifier.
    pub fn record_envelope(
        &self,
        mode: Option<&str>,
        message_type: &str,
        outcome: &'static str,
        seconds: f64,
    ) {
        let key = EnvelopeKey {
            mode: mode.map(str::to_owned),
            message_type: message_type.to_owned(),
            outcome,
        };
        let (key, series) = self.envelope_series(key);
        let seconds = if seconds.is_finite() && seconds > 0.0 {
            seconds
        } else {
            0.0
        };
        series.count.fetch_add(1, Ordering::Relaxed);
        series
            .sum_nanos
            .fetch_add((seconds * 1e9) as u64, Ordering::Relaxed);
        let slot = ENVELOPE_DURATION_BOUNDARIES
            .iter()
            .position(|b| seconds <= *b)
            .unwrap_or(ENVELOPE_DURATION_BOUNDARIES.len());
        series.buckets[slot].fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "otel")]
        if let Some(instruments) = self.instruments.get() {
            instruments.record_envelope(
                key.mode.as_deref(),
                &key.message_type,
                key.outcome,
                seconds,
            );
        }
        #[cfg(not(feature = "otel"))]
        let _ = key;
    }

    /// The series for `key`, folding a new label set beyond
    /// [`MAX_ENVELOPE_LABEL_SETS`] into `("_OTHER", "_OTHER", outcome)`.
    /// Returns the key actually used.
    fn envelope_series(&self, key: EnvelopeKey) -> (EnvelopeKey, Arc<EnvelopeSeries>) {
        {
            let guard = self.envelopes.read().unwrap_or_else(|e| e.into_inner());
            if let Some(series) = guard.get(&key) {
                return (key, Arc::clone(series));
            }
        }
        let mut guard = self.envelopes.write().unwrap_or_else(|e| e.into_inner());
        let key = if guard.len() >= MAX_ENVELOPE_LABEL_SETS && !guard.contains_key(&key) {
            EnvelopeKey {
                mode: Some(OTHER.to_owned()),
                message_type: OTHER.to_owned(),
                outcome: key.outcome,
            }
        } else {
            key
        };
        let series = Arc::clone(guard.entry(key.clone()).or_default());
        (key, series)
    }

    /// The semconv Envelope families in Prometheus text format, following
    /// the OpenTelemetry-to-Prometheus compatibility rules:
    /// `macp_envelope_processed_total` and
    /// `macp_envelope_process_duration_seconds_{bucket,sum,count}`, labelled
    /// `macp_mode_id` (omitted for an empty mode), `macp_message_type` and
    /// `macp_envelope_outcome`. Each family renders contiguously: its
    /// `# HELP` and `# TYPE` lines, then every series in label order. Nothing
    /// is rendered before the first Envelope.
    pub fn semconv_prometheus_lines(&self, out: &mut String) {
        use std::fmt::Write;
        let mut series: Vec<(EnvelopeKey, Arc<EnvelopeSeries>)> = {
            let guard = self.envelopes.read().unwrap_or_else(|e| e.into_inner());
            guard
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };
        if series.is_empty() {
            return;
        }
        series.sort_by(|a, b| a.0.cmp(&b.0));
        let labels = |k: &EnvelopeKey| -> String {
            let mut l = String::new();
            if let Some(mode) = &k.mode {
                let _ = write!(l, "macp_mode_id=\"{}\",", escape_label(mode));
            }
            let _ = write!(
                l,
                "macp_message_type=\"{}\",macp_envelope_outcome=\"{}\"",
                escape_label(&k.message_type),
                k.outcome
            );
            l
        };

        out.push_str(
            "# HELP macp_envelope_processed_total Envelopes the runtime processed, by outcome.\n\
             # TYPE macp_envelope_processed_total counter\n",
        );
        for (k, s) in &series {
            let _ = writeln!(
                out,
                "macp_envelope_processed_total{{{}}} {}",
                labels(k),
                s.count.load(Ordering::Relaxed)
            );
        }

        out.push_str(
            "# HELP macp_envelope_process_duration_seconds Time the runtime took to process one Envelope.\n\
             # TYPE macp_envelope_process_duration_seconds histogram\n",
        );
        for (k, s) in &series {
            let l = labels(k);
            let mut cumulative = 0u64;
            for (i, bound) in ENVELOPE_DURATION_BOUNDARIES.iter().enumerate() {
                cumulative += s.buckets[i].load(Ordering::Relaxed);
                let _ = writeln!(
                    out,
                    "macp_envelope_process_duration_seconds_bucket{{{l},le=\"{bound}\"}} {cumulative}"
                );
            }
            cumulative += s.buckets[ENVELOPE_DURATION_BOUNDARIES.len()].load(Ordering::Relaxed);
            let _ = writeln!(
                out,
                "macp_envelope_process_duration_seconds_bucket{{{l},le=\"+Inf\"}} {cumulative}"
            );
            let sum = s.sum_nanos.load(Ordering::Relaxed) as f64 / 1e9;
            let _ = writeln!(
                out,
                "macp_envelope_process_duration_seconds_sum{{{l}}} {sum}"
            );
            let _ = writeln!(
                out,
                "macp_envelope_process_duration_seconds_count{{{l}}} {}",
                s.count.load(Ordering::Relaxed)
            );
        }
    }

    pub fn record_replay_mismatch(&self, count: u64) {
        self.replay_mismatches.fetch_add(count, Ordering::Relaxed);
    }

    pub fn replay_mismatches(&self) -> u64 {
        self.replay_mismatches.load(Ordering::Relaxed)
    }

    pub fn record_receipt_issued(&self, signed: bool) {
        let counter = if signed {
            &self.receipts_issued_signed
        } else {
            &self.receipts_issued_unsigned
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_receipt_failure(&self) {
        self.receipt_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// `(signed, unsigned)` receipts issued.
    pub fn receipts_issued(&self) -> (u64, u64) {
        (
            self.receipts_issued_signed.load(Ordering::Relaxed),
            self.receipts_issued_unsigned.load(Ordering::Relaxed),
        )
    }

    pub fn receipt_failures(&self) -> u64 {
        self.receipt_failures.load(Ordering::Relaxed)
    }

    /// The receipt counters as Prometheus text lines.
    pub fn receipt_prometheus_lines(&self, out: &mut String) {
        let (signed, unsigned) = self.receipts_issued();
        out.push_str(&format!(
            "macp_receipts_issued_total{{signed=\"true\"}} {signed}\nmacp_receipts_issued_total{{signed=\"false\"}} {unsigned}\nmacp_receipt_issue_failures_total {}\n",
            self.receipt_failures()
        ));
    }

    pub fn snapshot(&self) -> Vec<(String, MetricsSnapshot)> {
        let guard = self.per_mode.read().unwrap_or_else(|e| e.into_inner());
        guard
            .iter()
            .map(|(mode, m)| {
                (
                    mode.clone(),
                    MetricsSnapshot {
                        messages_accepted: m.messages_accepted.load(Ordering::Relaxed),
                        messages_rejected: m.messages_rejected.load(Ordering::Relaxed),
                        sessions_started: m.sessions_started.load(Ordering::Relaxed),
                        sessions_resolved: m.sessions_resolved.load(Ordering::Relaxed),
                        sessions_expired: m.sessions_expired.load(Ordering::Relaxed),
                        sessions_cancelled: m.sessions_cancelled.load(Ordering::Relaxed),
                        commitments_accepted: m.commitments_accepted.load(Ordering::Relaxed),
                        commitments_rejected: m.commitments_rejected.load(Ordering::Relaxed),
                        sessions_suspended: m.sessions_suspended.load(Ordering::Relaxed),
                        sessions_resumed: m.sessions_resumed.load(Ordering::Relaxed),
                    },
                )
            })
            .collect()
    }
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// A Prometheus label value with `\\`, `"` and newlines escaped.
fn escape_label(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[derive(Debug)]
pub struct MetricsSnapshot {
    pub messages_accepted: u64,
    pub messages_rejected: u64,
    pub sessions_started: u64,
    pub sessions_resolved: u64,
    pub sessions_expired: u64,
    pub sessions_cancelled: u64,
    pub commitments_accepted: u64,
    pub commitments_rejected: u64,
    pub sessions_suspended: u64,
    pub sessions_resumed: u64,
}

impl MetricsSnapshot {
    /// Render this snapshot as Prometheus text-format lines for one mode.
    pub fn prometheus_lines(&self, mode: &str, out: &mut String) {
        use std::fmt::Write;
        let pairs: [(&str, u64); 10] = [
            ("macp_messages_accepted_total", self.messages_accepted),
            ("macp_messages_rejected_total", self.messages_rejected),
            ("macp_sessions_started_total", self.sessions_started),
            ("macp_sessions_resolved_total", self.sessions_resolved),
            ("macp_sessions_expired_total", self.sessions_expired),
            ("macp_sessions_cancelled_total", self.sessions_cancelled),
            ("macp_sessions_suspended_total", self.sessions_suspended),
            ("macp_sessions_resumed_total", self.sessions_resumed),
            ("macp_commitments_accepted_total", self.commitments_accepted),
            ("macp_commitments_rejected_total", self.commitments_rejected),
        ];
        for (name, value) in pairs {
            let _ = writeln!(out, "{name}{{mode=\"{mode}\"}} {value}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_METRIC_NAMES: [&str; 10] = [
        "macp_messages_accepted_total",
        "macp_messages_rejected_total",
        "macp_sessions_started_total",
        "macp_sessions_resolved_total",
        "macp_sessions_expired_total",
        "macp_sessions_cancelled_total",
        "macp_sessions_suspended_total",
        "macp_sessions_resumed_total",
        "macp_commitments_accepted_total",
        "macp_commitments_rejected_total",
    ];

    fn snapshot_map(m: &RuntimeMetrics) -> HashMap<String, MetricsSnapshot> {
        m.snapshot().into_iter().collect()
    }

    /// Assert one Prometheus text-format sample line: either
    /// `name{labels} value` or `name value`, with a numeric value and a
    /// well-formed metric name.
    fn assert_valid_prometheus_line(line: &str) {
        let (series, value) = line
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("sample line must contain a value: {line:?}"));
        // Counters are integers; a histogram's `_sum` is a float.
        assert!(
            value.parse::<u64>().is_ok() || value.parse::<f64>().is_ok_and(f64::is_finite),
            "sample value must be numeric: {line:?}"
        );
        let name = match series.split_once('{') {
            Some((name, labels)) => {
                assert!(labels.ends_with('}'), "label set must be closed: {line:?}");
                name
            }
            None => series,
        };
        assert!(!name.is_empty(), "metric name must be non-empty: {line:?}");
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        assert!(
            first.is_ascii_alphabetic() || first == '_' || first == ':',
            "metric name must not start with a digit: {line:?}"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'),
            "metric name has invalid characters: {line:?}"
        );
    }

    #[test]
    fn counters_record_per_mode_increments() {
        let m = RuntimeMetrics::new();

        m.record_session_start("macp.mode.decision.v1");
        m.record_message_accepted("macp.mode.decision.v1");
        m.record_message_accepted("macp.mode.decision.v1");
        m.record_message_rejected("macp.mode.decision.v1");
        m.record_session_resolved("macp.mode.decision.v1");
        m.record_commitment_accepted("macp.mode.decision.v1");

        m.record_session_start("macp.mode.quorum.v1");
        m.record_session_expired("macp.mode.quorum.v1");
        m.record_session_cancelled("macp.mode.quorum.v1");
        m.record_session_suspended("macp.mode.quorum.v1");
        m.record_session_resumed("macp.mode.quorum.v1");
        m.record_commitment_rejected("macp.mode.quorum.v1");

        let snap = snapshot_map(&m);
        assert_eq!(snap.len(), 2, "one entry per distinct mode");

        let d = &snap["macp.mode.decision.v1"];
        assert_eq!(d.sessions_started, 1);
        assert_eq!(d.messages_accepted, 2);
        assert_eq!(d.messages_rejected, 1);
        assert_eq!(d.sessions_resolved, 1);
        assert_eq!(d.commitments_accepted, 1);
        assert_eq!(d.sessions_expired, 0);
        assert_eq!(d.sessions_cancelled, 0);
        assert_eq!(d.sessions_suspended, 0);
        assert_eq!(d.sessions_resumed, 0);
        assert_eq!(d.commitments_rejected, 0);

        let q = &snap["macp.mode.quorum.v1"];
        assert_eq!(q.sessions_started, 1);
        assert_eq!(q.sessions_expired, 1);
        assert_eq!(q.sessions_cancelled, 1);
        assert_eq!(q.sessions_suspended, 1);
        assert_eq!(q.sessions_resumed, 1);
        assert_eq!(q.commitments_rejected, 1);
        assert_eq!(q.messages_accepted, 0);
        assert_eq!(q.messages_rejected, 0);
        assert_eq!(q.sessions_resolved, 0);
        assert_eq!(q.commitments_accepted, 0);
    }

    #[test]
    fn replay_mismatch_counter_accumulates() {
        let m = RuntimeMetrics::new();
        assert_eq!(m.replay_mismatches(), 0);
        m.record_replay_mismatch(2);
        m.record_replay_mismatch(3);
        assert_eq!(m.replay_mismatches(), 5);
    }

    #[test]
    fn prometheus_rendering_reflects_recorded_counts() {
        let m = RuntimeMetrics::new();
        m.record_message_accepted("macp.mode.decision.v1");
        m.record_message_accepted("macp.mode.decision.v1");
        m.record_message_rejected("macp.mode.decision.v1");
        m.record_session_start("macp.mode.decision.v1");

        let mut body = String::new();
        for (mode, snap) in m.snapshot() {
            snap.prometheus_lines(&mode, &mut body);
        }

        for name in EXPECTED_METRIC_NAMES {
            assert!(
                body.contains(name),
                "rendered output must include {name}:\n{body}"
            );
        }
        assert!(body.contains("macp_messages_accepted_total{mode=\"macp.mode.decision.v1\"} 2"));
        assert!(body.contains("macp_messages_rejected_total{mode=\"macp.mode.decision.v1\"} 1"));
        assert!(body.contains("macp_sessions_started_total{mode=\"macp.mode.decision.v1\"} 1"));
        assert!(body.contains("macp_sessions_resolved_total{mode=\"macp.mode.decision.v1\"} 0"));
    }

    #[test]
    fn prometheus_lines_are_valid_exposition_format() {
        let m = RuntimeMetrics::new();
        m.record_session_start("macp.mode.task.v1");
        m.record_commitment_accepted("macp.mode.task.v1");
        m.record_session_start("ext.multi_round.v1");

        let mut body = String::new();
        for (mode, snap) in m.snapshot() {
            snap.prometheus_lines(&mode, &mut body);
        }

        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines.len(),
            2 * EXPECTED_METRIC_NAMES.len(),
            "10 samples per mode"
        );
        for line in lines {
            assert_valid_prometheus_line(line);
        }
    }

    #[test]
    fn zero_state_rendering_does_not_panic() {
        // A metrics registry with no recorded activity snapshots to nothing.
        let m = RuntimeMetrics::new();
        assert!(m.snapshot().is_empty());

        // An all-zero snapshot renders every sample with value 0.
        let zero = MetricsSnapshot {
            messages_accepted: 0,
            messages_rejected: 0,
            sessions_started: 0,
            sessions_resolved: 0,
            sessions_expired: 0,
            sessions_cancelled: 0,
            commitments_accepted: 0,
            commitments_rejected: 0,
            sessions_suspended: 0,
            sessions_resumed: 0,
        };
        let mut body = String::new();
        zero.prometheus_lines("macp.mode.decision.v1", &mut body);
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), EXPECTED_METRIC_NAMES.len());
        for line in lines {
            assert!(
                line.ends_with(" 0"),
                "zero-state sample must be 0: {line:?}"
            );
            assert_valid_prometheus_line(line);
        }
    }

    #[test]
    fn mode_cardinality_overflow_aggregates_into_overflow_bucket() {
        let m = RuntimeMetrics::new();
        for i in 0..MAX_MODE_CARDINALITY {
            m.record_message_accepted(&format!("mode-{i}"));
        }
        // Beyond the cardinality limit, new modes fold into "_overflow".
        m.record_message_accepted("mode-beyond-limit-a");
        m.record_message_accepted("mode-beyond-limit-b");
        // Existing modes keep their own bucket.
        m.record_message_accepted("mode-0");

        let snap = snapshot_map(&m);
        assert_eq!(snap.len(), MAX_MODE_CARDINALITY + 1);
        assert!(!snap.contains_key("mode-beyond-limit-a"));
        assert_eq!(snap[OVERFLOW_MODE].messages_accepted, 2);
        assert_eq!(snap["mode-0"].messages_accepted, 2);
    }

    /// The legacy rendering for `m`, as `/metrics` builds it.
    fn legacy_lines(m: &RuntimeMetrics) -> String {
        let mut snaps = m.snapshot();
        snaps.sort_by(|a, b| a.0.cmp(&b.0));
        let mut body = String::new();
        for (mode, snap) in snaps {
            snap.prometheus_lines(&mode, &mut body);
        }
        body
    }

    /// Two modes, two message types, every outcome, plus an ambient Signal.
    fn record_traffic(m: &RuntimeMetrics) {
        const D: &str = "macp.mode.decision.v1";
        const Q: &str = "macp.mode.quorum.v1";
        m.record_envelope(Some(D), "SessionStart", "accepted", 0.0003);
        m.record_envelope(Some(D), "Vote", "accepted", 0.002);
        m.record_envelope(Some(D), "Vote", "rejected", 0.02);
        m.record_envelope(Some(Q), "SessionStart", "accepted", 0.3);
        m.record_envelope(Some(Q), "Vote", "duplicate", 20.0);
        m.record_envelope(None, "Signal", "accepted", 0.0001);
        m.record_envelope(Some(OTHER), OTHER, "rejected", 0.0001);
    }

    /// AC 7 (P3): the semconv families are valid exposition lines, each
    /// family is contiguous, and the legacy lines are untouched.
    #[test]
    fn semconv_prometheus_families_are_valid_contiguous_and_leave_legacy_lines_alone() {
        let m = RuntimeMetrics::new();
        m.record_session_start("macp.mode.decision.v1");
        m.record_message_accepted("macp.mode.decision.v1");
        m.record_message_rejected("macp.mode.quorum.v1");
        let legacy_before = legacy_lines(&m);
        assert!(legacy_before
            .contains("macp_messages_accepted_total{mode=\"macp.mode.decision.v1\"} 1"));

        record_traffic(&m);
        let mut body = String::new();
        m.semconv_prometheus_lines(&mut body);

        // Every sample line is valid; comments are HELP/TYPE only.
        let mut families: Vec<&str> = Vec::new();
        for line in body.lines() {
            if let Some(comment) = line.strip_prefix("# ") {
                assert!(
                    comment.starts_with("HELP ") || comment.starts_with("TYPE "),
                    "{line}"
                );
                let family = comment.split(' ').nth(1).unwrap();
                families.push(family);
                continue;
            }
            assert_valid_prometheus_line(line);
            let name = line.split(['{', ' ']).next().unwrap();
            let family = if name.starts_with("macp_envelope_process_duration_seconds") {
                "macp_envelope_process_duration_seconds"
            } else {
                name
            };
            families.push(family);
        }
        // Contiguity: once a family gives way to another, it never returns.
        let mut seen: Vec<&str> = Vec::new();
        for f in &families {
            if seen.last() != Some(f) {
                assert!(!seen.contains(f), "family {f} renders in two runs:\n{body}");
                seen.push(f);
            }
        }
        assert_eq!(
            seen,
            [
                "macp_envelope_processed_total",
                "macp_envelope_process_duration_seconds"
            ]
        );
        assert!(body.starts_with(
            "# HELP macp_envelope_processed_total Envelopes the runtime processed, by outcome.\n\
             # TYPE macp_envelope_processed_total counter\n"
        ));
        assert!(body.contains("# TYPE macp_envelope_process_duration_seconds histogram\n"));

        // Labels: mode omitted for an empty mode, `_OTHER` kept as a value.
        assert!(body.contains(
            "macp_envelope_processed_total{macp_mode_id=\"macp.mode.decision.v1\",\
             macp_message_type=\"Vote\",macp_envelope_outcome=\"rejected\"} 1\n"
        ));
        assert!(body.contains(
            "macp_envelope_processed_total{macp_message_type=\"Signal\",\
             macp_envelope_outcome=\"accepted\"} 1\n"
        ));
        assert!(body.contains(
            "macp_envelope_processed_total{macp_mode_id=\"_OTHER\",\
             macp_message_type=\"_OTHER\",macp_envelope_outcome=\"rejected\"} 1\n"
        ));
        // Buckets are cumulative and end at +Inf == count.
        let q = "macp_mode_id=\"macp.mode.quorum.v1\",macp_message_type=\"Vote\",\
                 macp_envelope_outcome=\"duplicate\"";
        assert!(body.contains(&format!(
            "macp_envelope_process_duration_seconds_bucket{{{q},le=\"10\"}} 0\n"
        )));
        assert!(body.contains(&format!(
            "macp_envelope_process_duration_seconds_bucket{{{q},le=\"+Inf\"}} 1\n"
        )));
        assert!(body.contains(&format!(
            "macp_envelope_process_duration_seconds_count{{{q}}} 1\n"
        )));
        assert!(body.contains(&format!(
            "macp_envelope_process_duration_seconds_sum{{{q}}} 20\n"
        )));
        let d = "macp_mode_id=\"macp.mode.decision.v1\",macp_message_type=\"SessionStart\",\
                 macp_envelope_outcome=\"accepted\"";
        assert!(body.contains(&format!(
            "macp_envelope_process_duration_seconds_bucket{{{d},le=\"0.0005\"}} 1\n"
        )));
        // No identifier ever becomes a label.
        for forbidden in ["session", "message_id", "sender"] {
            assert!(!body.contains(forbidden), "{forbidden} in:\n{body}");
        }

        // The legacy lines are byte-identical for the same legacy traffic.
        assert_eq!(legacy_lines(&m), legacy_before);
    }

    #[test]
    fn semconv_rendering_is_empty_before_any_envelope() {
        let mut body = String::new();
        RuntimeMetrics::new().semconv_prometheus_lines(&mut body);
        assert!(body.is_empty(), "{body}");
    }

    /// Beyond the backstop cap, new label sets fold into
    /// `("_OTHER", "_OTHER", outcome)`; existing ones keep counting.
    #[test]
    fn envelope_label_sets_fold_into_other_beyond_the_backstop_cap() {
        let m = RuntimeMetrics::new();
        for i in 0..MAX_ENVELOPE_LABEL_SETS {
            m.record_envelope(
                Some(&format!("ext.m{i}.v1")),
                "SessionStart",
                "accepted",
                0.001,
            );
        }
        m.record_envelope(Some("ext.beyond.v1"), "SessionStart", "accepted", 0.001);
        m.record_envelope(Some("ext.beyond.v2"), "Ping", "accepted", 0.001);
        m.record_envelope(Some("ext.beyond.v2"), "Ping", "rejected", 0.001);
        m.record_envelope(Some("ext.m0.v1"), "SessionStart", "accepted", 0.001);
        let mut body = String::new();
        m.semconv_prometheus_lines(&mut body);
        assert!(
            !body.contains("ext.beyond"),
            "a set beyond the cap was kept"
        );
        assert!(body.contains(
            "macp_envelope_processed_total{macp_mode_id=\"_OTHER\",\
             macp_message_type=\"_OTHER\",macp_envelope_outcome=\"accepted\"} 2\n"
        ));
        // A fold set beyond the cap is itself one more set, at most one per
        // outcome.
        assert!(body.contains(
            "macp_envelope_processed_total{macp_mode_id=\"_OTHER\",\
             macp_message_type=\"_OTHER\",macp_envelope_outcome=\"rejected\"} 1\n"
        ));
        assert!(body.contains(
            "macp_envelope_processed_total{macp_mode_id=\"ext.m0.v1\",\
             macp_message_type=\"SessionStart\",macp_envelope_outcome=\"accepted\"} 2\n"
        ));
        let samples = body
            .lines()
            .filter(|l| l.starts_with("macp_envelope_processed_total{"))
            .count();
        assert_eq!(samples, MAX_ENVELOPE_LABEL_SETS + 2);
    }

    #[test]
    fn label_values_are_escaped() {
        assert_eq!(escape_label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
