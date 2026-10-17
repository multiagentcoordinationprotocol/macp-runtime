//! The acceptance clock (RFC-MACP-0001 §7.6 item 4, RFC-MACP-0010 §5.1(1)).
//!
//! Every time-dependent coordination rule -- TTL, the suspension cap, the
//! handoff implicit-accept deadline -- is measured on the clock at which the
//! runtime *accepts* a message, never on a client-supplied
//! `Envelope.timestamp_unix_ms`. The library step API takes that clock as an
//! explicit `now_ms` argument; a long-lived host such as `macp-runtime`'s
//! `Runtime` reads it through this trait instead, so a deployment, a test
//! harness or a conformance runner can inject it.
//!
//! Implementations MUST be monotonic non-decreasing across calls made for one
//! session: the kernel stamps accepted history with the values read here, and
//! replay re-derives every deadline from those stamps.

use std::sync::atomic::{AtomicI64, Ordering};

/// A source of acceptance time, in Unix milliseconds.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now_ms(&self) -> i64;
}

/// Wall-clock time. The production default.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to. For tests, conformance runners and
/// deterministic simulation; never for production.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(now_ms: i64) -> Self {
        Self(AtomicI64::new(now_ms))
    }

    /// Set the clock. Moving it backwards breaks the monotonicity contract
    /// above; that is the caller's responsibility.
    pub fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: i64) {
        self.0.fetch_add(delta_ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let c = ManualClock::new(1_000);
        assert_eq!(c.now_ms(), 1_000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1_500);
        c.set(2_500);
        assert_eq!(c.now_ms(), 2_500);
    }

    #[test]
    fn system_clock_is_wall_time() {
        let before = chrono_free_now();
        let now = SystemClock.now_ms();
        assert!(now >= before && now - before < 60_000);
    }

    fn chrono_free_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
}
