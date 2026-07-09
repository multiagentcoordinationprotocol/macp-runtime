use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

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

pub struct RuntimeMetrics {
    per_mode: RwLock<HashMap<String, Arc<ModeMetrics>>>,
    /// Replay/snapshot divergences observed during startup recovery (D7).
    replay_mismatches: AtomicU64,
}

impl RuntimeMetrics {
    pub fn new() -> Self {
        Self {
            per_mode: RwLock::new(HashMap::new()),
            replay_mismatches: AtomicU64::new(0),
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

    pub fn record_replay_mismatch(&self, count: u64) {
        self.replay_mismatches.fetch_add(count, Ordering::Relaxed);
    }

    pub fn replay_mismatches(&self) -> u64 {
        self.replay_mismatches.load(Ordering::Relaxed)
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
        assert!(
            value.parse::<u64>().is_ok(),
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
}
