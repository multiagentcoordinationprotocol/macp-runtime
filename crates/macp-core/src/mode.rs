//! The result a coordination mode hands back to the kernel.
//!
//! The `Mode` *trait* itself lives in `macp-modes` (behavior), but this enum
//! (data) lives in core because [`crate::session::Session::apply_mode_response`]
//! consumes it — keeping it here avoids a `macp-core -> macp-modes` cycle.

/// The result of a Mode processing a message.
/// The runtime applies this response to mutate session state.
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
