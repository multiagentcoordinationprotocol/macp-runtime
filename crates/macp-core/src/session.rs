use crate::error::MacpError;
use crate::mode::ModeResponse;
use crate::policy::PolicyDefinition;
use macp_pb::pb::SessionStartPayload;
use prost::Message;
use std::collections::{HashMap, HashSet};

pub const MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Default cap on the cumulative time a session may spend `Suspended` before
/// it is force-expired (RFC-MACP-0001 §7.5). Bounds indefinite
/// human-in-the-loop holds. Sessions may bind their own cap via
/// `SessionStartPayload.max_suspend_ms` (0 selects this default); the
/// resolved cap is recorded at SessionStart and used by replay
/// (RFC-MACP-0003 §2) — see [`Session::effective_max_suspend_ms`].
pub const MAX_SUSPEND_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Cap on the number of completed suspend/resume *cycles* a session may
/// record in [`Session::suspension_intervals`]. Enforced in
/// [`Session::resume`] at **every** revision, but with revision-dependent
/// behavior (see below), so legacy histories replay bit-identically.
///
/// Why a *count* cap is needed even though [`MAX_SUSPEND_MS`] exists:
/// `MAX_SUSPEND_MS` bounds accumulated suspended *duration* (7 days by
/// default), not the number of cycles — N one-millisecond suspend/resume
/// cycles accrue ~0 against that budget, and there is no other cycle counter
/// anywhere in the session model. `SuspendSession`/`ResumeSession` are also
/// un-rate-limited RPCs (unlike `Send`), so the cycle count is attacker- (or
/// bug-) controlled by the session initiator alone.
///
/// What the cap closes: each cycle writes two full `PersistedSession`
/// snapshots, and `suspension_intervals` is part of that snapshot — so an
/// unbounded vec turns two constant-size writes per cycle into O(N) writes,
/// i.e. O(N²) total snapshot bytes over a session's life. Bounding the vec
/// bounds the amplification — and the vec must be bounded at *every*
/// revision, because a session replayed from a legacy log loads at rev 0/1
/// and can still be suspended and resumed through the un-rate-limited
/// `SuspendSession`/`ResumeSession` RPCs.
///
/// Two behaviors, split on the revision:
///
/// - **`semantics_rev >= 2`** — over the cap, `resume` takes the posture it
///   already takes for `MAX_SUSPEND_MS`: force-expire the session and return
///   [`MacpError::TtlExpired`].
/// - **`semantics_rev <= 1`** — `resume` keeps succeeding but simply **stops
///   recording** once the vec reaches the cap. Nothing reads
///   `suspension_intervals` below rev 2 ([`Session::unsuspended_deadline`] is
///   a rev >= 2 path), so dropping the overflow keeps legacy replay
///   bit-identical while still bounding memory and snapshot size. A legacy
///   session is never force-expired by a rule that did not exist when it was
///   accepted.
pub const MAX_SUSPENSION_CYCLES: usize = 1024;

/// Current session-semantics revision. Recorded at SessionStart (on the
/// session and its log entry) and consulted wherever acceptance-time behavior
/// changed across releases, so legacy histories replay under the semantics
/// they were accepted with (RFC-MACP-0003 §1).
///
/// Revisions:
/// - 0 — legacy: Handoff implicit-accept timed against the client-supplied
///   envelope timestamp.
/// - 1 — Handoff implicit-accept times against the runtime acceptance clock
///   (`MessageContext::accepted_at_ms`).
/// - 2 — suspension-corrected Handoff implicit-accept deadline
///   (RFC-MACP-0010 §5.1(1)): time the session spends `Suspended` no longer
///   counts toward `implicit_accept_timeout_ms`. The offer record snapshots
///   `accumulated_suspended_ms` at offer time and the timeout arithmetic
///   subtracts the suspension accrued since the offer. Revisions 0 and 1 keep
///   counting suspended time, so their histories replay to the outcome they
///   were accepted with.
pub const CURRENT_SEMANTICS_REV: u32 = 2;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionState {
    Open,
    /// Non-terminal pause of an `Open` session (RFC-MACP-0001 §7.5). TTL is
    /// banked while suspended; only `Open`<->`Suspended` and `Suspended`->
    /// `Expired`/`Cancelled` transitions are permitted.
    Suspended,
    Resolved,
    Expired,
    /// Terminal: ended by an accepted `CancelSession` (distinct from `Expired`).
    Cancelled,
}

impl SessionState {
    /// Terminal states accept no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            SessionState::Resolved | SessionState::Expired | SessionState::Cancelled
        )
    }
}

/// Session model. Fields are public for reads, but the struct is
/// `#[non_exhaustive]`: construct via [`Session::builder`]. This lets the model
/// gain fields without breaking every constructor in downstream crates
/// (pre-1.0 freeze requirement).
#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Session {
    pub session_id: String,
    pub state: SessionState,
    pub ttl_expiry: i64,
    pub ttl_ms: i64,
    pub started_at_unix_ms: i64,
    pub resolution: Option<Vec<u8>>,
    pub mode: String,
    pub mode_state: Vec<u8>,
    pub participants: Vec<String>,
    pub seen_message_ids: HashSet<String>,
    pub intent: String,
    pub mode_version: String,
    pub configuration_version: String,
    pub policy_version: String,
    pub context_id: String,
    pub extensions: HashMap<String, Vec<u8>>,
    pub roots: Vec<macp_pb::pb::Root>,
    pub initiator_sender: String,
    pub participant_message_counts: HashMap<String, u32>,
    pub participant_last_seen: HashMap<String, i64>,
    pub policy_definition: Option<PolicyDefinition>,
    /// Wall-clock (session-timeline) ms at which the session was suspended, or
    /// `None` when not suspended. Used to bank TTL across a suspension (§7.5).
    pub suspended_at_ms: Option<i64>,
    /// Cumulative ms the session has spent suspended across all suspend/resume
    /// cycles. Drives the `MAX_SUSPEND_MS` cap.
    pub accumulated_suspended_ms: i64,
    /// Completed `(suspended_at, resumed_at)` pairs on the session timeline
    /// (ms), in the order they completed. An *in-progress* suspension is
    /// deliberately **not** here — the pair is pushed by [`Session::resume`],
    /// so the vec always describes finished pauses only.
    ///
    /// Recorded at **every** `semantics_rev` (so a session that later matters
    /// has the history), but read only at rev >= 2, by
    /// [`Session::unsuspended_deadline`] (RFC-MACP-0010 §5.1). Legacy
    /// snapshots and checkpoints deserialize this as empty; see
    /// `unsuspended_deadline`'s under-count invariant for why that is safe.
    ///
    /// Bounded at every revision by [`MAX_SUSPENSION_CYCLES`] — rev >= 2
    /// force-expires the session over the cap, rev <= 1 stops recording.
    pub suspension_intervals: Vec<(i64, i64)>,
    /// Session-semantics revision this session was accepted under. See
    /// [`CURRENT_SEMANTICS_REV`]. Legacy persisted sessions load as `0`.
    pub semantics_rev: u32,
    /// Maximum-suspension cap bound at SessionStart (RFC-MACP-0001 §7.5,
    /// RFC-MACP-0003 §2). `0` = unbound (legacy sessions and library
    /// defaults) — the [`MAX_SUSPEND_MS`] default applies; see
    /// [`Session::effective_max_suspend_ms`]. The kernel records the
    /// *resolved* value here for new sessions.
    pub max_suspend_ms: i64,
}

impl Session {
    /// Start building a `Session`. The three arguments are the fields with no
    /// meaningful default; everything else starts from documented defaults
    /// (see [`SessionBuilder`]) and is set with the builder's methods.
    pub fn builder(
        session_id: impl Into<String>,
        mode: impl Into<String>,
        initiator_sender: impl Into<String>,
    ) -> SessionBuilder {
        SessionBuilder {
            inner: Session {
                session_id: session_id.into(),
                state: SessionState::Open,
                // Never-expires by default: every kernel path overrides this
                // from the validated SessionStart payload; library/test
                // consumers get a session that behaves until told otherwise.
                ttl_expiry: i64::MAX,
                ttl_ms: 0,
                started_at_unix_ms: 0,
                resolution: None,
                mode: mode.into(),
                mode_state: vec![],
                participants: vec![],
                seen_message_ids: HashSet::new(),
                intent: String::new(),
                mode_version: String::new(),
                configuration_version: String::new(),
                policy_version: String::new(),
                context_id: String::new(),
                extensions: HashMap::new(),
                roots: vec![],
                initiator_sender: initiator_sender.into(),
                participant_message_counts: HashMap::new(),
                participant_last_seen: HashMap::new(),
                policy_definition: None,
                suspended_at_ms: None,
                accumulated_suspended_ms: 0,
                suspension_intervals: vec![],
                semantics_rev: CURRENT_SEMANTICS_REV,
                max_suspend_ms: 0,
            },
        }
    }

    pub fn record_participant_activity(&mut self, sender: &str, timestamp_ms: i64) {
        *self
            .participant_message_counts
            .entry(sender.to_string())
            .or_insert(0) += 1;
        self.participant_last_seen
            .insert(sender.to_string(), timestamp_ms);
    }

    /// Suspend an `Open` session (RFC-MACP-0001 §7.5). Records the suspend time
    /// so TTL can be banked on resume. Pure: no clock, no I/O — the caller
    /// injects `now_ms`.
    pub fn suspend(&mut self, now_ms: i64) -> Result<(), MacpError> {
        if self.state != SessionState::Open {
            return Err(MacpError::SessionNotOpen);
        }
        self.state = SessionState::Suspended;
        self.suspended_at_ms = Some(now_ms);
        Ok(())
    }

    /// The suspension cap governing this session: the value bound at
    /// SessionStart, or the [`MAX_SUSPEND_MS`] default when unbound (0).
    pub fn effective_max_suspend_ms(&self) -> i64 {
        if self.max_suspend_ms > 0 {
            self.max_suspend_ms
        } else {
            MAX_SUSPEND_MS
        }
    }

    /// Resume a `Suspended` session, banking the suspended duration into the
    /// TTL deadline (`ttl_expiry += now - suspended_at`) and recording the
    /// completed pause in [`Session::suspension_intervals`].
    ///
    /// Force-expires the session (state `Expired`, `Err(TtlExpired)`) when
    /// either suspension cap is exceeded: cumulative duration past
    /// [`MAX_SUSPEND_MS`] (every revision), or completed cycle count past
    /// [`MAX_SUSPENSION_CYCLES`] (`semantics_rev >= 2` only — at rev <= 1 the
    /// cap instead stops the recording, see [`MAX_SUSPENSION_CYCLES`]). The
    /// pair is pushed before either check, so history is recorded even on the
    /// expiring call.
    ///
    /// Pure: no clock, no I/O — the caller injects `now_ms`.
    pub fn resume(&mut self, now_ms: i64) -> Result<(), MacpError> {
        if self.state != SessionState::Suspended {
            return Err(MacpError::SessionNotOpen);
        }
        let suspended_at = self.suspended_at_ms.unwrap_or(now_ms);
        let banked = (now_ms - suspended_at).max(0);
        self.accumulated_suspended_ms = self.accumulated_suspended_ms.saturating_add(banked);
        self.suspended_at_ms = None;
        // Record the completed pair at EVERY revision and BEFORE either cap
        // check can return: the vec is history, not a decision input, and a
        // pause that force-expires the session is still a pause that happened.
        // (Phase-9 precedent: record everywhere, read only under rev >= 2.)
        //
        // The one exception is the rev <= 1 overflow below: a legacy session
        // is reachable through the un-rate-limited Suspend/Resume RPCs just
        // like a current one, so its vec has to be bounded too — but it must
        // not be force-expired by a rule postdating its acceptance. Since
        // nothing reads the vec below rev 2, dropping the overflow bounds
        // memory and snapshot size while leaving legacy replay bit-identical.
        if self.semantics_rev >= 2 || self.suspension_intervals.len() < MAX_SUSPENSION_CYCLES {
            self.suspension_intervals.push((suspended_at, now_ms));
        }
        // Cycle-count cap, rev >= 2 only — see `MAX_SUSPENSION_CYCLES`. Gated
        // on the revision so rev <= 1 histories replay bit-identically.
        if self.semantics_rev >= 2 && self.suspension_intervals.len() > MAX_SUSPENSION_CYCLES {
            self.state = SessionState::Expired;
            return Err(MacpError::TtlExpired);
        }
        if self.accumulated_suspended_ms > self.effective_max_suspend_ms() {
            self.state = SessionState::Expired;
            return Err(MacpError::TtlExpired);
        }
        self.ttl_expiry = self.ttl_expiry.saturating_add(banked);
        self.state = SessionState::Open;
        Ok(())
    }

    /// Cancel an `Open` or `Suspended` session into the terminal `Cancelled`
    /// state (RFC-MACP-0001 §7.3). Returns an error if already terminal.
    pub fn cancel(&mut self) -> Result<(), MacpError> {
        if self.state.is_terminal() {
            return Err(MacpError::SessionNotOpen);
        }
        self.state = SessionState::Cancelled;
        self.suspended_at_ms = None;
        Ok(())
    }

    /// Whether a currently-`Suspended` session has exceeded `MAX_SUSPEND_MS` as
    /// of `now_ms` (cumulative banked plus the in-progress suspension).
    pub fn suspend_cap_exceeded(&self, now_ms: i64) -> bool {
        match self.suspended_at_ms {
            Some(at) => {
                self.accumulated_suspended_ms
                    .saturating_add((now_ms - at).max(0))
                    > self.effective_max_suspend_ms()
            }
            None => self.accumulated_suspended_ms > self.effective_max_suspend_ms(),
        }
    }

    /// The session-timeline instant at which `duration_ms` of **unsuspended**
    /// time has elapsed since `from_ms` — i.e. the earliest `T` with
    /// `(T - from_ms) - suspended_in[from_ms, T] >= duration_ms`.
    ///
    /// This is RFC-MACP-0010 §5.1(3)'s own formula for the synthetic implicit
    /// accept's timestamp ("offer acceptance time + timeout + suspended time
    /// **within the window**"), evaluated on the recorded timeline required by
    /// §5.1(1). It is deliberately not the naive
    /// `from_ms + duration_ms + banked_since(from_ms)`: that counts pauses
    /// that begin *after* the true deadline, so it is wrong whenever a
    /// suspend/resume pair lands between the deadline and the observation —
    /// fully reachable, since `SuspendSession`/`ResumeSession` are RPCs that
    /// need no session-scoped message. It would also make the timestamp
    /// depend on *when* it was computed, which is unacceptable for a value
    /// baked into permanent history.
    ///
    /// The walk: start at `from_ms` with the full `duration_ms` remaining;
    /// for each completed pause `(s, e)` starting at or after `from_ms`, if
    /// the unsuspended run up to `s` already covers what remains, stop inside
    /// that run; otherwise consume it and jump to `e`. A pause starting
    /// exactly at the returned deadline does not extend it — the offer's
    /// unsuspended time had already hit the timeout at that instant.
    ///
    /// Pure and saturating: no clock read, no I/O, no panics on overflow.
    ///
    /// **Under-count invariant.** The walk's contribution satisfies
    /// `walk_sum <= accumulated_suspended_ms - offer.suspended_ms_at_offer`:
    /// [`Session::suspension_intervals`] may *under*-report completed pauses
    /// but can never over-report them. Three distinct sources produce a short
    /// or empty vec, each permanent for the life of that log:
    ///
    /// 1. A snapshot or checkpoint written *before the field existed*
    ///    deserializes it as empty (`#[serde(default)]`) while
    ///    `accumulated_suspended_ms` is already positive.
    /// 2. Any replay — including a post-11b one — that resumes from a
    ///    *pre-11b mid-session checkpoint*: the fast path replays only
    ///    `&log_entries[idx + 1..]`, so every pause that completed before that
    ///    checkpoint is gone and can never be recovered, no matter how many
    ///    times the log is replayed afterwards.
    /// 3. A `semantics_rev <= 1` session that exceeded
    ///    [`MAX_SUSPENSION_CYCLES`]: recording stops rather than
    ///    force-expiring, so pairs past the cap are dropped. This one is
    ///    outside this function's read domain by construction — the walk is
    ///    only consulted at `semantics_rev >= 2`, where the cap force-expires
    ///    instead of dropping — so the enumeration above is exhaustive for
    ///    every vec this function can actually be asked to walk.
    ///
    /// An under-count only moves the returned deadline *earlier*, never later
    /// — the safe direction, so callers may rely on `deadline <= now_ms` once
    /// the scalar arithmetic has already decided the timeout elapsed.
    ///
    /// **Why a short vec cannot break replay determinism.** This describes the
    /// design this function exists to serve; the synthetic entry itself
    /// arrives in a later phase. Replay is never to *recompute* a synthetic
    /// implicit-accept timestamp — it replays the recorded synthetic entry as
    /// data. Only the live emitter computes a deadline, exactly once, at
    /// *emission* time, from the offer's recorded `offered_at_ms`. So a short
    /// vec can make a
    /// *newly* emitted implicit accept land earlier than a fully-recorded one
    /// would have, but it can never make a *replayed* one disagree with the
    /// live value already baked into history — byte-identical replay is not
    /// foreclosed.
    pub fn unsuspended_deadline(&self, from_ms: i64, duration_ms: i64) -> i64 {
        let mut cur = from_ms;
        let mut remaining = duration_ms;
        for &(s, e) in self
            .suspension_intervals
            .iter()
            .filter(|(s, _)| *s >= from_ms)
        {
            // Clamp both the run and the jump. `Utc::now()` is not monotonic
            // (an NTP step back between suspend and resume yields `e < s`),
            // pairs can overlap or nest, and the public
            // `SessionBuilder::suspension_intervals` setter lets a library
            // consumer supply anything at all. Without the clamps a negative
            // `run` would *grow* `remaining` and a backwards `e` would rewind
            // `cur`, i.e. the walk would OVER-report and violate the
            // under-count invariant above.
            let run = s.saturating_sub(cur).max(0);
            if run >= remaining {
                return cur.saturating_add(remaining);
            }
            remaining = remaining.saturating_sub(run);
            // `s` is in the max as well as `e`: the run up to `s` was just
            // consumed, so `cur` must end at or after `s` even when the pair
            // is backwards. Jumping only to `e` there would spend the run
            // without advancing the cursor, returning a deadline *before*
            // `from_ms + duration_ms` — safe against over-reporting, but a
            // timeout that fires before it nominally elapsed is its own bug.
            // A degenerate pair is therefore treated as a zero-width pause.
            cur = e.max(s).max(cur);
        }
        cur.saturating_add(remaining)
    }

    pub fn apply_mode_response(&mut self, response: ModeResponse) {
        match response {
            ModeResponse::NoOp => {}
            ModeResponse::PersistState(state) => self.mode_state = state,
            ModeResponse::Resolve(resolution) => {
                self.state = SessionState::Resolved;
                self.resolution = Some(resolution);
            }
            ModeResponse::PersistAndResolve { state, resolution } => {
                self.mode_state = state;
                self.state = SessionState::Resolved;
                self.resolution = Some(resolution);
            }
        }
    }
}

/// Builder for [`Session`] — the only construction path outside `macp-core`
/// (the struct is `#[non_exhaustive]`).
///
/// Defaults: `state: Open`, `ttl_expiry: i64::MAX` (never expires until set),
/// numeric fields `0`, everything else empty/`None`.
#[derive(Clone, Debug)]
pub struct SessionBuilder {
    inner: Session,
}

macro_rules! builder_setters {
    ($($(#[$doc:meta])* $name:ident: $ty:ty),* $(,)?) => {
        $(
            $(#[$doc])*
            pub fn $name(mut self, value: $ty) -> Self {
                self.inner.$name = value;
                self
            }
        )*
    };
}

impl SessionBuilder {
    builder_setters! {
        state: SessionState,
        ttl_expiry: i64,
        ttl_ms: i64,
        started_at_unix_ms: i64,
        resolution: Option<Vec<u8>>,
        mode_state: Vec<u8>,
        participants: Vec<String>,
        seen_message_ids: HashSet<String>,
        extensions: HashMap<String, Vec<u8>>,
        roots: Vec<macp_pb::pb::Root>,
        participant_message_counts: HashMap<String, u32>,
        participant_last_seen: HashMap<String, i64>,
        policy_definition: Option<crate::policy::PolicyDefinition>,
        suspended_at_ms: Option<i64>,
        accumulated_suspended_ms: i64,
        /// Completed suspend/resume pairs (see
        /// [`Session::suspension_intervals`]). Needed so persistence layers
        /// can restore the field — `Session` is `#[non_exhaustive]`, so they
        /// cannot use a struct literal.
        suspension_intervals: Vec<(i64, i64)>,
        semantics_rev: u32,
        /// Suspension cap bound at SessionStart; 0 = use the
        /// [`MAX_SUSPEND_MS`] default (legacy sessions, library consumers).
        max_suspend_ms: i64,
    }

    pub fn intent(mut self, value: impl Into<String>) -> Self {
        self.inner.intent = value.into();
        self
    }

    pub fn mode_version(mut self, value: impl Into<String>) -> Self {
        self.inner.mode_version = value.into();
        self
    }

    pub fn configuration_version(mut self, value: impl Into<String>) -> Self {
        self.inner.configuration_version = value.into();
        self
    }

    pub fn policy_version(mut self, value: impl Into<String>) -> Self {
        self.inner.policy_version = value.into();
        self
    }

    pub fn context_id(mut self, value: impl Into<String>) -> Self {
        self.inner.context_id = value.into();
        self
    }

    pub fn build(self) -> Session {
        self.inner
    }
}

pub fn requires_strict_session_start(mode: &str) -> bool {
    matches!(
        mode,
        "macp.mode.decision.v1"
            | "macp.mode.proposal.v1"
            | "macp.mode.task.v1"
            | "macp.mode.handoff.v1"
            | "macp.mode.quorum.v1"
            | "ext.multi_round.v1"
    )
}

/// Parse a protobuf-encoded SessionStartPayload from raw bytes.
pub fn parse_session_start_payload(payload: &[u8]) -> Result<SessionStartPayload, MacpError> {
    if payload.is_empty() {
        return Err(MacpError::InvalidPayload);
    }
    SessionStartPayload::decode(payload).map_err(|_| MacpError::InvalidPayload)
}

/// Extract and validate TTL from a parsed SessionStartPayload.
pub fn extract_ttl_ms(payload: &SessionStartPayload) -> Result<i64, MacpError> {
    if !(1..=MAX_TTL_MS).contains(&payload.ttl_ms) {
        return Err(MacpError::InvalidTtl);
    }
    Ok(payload.ttl_ms)
}

/// Modes whose canonical `SessionStart` may bind an **empty** `participants`
/// list.
///
/// Decision alone. RFC-MACP-0001 §7.1 requires `participants` only "when
/// required by the Mode", and RFC-MACP-0007 makes the initiator's authority
/// role-based rather than membership-based, so a Decision session with no
/// declared participants is well-defined: nobody — the initiator included — can
/// emit a `Proposal`, `Evaluation`, `Objection` or `Vote`, because
/// `DecisionMode::authorize_sender` routes all four through
/// `is_declared_participant`, which is `false` over an empty list. Such a
/// session can therefore only expire or be cancelled. Spec #99 removed
/// `minItems: 1` from the conformance fixture schema on exactly that reasoning
/// and added `decision_zero_participants.json` to pin it.
///
/// **Deliberately a positive allowlist of one, checked in one place.** The
/// other four standards-track modes each re-reject an insufficient roster in
/// their own `on_session_start`, but those are five independent
/// implementations: if the rule lived only there, deleting any one guard would
/// silently remove the guarantee with nothing at the core level left to notice.
/// Keeping the rule here means the exception is named once and every other
/// mode — including a promoted extension mode the list below has never heard
/// of — keeps the full canonical contract by default.
fn allows_empty_participants(mode: &str) -> bool {
    mode == "macp.mode.decision.v1"
}

/// Validate the complete canonical SessionStart binding contract.
///
/// Mode-independent, and therefore holds the roster non-emptiness rule for
/// **every** mode. Prefer
/// [`validate_canonical_session_start_payload_for_mode`] on any path that knows
/// the mode name; this entry point is retained with its original signature and
/// its original behaviour.
pub fn validate_canonical_session_start_payload(
    payload: &SessionStartPayload,
) -> Result<(), MacpError> {
    validate_canonical_start(payload, false)
}

/// The canonical SessionStart binding contract, with the roster rule scoped to
/// the mode.
///
/// Identical to [`validate_canonical_session_start_payload`] in every respect
/// except one: an empty `participants` list is accepted for the modes
/// `allows_empty_participants` names (Decision, and only Decision) and
/// rejected for all others, promoted extension modes included.
///
/// This is additive rather than a new parameter on
/// [`validate_canonical_session_start_payload`] on purpose — changing that
/// function's signature would be a `macp-core` API break, and every crate in
/// this workspace shares one version.
pub fn validate_canonical_session_start_payload_for_mode(
    mode: &str,
    payload: &SessionStartPayload,
) -> Result<(), MacpError> {
    validate_canonical_start(payload, allows_empty_participants(mode))
}

fn validate_canonical_start(
    payload: &SessionStartPayload,
    allow_empty_participants: bool,
) -> Result<(), MacpError> {
    extract_ttl_ms(payload)?;

    if payload.mode_version.trim().is_empty() || payload.configuration_version.trim().is_empty() {
        return Err(MacpError::InvalidPayload);
    }

    if payload.participants.is_empty() && !allow_empty_participants {
        return Err(MacpError::InvalidPayload);
    }

    // Safety limit: prevent resource exhaustion from excessively large participant lists.
    const MAX_PARTICIPANTS: usize = 1000;
    if payload.participants.len() > MAX_PARTICIPANTS {
        return Err(MacpError::InvalidPayload);
    }

    let mut seen = HashSet::new();
    for participant in &payload.participants {
        let participant = participant.trim();
        if participant.is_empty() || !seen.insert(participant.to_string()) {
            return Err(MacpError::InvalidPayload);
        }
    }

    // max_suspend_ms: 0 selects the runtime default; a positive value binds a
    // session-specific cap (RFC-MACP-0001 §7.1). Negative is meaningless.
    if payload.max_suspend_ms < 0 {
        return Err(MacpError::InvalidPayload);
    }

    Ok(())
}

/// Enforce the strict SessionStart binding contract for standards-track and qualifying extension modes.
pub fn validate_strict_session_start_payload(
    mode: &str,
    payload: &SessionStartPayload,
) -> Result<(), MacpError> {
    if !requires_strict_session_start(mode) {
        return Ok(());
    }

    validate_canonical_session_start_payload_for_mode(mode, payload)
}

/// Validate that a session ID meets the acceptance policy.
///
/// Accepts:
/// - UUID v4/v7 in hyphenated lowercase canonical form (36 chars)
/// - base64url tokens of 22+ chars (`[A-Za-z0-9_-]`)
///
/// Rejects everything else (empty, short human-readable, uppercase UUID, etc.).
pub fn validate_session_id_for_acceptance(session_id: &str) -> Result<(), MacpError> {
    if session_id.is_empty() {
        return Err(MacpError::InvalidSessionId);
    }

    // UUID-shaped ids (36 chars, parseable) are held to strict UUID rules with no
    // fall-through: canonical lowercase hyphenated form, version v4 or v7. This
    // keeps non-canonical forms (e.g. uppercase) of the same UUID from being
    // admitted as distinct base64url tokens. Only strings that do not parse as a
    // UUID at all fall through to the base64url rule.
    if session_id.len() == 36 && session_id.contains('-') {
        if let Ok(parsed) = uuid::Uuid::parse_str(session_id) {
            if parsed.as_hyphenated().to_string() == session_id {
                match parsed.get_version() {
                    Some(uuid::Version::Random) | Some(uuid::Version::SortRand) => {
                        return Ok(());
                    }
                    _ => {}
                }
            }
            return Err(MacpError::InvalidSessionId);
        }
    }

    // Try base64url: at least 22 chars, only [A-Za-z0-9_-]
    if session_id.len() >= 22
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }

    Err(MacpError::InvalidSessionId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn encode_payload(ttl_ms: i64, participants: Vec<String>) -> Vec<u8> {
        let payload = SessionStartPayload {
            intent: String::new(),
            participants,
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        };
        payload.encode_to_vec()
    }

    #[test]
    fn parse_empty_payload_is_invalid() {
        let err = parse_session_start_payload(b"").unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn parse_valid_protobuf_payload() {
        let bytes = encode_payload(5000, vec!["alice".into(), "bob".into()]);
        let result = parse_session_start_payload(&bytes).unwrap();
        assert_eq!(result.ttl_ms, 5000);
        assert_eq!(result.participants, vec!["alice", "bob"]);
    }

    #[test]
    fn extract_ttl_requires_explicit_positive_value() {
        let payload = SessionStartPayload::default();
        assert_eq!(
            extract_ttl_ms(&payload).unwrap_err().to_string(),
            "InvalidTtl"
        );

        let payload = SessionStartPayload {
            ttl_ms: 5000,
            ..Default::default()
        };
        assert_eq!(extract_ttl_ms(&payload).unwrap(), 5000);
    }

    #[test]
    fn standard_mode_requires_explicit_versions() {
        // Renamed from `standard_mode_requires_explicit_versions_and_participants`
        // and narrowed: the empty-`participants` half moved out, because
        // Decision now accepts an empty roster. The version and TTL halves of
        // the strict contract are kept verbatim so the rest stays pinned; the
        // roster rule is pinned for every other mode by
        // `every_standard_mode_except_decision_rejects_an_empty_roster` below.
        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            mode_version: String::new(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );

        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            mode_version: "1.0.0".into(),
            configuration_version: String::new(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );

        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 0,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidTtl"
        );
    }

    /// Build an otherwise-valid strict payload with an empty roster.
    fn empty_roster_payload() -> SessionStartPayload {
        SessionStartPayload {
            participants: vec![],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn decision_accepts_an_empty_participant_list() {
        assert!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &empty_roster_payload())
                .is_ok(),
            "RFC-MACP-0001 §7.1 requires participants only when the mode does, and \
             RFC-MACP-0007 makes Decision authority role-based; spec #99's \
             decision_zero_participants.json pins the accepted SessionStart"
        );
        // The relaxation must be the *only* thing that moved: the same payload
        // with everything else intact still fails on a missing version.
        let mut broken = empty_roster_payload();
        broken.mode_version = String::new();
        assert!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &broken).is_err(),
            "an empty roster must not waive the rest of the strict contract"
        );
    }

    #[test]
    fn every_standard_mode_except_decision_rejects_an_empty_roster() {
        // The structural guard, and the reason the rule lives here rather than
        // in five `on_session_start` implementations. Iterating the strict-mode
        // list means a mode *added* to it inherits the roster requirement, and
        // a future change that widened the carve-out would have to edit this
        // table to stay green — neither is true of a per-mode guard, which a
        // PR can delete alongside its own test.
        for mode in [
            "macp.mode.proposal.v1",
            "macp.mode.task.v1",
            "macp.mode.handoff.v1",
            "macp.mode.quorum.v1",
            "ext.multi_round.v1",
        ] {
            assert!(
                requires_strict_session_start(mode),
                "{mode} must be strict for this table to mean anything"
            );
            assert_eq!(
                validate_strict_session_start_payload(mode, &empty_roster_payload())
                    .unwrap_err()
                    .to_string(),
                "InvalidPayload",
                "{mode} must still reject an empty participant list at the core layer"
            );
        }

        // And a *promoted* extension mode — a name the static carve-out list
        // has never heard of, which `ModeRegistry::promote_mode` can mark
        // strict at runtime — keeps the full canonical contract. This is the
        // trap the two call sites had to avoid: swapping the strictness source
        // for the core's static list would have dropped canonical validation
        // for exactly these names.
        assert_eq!(
            validate_canonical_session_start_payload_for_mode(
                "ext.promoted.v1",
                &empty_roster_payload()
            )
            .unwrap_err()
            .to_string(),
            "InvalidPayload",
            "an unrecognised (e.g. promoted) mode must default to the strict roster rule"
        );

        // The mode-independent entry point keeps its original behaviour, so a
        // caller that cannot supply a mode name is never silently relaxed.
        assert_eq!(
            validate_canonical_session_start_payload(&empty_roster_payload())
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    fn open_session(ttl_expiry: i64) -> Session {
        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry,
            ttl_ms: 60_000,
            started_at_unix_ms: 0,
            resolution: None,
            mode: "macp.mode.decision.v1".into(),
            mode_state: vec![],
            participants: vec![],
            seen_message_ids: HashSet::new(),
            intent: String::new(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            context_id: String::new(),
            extensions: HashMap::new(),
            roots: vec![],
            initiator_sender: "agent://a".into(),
            participant_message_counts: HashMap::new(),
            participant_last_seen: HashMap::new(),
            policy_definition: None,
            suspended_at_ms: None,
            accumulated_suspended_ms: 0,
            suspension_intervals: vec![],
            semantics_rev: CURRENT_SEMANTICS_REV,
            max_suspend_ms: 0,
        }
    }

    #[test]
    fn suspend_then_resume_banks_ttl() {
        let mut s = open_session(10_000);
        s.suspend(2_000).unwrap();
        assert_eq!(s.state, SessionState::Suspended);
        assert_eq!(s.suspended_at_ms, Some(2_000));
        // Resume 3_000ms later: banked 3_000 is added to the deadline.
        s.resume(5_000).unwrap();
        assert_eq!(s.state, SessionState::Open);
        assert_eq!(s.ttl_expiry, 13_000);
        assert_eq!(s.accumulated_suspended_ms, 3_000);
        assert_eq!(s.suspended_at_ms, None);
    }

    #[test]
    fn suspend_requires_open_and_resume_requires_suspended() {
        let mut s = open_session(10_000);
        // resume on an Open session is rejected
        assert!(matches!(
            s.resume(1).unwrap_err(),
            MacpError::SessionNotOpen
        ));
        s.suspend(1).unwrap();
        // double-suspend rejected
        assert!(matches!(
            s.suspend(2).unwrap_err(),
            MacpError::SessionNotOpen
        ));
    }

    #[test]
    fn resume_exceeding_max_suspend_expires() {
        let mut s = open_session(10_000);
        s.suspend(0).unwrap();
        // Resume after more than MAX_SUSPEND_MS: force-expired.
        let err = s.resume(MAX_SUSPEND_MS + 1).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s.state, SessionState::Expired);
    }

    /// A session-bound cap (SessionStartPayload.max_suspend_ms) overrides the
    /// default: resuming past the BOUND cap force-expires even though the
    /// default cap is nowhere near exceeded.
    #[test]
    fn bound_cap_overrides_default_on_resume() {
        let mut s = open_session(10_000);
        s.max_suspend_ms = 500;
        s.suspend(0).unwrap();
        let err = s.resume(501).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s.state, SessionState::Expired);
    }

    #[test]
    fn bound_cap_within_limit_resumes_and_banks_ttl() {
        let mut s = open_session(10_000);
        s.max_suspend_ms = 500;
        s.suspend(0).unwrap();
        s.resume(400).unwrap();
        assert_eq!(s.state, SessionState::Open);
        assert_eq!(s.ttl_expiry, 10_400);
    }

    /// The cap is cumulative across pauses, not per-pause: two 300ms
    /// suspensions each fit under a 500ms bound cap on their own, but the
    /// second resume sees the 600ms total and force-expires. Pins that
    /// `resume` accumulates `accumulated_suspended_ms` rather than
    /// overwriting it with the latest pause — the invariant the rev-2 handoff
    /// deadline reads (`HandoffMode::rev2_elapsed_ms`).
    #[test]
    fn bound_cap_counts_suspension_cumulatively_across_pauses() {
        let mut s = open_session(10_000);
        s.max_suspend_ms = 500;
        s.suspend(0).unwrap();
        s.resume(300).unwrap();
        assert_eq!(s.accumulated_suspended_ms, 300);
        assert_eq!(s.ttl_expiry, 10_300);
        s.suspend(400).unwrap();
        // 300 + 300 = 600 > the 500ms cap, though neither pause alone is.
        let err = s.resume(700).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s.state, SessionState::Expired);
        assert_eq!(s.accumulated_suspended_ms, 600);
    }

    #[test]
    fn suspend_cap_exceeded_uses_bound_cap() {
        let mut s = open_session(10_000);
        s.max_suspend_ms = 500;
        s.suspend(0).unwrap();
        assert!(!s.suspend_cap_exceeded(400));
        assert!(s.suspend_cap_exceeded(501));
    }

    #[test]
    fn unbound_session_uses_default_cap() {
        let s = open_session(10_000);
        assert_eq!(s.max_suspend_ms, 0);
        assert_eq!(s.effective_max_suspend_ms(), MAX_SUSPEND_MS);
    }

    #[test]
    fn negative_max_suspend_ms_rejected_in_canonical_payload() {
        let payload = SessionStartPayload {
            participants: vec!["a".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            max_suspend_ms: -1,
            ..Default::default()
        };
        assert_eq!(
            validate_canonical_session_start_payload(&payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
        // 0 (runtime default) and positive values are both valid.
        let ok0 = SessionStartPayload {
            max_suspend_ms: 0,
            ..payload.clone()
        };
        validate_canonical_session_start_payload(&ok0).unwrap();
        let ok_pos = SessionStartPayload {
            max_suspend_ms: 60_000,
            ..payload
        };
        validate_canonical_session_start_payload(&ok_pos).unwrap();
    }

    #[test]
    fn cancel_from_open_or_suspended_then_terminal_is_rejected() {
        let mut s = open_session(10_000);
        s.suspend(1).unwrap();
        s.cancel().unwrap();
        assert_eq!(s.state, SessionState::Cancelled);
        assert_eq!(s.suspended_at_ms, None);
        // Already terminal: further cancel is rejected.
        assert!(matches!(s.cancel().unwrap_err(), MacpError::SessionNotOpen));

        let mut open = open_session(10_000);
        open.cancel().unwrap();
        assert_eq!(open.state, SessionState::Cancelled);
    }

    #[test]
    fn standard_mode_rejects_duplicate_participants() {
        let payload = SessionStartPayload {
            participants: vec!["alice".into(), "alice".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.proposal.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn multi_round_requires_strict_session_start() {
        let payload = SessionStartPayload::default();
        assert!(validate_strict_session_start_payload("ext.multi_round.v1", &payload).is_err());
    }

    #[test]
    fn valid_uuid_v4_accepted() {
        let id = uuid::Uuid::new_v4().as_hyphenated().to_string();
        validate_session_id_for_acceptance(&id).unwrap();
    }

    #[test]
    fn valid_base64url_accepted() {
        // 22-char base64url token
        validate_session_id_for_acceptance("abcdefghijklmnopqrstuv").unwrap();
        // longer base64url with underscore and hyphen
        validate_session_id_for_acceptance("abc-def_ghi-jkl_mno-pqr").unwrap();
    }

    #[test]
    fn empty_id_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn short_weak_id_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("s1")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
        assert_eq!(
            validate_session_id_for_acceptance("decision-demo-1")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn uppercase_uuid_rejected() {
        let id = uuid::Uuid::new_v4()
            .as_hyphenated()
            .to_string()
            .to_uppercase();
        assert_eq!(
            validate_session_id_for_acceptance(&id)
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn base64url_36_chars_with_hyphen_accepted() {
        // 36-char base64url token containing '-' that is NOT UUID-shaped: must be
        // accepted via the base64url rule, not rejected by the UUID branch.
        // (Regression test for the hard-routing bug: len==36 && contains('-')
        // previously returned Err without trying the base64url rule.)
        let id = "Zx-abcdefghijklmnopqrstuvwxyz_ABCDE-";
        assert_eq!(id.len(), 36);
        assert!(uuid::Uuid::parse_str(id).is_err());
        validate_session_id_for_acceptance(id).unwrap();
    }

    #[test]
    fn uuid_shaped_but_wrong_version_does_not_fall_through() {
        // A canonical v1 UUID is also 36 chars of valid base64url charset; it must
        // still be rejected (UUID rules apply, no fall-through to base64url).
        let v4 = uuid::Uuid::new_v4();
        let mut bytes = *v4.as_bytes();
        bytes[6] = (bytes[6] & 0x0F) | 0x10;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v1_id = uuid::Uuid::from_bytes(bytes).as_hyphenated().to_string();
        assert!(validate_session_id_for_acceptance(&v1_id).is_err());
    }

    #[test]
    fn base64url_too_short_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("abcdefghij")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn valid_uuid_v7_accepted() {
        // Construct a v7 UUID by patching the version nibble of a v4 UUID
        let v4 = uuid::Uuid::new_v4();
        let mut bytes = *v4.as_bytes();
        // Set version nibble (bits 48-51) to 0b0111 (v7)
        bytes[6] = (bytes[6] & 0x0F) | 0x70;
        // Keep variant bits valid (RFC 4122: 0b10xx)
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v7_id = uuid::Uuid::from_bytes(bytes).as_hyphenated().to_string();
        assert!(validate_session_id_for_acceptance(&v7_id).is_ok());
    }

    #[test]
    fn uuid_v1_rejected() {
        // Construct a v1 UUID by patching the version nibble of a v4 UUID
        let v4 = uuid::Uuid::new_v4();
        let mut bytes = *v4.as_bytes();
        // Set version nibble (bits 48-51) to 0b0001 (v1)
        bytes[6] = (bytes[6] & 0x0F) | 0x10;
        // Keep variant bits valid (RFC 4122: 0b10xx)
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v1_id = uuid::Uuid::from_bytes(bytes).as_hyphenated().to_string();
        assert_eq!(
            validate_session_id_for_acceptance(&v1_id)
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn too_many_participants_rejected() {
        let participants: Vec<String> = (0..1001).map(|i| format!("agent://p{i}")).collect();
        let bytes = encode_payload(5000, participants);
        let payload = parse_session_start_payload(&bytes).unwrap();
        assert_eq!(
            validate_canonical_session_start_payload(&payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn max_participants_accepted() {
        let participants: Vec<String> = (0..1000).map(|i| format!("agent://p{i}")).collect();
        let bytes = encode_payload(5000, participants);
        let payload = parse_session_start_payload(&bytes).unwrap();
        validate_canonical_session_start_payload(&payload).unwrap();
    }

    // ---- Phase 11b: suspension intervals + the unsuspended deadline ----

    /// `resume` records the completed pause, in order, at every revision.
    #[test]
    fn resume_records_completed_suspension_intervals() {
        let mut s = open_session(100_000);
        assert!(s.suspension_intervals.is_empty());
        s.suspend(2_000).unwrap();
        // An in-progress suspension is NOT in the vec — completed pairs only.
        assert!(s.suspension_intervals.is_empty());
        s.resume(5_000).unwrap();
        s.suspend(6_000).unwrap();
        s.resume(6_500).unwrap();
        assert_eq!(s.suspension_intervals, vec![(2_000, 5_000), (6_000, 6_500)]);

        // Recorded at legacy revisions too (read only at rev >= 2).
        let mut legacy = open_session(100_000);
        legacy.semantics_rev = 0;
        legacy.suspend(1_000).unwrap();
        legacy.resume(1_400).unwrap();
        assert_eq!(legacy.suspension_intervals, vec![(1_000, 1_400)]);
    }

    /// The pause is recorded even when the resume force-expires the session:
    /// the push happens before either cap check can return.
    #[test]
    fn resume_records_the_pause_even_when_it_force_expires() {
        let mut s = open_session(10_000);
        s.suspend(0).unwrap();
        assert!(s.resume(MAX_SUSPEND_MS + 1).is_err());
        assert_eq!(s.state, SessionState::Expired);
        assert_eq!(s.suspension_intervals, vec![(0, MAX_SUSPEND_MS + 1)]);
    }

    /// Acceptance criterion 1 — the `unsuspended_deadline` matrix
    /// (RFC-MACP-0010 §5.1(3): "offer acceptance time + timeout + suspended
    /// time *within the window*").
    #[test]
    fn unsuspended_deadline_walks_the_suspension_intervals() {
        let base = open_session(1_000_000);
        let with = |pairs: Vec<(i64, i64)>| {
            let mut s = base.clone();
            s.suspension_intervals = pairs;
            s
        };

        // No pauses: the raw deadline.
        assert_eq!(with(vec![]).unsuspended_deadline(1_000, 100), 1_100);

        // One pause that starts inside the window: extends by its full width.
        assert_eq!(
            with(vec![(1_050, 1_200)]).unsuspended_deadline(1_000, 100),
            1_250
        );

        // One pause starting after the raw deadline: does NOT extend it.
        assert_eq!(
            with(vec![(1_500, 1_600)]).unsuspended_deadline(1_000, 100),
            1_100
        );

        // Boundary: a pause starting exactly at the returned deadline does not
        // extend it — the timeout had already elapsed at that instant.
        assert_eq!(
            with(vec![(1_100, 1_300)]).unsuspended_deadline(1_000, 100),
            1_100
        );

        // Two pauses, both inside the window: both are added.
        assert_eq!(
            with(vec![(1_050, 1_200), (1_230, 1_300)]).unsuspended_deadline(1_000, 100),
            1_320
        );

        // A pause predating `from_ms` is ignored entirely.
        assert_eq!(
            with(vec![(500, 700)]).unsuspended_deadline(1_000, 100),
            1_100
        );
        assert_eq!(
            with(vec![(500, 700), (1_050, 1_200)]).unsuspended_deadline(1_000, 100),
            1_250
        );
    }

    /// The walk must never OVER-report, whatever shape the vec is in. The
    /// pairs are not guaranteed sorted, disjoint, or even monotone: `Utc::now`
    /// is not monotonic (an NTP step back between suspend and resume yields
    /// `e < s`, which `resume` records as-is — it clamps only `banked`), and
    /// the public `SessionBuilder::suspension_intervals` setter lets a library
    /// consumer supply arbitrary pairs. Each case below drove `remaining`
    /// upward or `cur` backwards before the clamps in `unsuspended_deadline`.
    #[test]
    fn unsuspended_deadline_never_over_reports_on_adversarial_pairs() {
        let base = open_session(1_000_000);
        let with = |pairs: Vec<(i64, i64)>| {
            let mut s = base.clone();
            s.suspension_intervals = pairs;
            s
        };

        // Overlapping pairs, second starting inside the first. The 100 ms of
        // unsuspended time is exhausted at 1_050 + the union of the pauses
        // (1_050..1_400) + the remaining 50 ms => 1_450. Before the clamp the
        // negative run (1_100 - 1_400) GREW `remaining` to 350 and returned
        // 1_550 — a deadline 100 ms LATER than the truth.
        assert_eq!(
            with(vec![(1_050, 1_400), (1_100, 1_200)]).unsuspended_deadline(1_000, 100),
            1_450
        );

        // A fully-nested pair: the inner pause contributes nothing.
        assert_eq!(
            with(vec![(1_050, 1_400), (1_200, 1_300)]).unsuspended_deadline(1_000, 100),
            1_450
        );

        // A backwards pair (e < s), as an NTP step back would record it. It
        // must neither rewind `cur` nor inflate `remaining`; the 50 ms of run
        // before it is consumed and nothing is added.
        assert_eq!(
            with(vec![(1_050, 1_000)]).unsuspended_deadline(1_000, 100),
            1_100
        );
        // ... and the same pair ahead of a real one extends by the real
        // pause's width only (60 ms of run, then the 1_060..1_200 pause, then
        // the remaining 40 ms), the degenerate pair counting as zero-width.
        assert_eq!(
            with(vec![(1_050, 1_000), (1_060, 1_200)]).unsuspended_deadline(1_000, 100),
            1_240
        );

        // Unsorted pairs: the later pause listed first. The walk's answer must
        // still not exceed the sorted-order answer.
        let sorted = with(vec![(1_050, 1_200), (1_230, 1_300)]).unsuspended_deadline(1_000, 100);
        let unsorted = with(vec![(1_230, 1_300), (1_050, 1_200)]).unsuspended_deadline(1_000, 100);
        assert_eq!(sorted, 1_320);
        assert!(
            unsorted <= sorted,
            "an unsorted vec must under-report, never over-report \
             ({unsorted} vs {sorted})"
        );

        // The invariant stated in the rustdoc, checked directly: the walk's
        // own contribution never exceeds the sum of the normalized pair widths (an upper bound on their union).
        for pairs in [
            vec![(1_050, 1_400), (1_100, 1_200)],
            vec![(1_050, 1_400), (1_200, 1_300)],
            vec![(1_050, 1_000)],
            vec![(1_230, 1_300), (1_050, 1_200)],
        ] {
            let union: i64 = pairs.iter().map(|(s, e)| (e - s).max(0)).sum();
            let walk = with(pairs.clone()).unsuspended_deadline(1_000, 100) - 1_100;
            assert!(
                (0..=union).contains(&walk),
                "walk contribution {walk} outside [0, {union}] for {pairs:?}"
            );
        }
    }

    /// Acceptance criterion 5 — the cycle cap fires on *count*, at rev 2 only.
    ///
    /// Every pause here is 0 ms wide, so `accumulated_suspended_ms` stays at 0
    /// and `MAX_SUSPEND_MS` is nowhere near exhausted: only the count cap can
    /// be what expires the session.
    #[test]
    fn suspension_cycle_cap_force_expires_at_rev2() {
        let mut s = open_session(1_000_000_000);
        assert_eq!(s.semantics_rev, CURRENT_SEMANTICS_REV);
        assert!(CURRENT_SEMANTICS_REV >= 2);
        for i in 0..MAX_SUSPENSION_CYCLES as i64 {
            s.suspend(i).unwrap();
            s.resume(i).unwrap();
        }
        assert_eq!(s.suspension_intervals.len(), MAX_SUSPENSION_CYCLES);
        assert_eq!(s.accumulated_suspended_ms, 0);
        assert_eq!(s.state, SessionState::Open);

        // One cycle past the cap force-expires.
        s.suspend(MAX_SUSPENSION_CYCLES as i64).unwrap();
        let err = s.resume(MAX_SUSPENSION_CYCLES as i64).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s.state, SessionState::Expired);
        assert_eq!(
            s.accumulated_suspended_ms, 0,
            "the duration cap must be nowhere near exhausted, else the count \
             cap is not what fired"
        );

        // The rev gate: the identical sequence on a rev-1 session keeps
        // succeeding, so legacy histories replay bit-identically.
        let mut legacy = open_session(1_000_000_000);
        legacy.semantics_rev = 1;
        for i in 0..(MAX_SUSPENSION_CYCLES as i64 + 10) {
            legacy.suspend(i).unwrap();
            legacy.resume(i).unwrap();
        }
        assert_eq!(legacy.state, SessionState::Open);
    }

    /// The other half of the cap: a legacy (rev <= 1) session is reachable
    /// through the same un-rate-limited `SuspendSession`/`ResumeSession` RPCs
    /// as a current one, so its `suspension_intervals` must be bounded too —
    /// but it must NOT be force-expired by a rule postdating its acceptance.
    /// So the cap stops the *recording* instead: the session keeps cycling and
    /// the vec stays pinned at [`MAX_SUSPENSION_CYCLES`].
    #[test]
    fn suspension_cycle_cap_stops_recording_at_rev1_without_expiring() {
        for rev in [0u32, 1] {
            let mut s = open_session(1_000_000_000);
            s.semantics_rev = rev;
            for i in 0..(MAX_SUSPENSION_CYCLES as i64 * 2) {
                s.suspend(i).unwrap();
                assert_eq!(s.state, SessionState::Suspended, "rev {rev}, cycle {i}");
                s.resume(i)
                    .unwrap_or_else(|e| panic!("rev {rev}, cycle {i} must resume, got {e:?}"));
                assert_eq!(s.state, SessionState::Open, "rev {rev}, cycle {i}");
            }
            assert_eq!(
                s.suspension_intervals.len(),
                MAX_SUSPENSION_CYCLES,
                "rev {rev}: the vec must stay pinned at the cap, not grow \
                 unbounded — an unbounded vec is the O(N²) snapshot \
                 amplification MAX_SUSPENSION_CYCLES exists to close"
            );
            // The recorded prefix is the FIRST `MAX_SUSPENSION_CYCLES` pauses:
            // overflow is dropped, not rotated, so nothing already written
            // ever changes.
            assert_eq!(s.suspension_intervals[0], (0, 0));
            assert_eq!(
                s.suspension_intervals[MAX_SUSPENSION_CYCLES - 1],
                (
                    MAX_SUSPENSION_CYCLES as i64 - 1,
                    MAX_SUSPENSION_CYCLES as i64 - 1
                )
            );
        }
    }

    /// Acceptance criterion 6 — a rev-2 session carrying the pre-11b artifact
    /// shape (positive `accumulated_suspended_ms`, empty
    /// `suspension_intervals`, e.g. a snapshot or mid-session checkpoint
    /// written before the field existed) walks to a deadline at or *earlier*
    /// than the fully-recorded one. Under-counting is the safe direction.
    #[test]
    fn legacy_rev2_snapshot_without_intervals_walks_early_not_late() {
        let mut recorded = open_session(1_000_000);
        recorded.semantics_rev = 2;
        recorded.accumulated_suspended_ms = 150 + 70;
        recorded.suspension_intervals = vec![(1_050, 1_200), (1_230, 1_300)];

        let mut legacy = recorded.clone();
        legacy.suspension_intervals.clear();

        let recorded_deadline = recorded.unsuspended_deadline(1_000, 100);
        let legacy_deadline = legacy.unsuspended_deadline(1_000, 100);
        assert_eq!(recorded_deadline, 1_320);
        assert_eq!(legacy_deadline, 1_100);
        assert!(
            legacy_deadline <= recorded_deadline,
            "an under-reported interval vec must move the deadline EARLIER \
             ({legacy_deadline} vs {recorded_deadline}), never later"
        );

        // The invariant the walk relies on: the walk's own contribution never
        // exceeds the scalar it is refining.
        let walk_sum = recorded_deadline - (1_000 + 100);
        assert!(walk_sum <= recorded.accumulated_suspended_ms);
        let legacy_walk_sum = legacy_deadline - (1_000 + 100);
        assert!(legacy_walk_sum <= legacy.accumulated_suspended_ms);
    }
}
