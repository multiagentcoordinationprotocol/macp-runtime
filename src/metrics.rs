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
}

impl RuntimeMetrics {
    pub fn new() -> Self {
        Self {
            per_mode: RwLock::new(HashMap::new()),
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
}
