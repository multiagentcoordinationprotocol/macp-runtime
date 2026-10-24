//! The [`ConformanceTarget`] trait: what the runner needs from an
//! implementation under test.
//!
//! The runner owns the fixture logic (building envelopes, minting ids, the
//! expectations); a target only submits envelopes and reports what it can
//! observe. A target that cannot observe something says so through
//! [`Capabilities`] and [`Observation`], and the runner records the matching
//! assertion as *unverified* rather than passing or failing it.
//!
//! Two targets ship: [`crate::InProcessTarget`] (the Embedded profile, over
//! `macp_modes::step` and `macp_modes::replay`) and, with the `grpc` feature,
//! `GrpcTarget` (the Server profile, over any `MACPRuntimeService`). Implement
//! the trait to run the fixtures against anything else, for example your own
//! runtime embedding the crates with durable storage.

use async_trait::async_trait;
use macp_core::PolicyDefinition;
use macp_pb::pb::{CommitmentPayload, Envelope};
use serde::Serialize;

/// How the implementation is driven (spec `docs/conformance.md`, Profiles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Over the network, through `MACPRuntimeService`.
    Server,
    /// In-process, through the implementation's acceptance and replay API.
    Embedded,
}

/// What a target can do beyond submitting envelopes and reading state.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[non_exhaustive]
pub struct Capabilities {
    /// [`ConformanceTarget::set_clock`] sets the acceptance clock. Fixtures
    /// with `accepted_at_ms` are skipped (`needs-clock-control`) without it.
    pub clock_control: bool,
    /// [`ConformanceTarget::check_replay`] is implemented.
    pub replay: bool,
    /// [`ConformanceTarget::commitment_hash`] and
    /// [`ConformanceTarget::verify_commitment_hash`] are implemented.
    pub commitment_hash: bool,
}

impl Capabilities {
    pub fn with_clock_control(mut self, on: bool) -> Self {
        self.clock_control = on;
        self
    }
    pub fn with_replay(mut self, on: bool) -> Self {
        self.replay = on;
        self
    }
    pub fn with_commitment_hash(mut self, on: bool) -> Self {
        self.commitment_hash = on;
        self
    }
}

/// The target's answer to one submitted envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    Accepted {
        duplicate: bool,
    },
    /// Rejected with this MACP error code (e.g. `INVALID_ENVELOPE`).
    Rejected {
        code: String,
    },
}

/// Something the target may or may not be able to see.
#[derive(Debug, Clone, PartialEq)]
pub enum Observable<T> {
    Observed(T),
    /// The target has no way to read it (the runner marks the assertion
    /// unverified).
    Unobservable,
}

impl<T> Observable<T> {
    pub fn observed(&self) -> Option<&T> {
        match self {
            Observable::Observed(v) => Some(v),
            Observable::Unobservable => None,
        }
    }
}

/// One accepted-history entry after the `SessionStart`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct HistoryEntry {
    pub message_type: String,
    pub sender: String,
    pub message_id: String,
    pub payload: Vec<u8>,
}

impl HistoryEntry {
    pub fn new(message_type: String, sender: String, message_id: String, payload: Vec<u8>) -> Self {
        Self {
            message_type,
            sender,
            message_id,
            payload,
        }
    }

    pub fn from_envelope(env: &Envelope) -> Self {
        Self::new(
            env.message_type.clone(),
            env.sender.clone(),
            env.message_id.clone(),
            env.payload.clone(),
        )
    }
}

/// The session as the target sees it after the fixture's last message.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Observation {
    /// `Open`, `Resolved`, `Expired`, `Suspended` or `Cancelled` (the
    /// fixture `expected_final_state` vocabulary).
    pub state: String,
    /// The resolution bytes (a `CommitmentPayload`), `None` when unresolved.
    pub resolution: Observable<Option<Vec<u8>>>,
    /// The mode state as JSON, `None` when the mode keeps none.
    pub mode_state: Observable<Option<serde_json::Value>>,
    /// Accepted history after the `SessionStart`, synthetic entries
    /// included, in order.
    pub history: Observable<Vec<HistoryEntry>>,
}

impl Observation {
    pub fn new(state: impl Into<String>) -> Self {
        Self {
            state: state.into(),
            resolution: Observable::Unobservable,
            mode_state: Observable::Unobservable,
            history: Observable::Unobservable,
        }
    }
    pub fn with_resolution(mut self, r: Option<Vec<u8>>) -> Self {
        self.resolution = Observable::Observed(r);
        self
    }
    pub fn with_mode_state(mut self, m: Option<serde_json::Value>) -> Self {
        self.mode_state = Observable::Observed(m);
        self
    }
    pub fn with_history(mut self, h: Vec<HistoryEntry>) -> Self {
        self.history = Observable::Observed(h);
        self
    }
}

/// The target itself failed (transport down, an internal error): not a
/// conformance verdict. The runner reports the fixture as `error`.
#[derive(Debug, Clone)]
pub struct TargetError(pub String);

impl std::fmt::Display for TargetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TargetError {}

/// An implementation under test. See the module docs.
///
/// The runner calls, per fixture: [`begin_fixture`](Self::begin_fixture),
/// [`register_policy`](Self::register_policy) when the fixture has one, then
/// per message [`set_clock`](Self::set_clock) (clocked fixtures only) and
/// [`submit`](Self::submit), then [`observe`](Self::observe) and, when the
/// fixture asks for it, [`check_replay`](Self::check_replay).
#[async_trait]
pub trait ConformanceTarget: Send {
    fn profile(&self) -> Profile;

    fn capabilities(&self) -> Capabilities;

    /// A one-line description for the report (`in-process macp-modes 0.9.0`,
    /// `grpc://127.0.0.1:50051`).
    fn describe(&self) -> String;

    /// The semantics revision new sessions are accepted at, when the target
    /// knows it. An Embedded claim SHOULD state it (RFC-MACP-0003 §3.2).
    fn semantics_rev(&self) -> Option<u32> {
        None
    }

    /// Start a fixture. `clock_start` is `Some(0)` for a clocked fixture (the
    /// target's acceptance clock must read it from here on). A target that
    /// can isolate fixtures (a fresh registry) does so here.
    async fn begin_fixture(&mut self, clock_start: Option<i64>) -> Result<(), TargetError>;

    /// Make `definition` resolvable by its `policy_id` for the fixture's
    /// sessions, replacing any earlier definition with that id.
    async fn register_policy(&mut self, definition: PolicyDefinition) -> Result<(), TargetError>;

    /// Set the acceptance clock. Only called with
    /// [`Capabilities::clock_control`]; values are non-decreasing.
    async fn set_clock(&mut self, _now_ms: i64) -> Result<(), TargetError> {
        Err(TargetError("clock control is not supported".into()))
    }

    /// Submit one envelope, `SessionStart` included.
    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError>;

    /// Read the session after the fixture's messages. `initiator` is the
    /// fixture's initiator, for targets that read as a participant.
    async fn observe(
        &mut self,
        session_id: &str,
        initiator: &str,
    ) -> Result<Observation, TargetError>;

    /// Rebuild the session from its recorded history and compare it with the
    /// live one (state, resolution, mode state, dedup set). `Ok(Err(..))` is
    /// a replay mismatch, a conformance failure. Only called with
    /// [`Capabilities::replay`].
    async fn check_replay(&mut self, _session_id: &str) -> Result<Result<(), String>, TargetError> {
        Err(TargetError("replay is not supported".into()))
    }

    /// RFC-MACP-0013 commitment hash of `payload`. Only called with
    /// [`Capabilities::commitment_hash`].
    fn commitment_hash(&self, _payload: &CommitmentPayload) -> Option<String> {
        None
    }

    /// Verify `claimed` against `wire` (a `CommitmentPayload` encoding):
    /// `match`, `mismatch` or `cannot_verify`. Only called with
    /// [`Capabilities::commitment_hash`].
    fn verify_commitment_hash(&self, _wire: &[u8], _claimed: &str) -> Option<String> {
        None
    }
}
