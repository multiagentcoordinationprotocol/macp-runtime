//! The session **anchor**: the span context of the accepted SessionStart's
//! `process` span (`docs/opentelemetry.md`, "Correlation across a session").
//!
//! Every later span of the session (its other `process` spans and its
//! transition spans, the root `expire` included) links to it, so a trace
//! backend can gather every trace that touched a session from the anchor.
//!
//! The anchor is **not session state**. It lives in this side cache owned by
//! the `Runtime`, never in `Session`, `PersistedSession` (which is also the
//! checkpoint body, so it would enter the log and replay), a `LogEntry` or an
//! Envelope. It is never hashed and never replayed. P4 keeps it in memory
//! only; [`encode`]/[`decode`] are the durable form P5 persists.
//!
//! Bounded by the registry: entries are inserted only for a session the
//! runtime just accepted (so resident), looked up only for resident sessions
//! (`Runtime::trace_anchor`), and forgotten when the session leaves the
//! registry (memory eviction, disk GC, and a prune of anything no longer
//! resident on every eviction pass). A client sending envelopes with
//! arbitrary `session_id`s therefore cannot grow it.
//!
//! The lock is a `std::sync::RwLock`, held only for a map operation and never
//! across an `.await`.

use std::collections::HashMap;
use std::sync::RwLock;

use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

/// The format version of the durable anchor record.
#[cfg_attr(not(test), allow(dead_code))] // P5 persists the anchor
pub(crate) const RECORD_VERSION: u64 = 1;

/// Session id → anchor span context.
#[derive(Debug, Default)]
pub(crate) struct AnchorCache {
    map: RwLock<HashMap<String, SpanContext>>,
}

impl AnchorCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The anchor of `session_id`, if one was recorded.
    pub(crate) fn get(&self, session_id: &str) -> Option<SpanContext> {
        self.read().get(session_id).cloned()
    }

    /// Record `cx` as the anchor of `session_id`. An invalid context is
    /// ignored. A sampled-out context (flags `00`) is still valid and kept.
    pub(crate) fn put(&self, session_id: &str, cx: SpanContext) {
        if cx.is_valid() {
            self.write().insert(session_id.to_owned(), cx);
        }
    }

    /// Forget the anchor of `session_id`.
    pub(crate) fn forget(&self, session_id: &str) {
        self.write().remove(session_id);
    }

    /// Keep only the anchors whose session id satisfies `keep`.
    pub(crate) fn retain(&self, mut keep: impl FnMut(&str) -> bool) {
        self.write().retain(|id, _| keep(id));
    }

    /// The number of anchors held.
    pub(crate) fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no anchor is held.
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, SpanContext>> {
        self.map.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, SpanContext>> {
        self.map.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// The durable anchor record: `{"v":1,"traceparent":"00-<trace>-<span>-<flags>"}`.
#[cfg_attr(not(test), allow(dead_code))] // P5 persists the anchor
pub(crate) fn encode(cx: &SpanContext) -> Vec<u8> {
    // Written by hand so the key order is fixed whatever serde_json's
    // features; the `traceparent` is hex and dashes only, so no escaping.
    format!(
        r#"{{"v":{RECORD_VERSION},"traceparent":"{}"}}"#,
        traceparent(cx)
    )
    .into_bytes()
}

/// Parse a record written by [`encode`]. `None` for anything else: another
/// version, a malformed or all-zero `traceparent`, or not JSON at all.
#[cfg_attr(not(test), allow(dead_code))] // P5 persists the anchor
pub(crate) fn decode(bytes: &[u8]) -> Option<SpanContext> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("v")?.as_u64()? != RECORD_VERSION {
        return None;
    }
    parse_traceparent(value.get("traceparent")?.as_str()?)
}

/// The W3C `traceparent` (version `00`) of `cx`.
#[cfg_attr(not(test), allow(dead_code))] // P5 persists the anchor
pub(crate) fn traceparent(cx: &SpanContext) -> String {
    format!(
        "00-{}-{}-{:02x}",
        cx.trace_id(),
        cx.span_id(),
        cx.trace_flags().to_u8()
    )
}

/// Parse a version-`00` W3C `traceparent` strictly: lowercase hex, exact
/// lengths, non-zero ids. The result is a remote context.
#[cfg_attr(not(test), allow(dead_code))] // P5 persists the anchor
pub(crate) fn parse_traceparent(s: &str) -> Option<SpanContext> {
    let mut parts = s.split('-');
    let (version, trace, span, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || version != "00" {
        return None;
    }
    let hex = |p: &str, len: usize| {
        p.len() == len
            && p.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if !hex(trace, 32) || !hex(span, 16) || !hex(flags, 2) {
        return None;
    }
    let cx = SpanContext::new(
        TraceId::from_hex(trace).ok()?,
        SpanId::from_hex(span).ok()?,
        TraceFlags::new(u8::from_str_radix(flags, 16).ok()?),
        true,
        TraceState::default(),
    );
    cx.is_valid().then_some(cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(flags: u8) -> SpanContext {
        SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::new(flags),
            false,
            TraceState::default(),
        )
    }

    #[test]
    fn record_round_trips_and_has_the_planned_shape() {
        let bytes = encode(&cx(1));
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"v":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#
        );
        let back = decode(&bytes).unwrap();
        assert_eq!(back.trace_id(), cx(1).trace_id());
        assert_eq!(back.span_id(), cx(1).span_id());
        assert_eq!(back.trace_flags(), TraceFlags::SAMPLED);
        // A sampled-out anchor survives too.
        assert_eq!(
            decode(&encode(&cx(0))).unwrap().trace_flags(),
            TraceFlags::default()
        );
    }

    #[test]
    fn decode_refuses_anything_else() {
        let tp = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        for bad in [
            "".to_string(),
            "not json".into(),
            format!(r#"{{"v":2,"traceparent":"{tp}"}}"#),
            format!(r#"{{"traceparent":"{tp}"}}"#),
            r#"{"v":1}"#.into(),
            r#"{"v":1,"traceparent":7}"#.into(),
            r#"{"v":1,"traceparent":"01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#
                .into(),
            r#"{"v":1,"traceparent":"00-00000000000000000000000000000000-00f067aa0ba902b7-01"}"#
                .into(),
            r#"{"v":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01"}"#
                .into(),
            r#"{"v":1,"traceparent":"00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01"}"#
                .into(),
            r#"{"v":1,"traceparent":"00-+bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#
                .into(),
            r#"{"v":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-x"}"#
                .into(),
        ] {
            assert!(decode(bad.as_bytes()).is_none(), "{bad}");
        }
    }

    #[test]
    fn cache_put_get_forget_retain() {
        let c = AnchorCache::new();
        assert!(c.is_empty());
        c.put("s1", cx(1));
        c.put("s2", cx(0));
        c.put("s3", SpanContext::empty_context()); // invalid: ignored
        assert_eq!(c.len(), 2);
        assert_eq!(c.get("s1").unwrap().span_id(), cx(1).span_id());
        assert!(c.get("s3").is_none());
        c.forget("s1");
        assert!(c.get("s1").is_none());
        c.retain(|id| id != "s2");
        assert!(c.is_empty());
    }
}
