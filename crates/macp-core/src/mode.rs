//! The result a coordination mode hands back to the kernel.
//!
//! The `Mode` *trait* itself lives in `macp-modes` (behavior), but this enum
//! (data) lives in core because [`crate::session::Session::apply_mode_response`]
//! consumes it — keeping it here avoids a `macp-core -> macp-modes` cycle.

/// The result of a Mode processing a message.
/// The runtime applies this response to mutate session state.
/// `#[non_exhaustive]`: downstream wildcard arms must treat unknown responses
/// as no-ops or rejections, never as resolutions.
#[non_exhaustive]
#[derive(Debug)]
pub enum ModeResponse {
    /// No state change needed.
    NoOp,
    /// Persist updated mode state.
    PersistState(Vec<u8>),
    /// Resolve the session with the given resolution data.
    Resolve(Vec<u8>),
    /// Persist mode state and resolve in one step.
    PersistAndResolve { state: Vec<u8>, resolution: Vec<u8> },
}

/// Kernel-supplied context accompanying an accepted-for-processing message.
///
/// `accepted_at_ms` is the runtime's acceptance timestamp — the same value
/// recorded as the log entry's `received_at_ms`, so live processing and
/// replay observe the identical clock. Modes that need a trustworthy time
/// source (e.g. Handoff's implicit-accept timeout) must use this, never the
/// client-supplied `Envelope.timestamp_unix_ms`, which the sender can forge.
///
/// `#[non_exhaustive]`: construct via [`MessageContext::new`] so fields can be
/// added without breaking mode implementations.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct MessageContext {
    pub accepted_at_ms: i64,
}

impl MessageContext {
    pub fn new(accepted_at_ms: i64) -> Self {
        Self { accepted_at_ms }
    }
}
