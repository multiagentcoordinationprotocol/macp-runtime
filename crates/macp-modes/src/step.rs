//! The per-message coordination step — the pure, I/O-free kernel invariants.
//!
//! Every accepted MACP message passes the same per-message invariants: the
//! envelope shape, dedup (RFC-MACP-0001 §8 idempotency), mode-binding, the
//! deadline (TTL and suspension caps), and the monotonic OPEN gate
//! (§7.2/§7.3); then authorization and the client boundary; then any
//! runtime-originated envelope already due (RFC-MACP-0010 §5.1(2)'s synthetic
//! implicit accept); then mode validation, then commit. This module hosts them
//! once — synchronous and free of tokio, storage, transport, and the wall
//! clock (the caller injects `now_ms`) — and the gRPC server's kernel calls
//! the same functions, so a library embedder and the server cannot drift.
//!
//! Session creation has the same split: [`accept_session_start`] (or its two
//! halves, [`validate_session_start`] and [`ValidatedStart::accept`]) is the
//! only supported way to turn a client `SessionStart` into a [`Session`].
//!
//! Two ways to drive a message:
//! - [`step`] — all-in-one, for consumers that do not interpose durable
//!   storage between validation and commit. Synthesized envelopes come back in
//!   [`StepOutcome::Accepted`].
//! - the phases, for a durable consumer that writes each accepted envelope to
//!   an append-only log before committing it, so a failed write never consumes
//!   a dedup slot:
//!   [`check_preconditions`] → [`admit`] → loop {[`next_synthetic`] → append
//!   → [`commit_synthetic`]} → [`dispatch`] → append → [`commit`].
//!
//! # Durable embedders and rejected triggers
//!
//! A due synthetic envelope enters history *before* the triggering message is
//! evaluated, so it stays in history even when that trigger is then rejected
//! (CLAUDE.md's freeze-profile carve-out). [`step`] reports synthesized
//! envelopes only on success; an embedder that must persist them whatever the
//! trigger's fate calls [`advance`] first with the same `now_ms` — the
//! envelopes were due independently of the trigger, so emitting them eagerly is
//! the server's background-sweep behaviour, and the following [`step`] finds
//! nothing left to synthesize.

use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::mode::MessageContext;
use macp_core::policy::PolicyDefinition;
use macp_core::session::{
    extract_ttl_ms, parse_session_start_payload, requires_strict_session_start,
    validate_canonical_session_start_payload_for_mode, validate_session_id_for_acceptance, Session,
    SessionState, CURRENT_SEMANTICS_REV, MAX_SUSPEND_MS,
};
use macp_pb::pb::{Envelope, SessionStartPayload};
use std::sync::Arc;

/// The upper bound on synthetic envelopes emitted for one observation.
///
/// Every shipped mode emits at most one (a handoff session has at most one
/// outstanding offer, RFC-MACP-0010 §5(5)), and [`Mode::due_synthetic_envelope`]
/// must return `None` once its envelope is applied. The bound turns a mode that
/// breaks that contract into an `InvalidModeState` error instead of a hang.
pub const MAX_SYNTHETIC_PER_STEP: usize = 64;

/// Outcome of the mode-independent precondition checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precheck {
    /// `message_id` already accepted — idempotent no-op.
    Duplicate,
    /// The session's deadline has passed ([`Session::deadline_passed`]); the
    /// caller must expire the session ([`Session::expire`]) and reject the
    /// message with [`MacpError::TtlExpired`].
    Expired,
    /// Preconditions satisfied — proceed to [`admit`].
    Proceed,
}

/// Reject an envelope that no session could accept: wrong protocol version, or
/// a missing `message_type`, `message_id` or `sender`.
///
/// The server checks the same shape at its transport boundary (and derives
/// `sender` from authentication, never from the envelope); an embedder has no
/// such boundary, so the library checks it again. An empty `sender` would
/// otherwise pass an empty-roster session's authorization and be recorded as
/// a message from nobody.
pub fn check_envelope_shape(env: &Envelope) -> Result<(), MacpError> {
    if env.macp_version != macp_core::MACP_VERSION {
        return Err(MacpError::InvalidMacpVersion);
    }
    if env.message_type.is_empty() || env.message_id.is_empty() || env.sender.is_empty() {
        return Err(MacpError::InvalidEnvelope);
    }
    Ok(())
}

/// Mode-independent per-message invariants. Pure: no mutation, no I/O, no clock.
///
/// Order: envelope shape ([`check_envelope_shape`]) → dedup → mode-binding →
/// deadline → the monotonic OPEN gate. `now_ms` is the injected acceptance
/// clock. The deadline test is [`Session::deadline_passed`]: an `Open` session
/// past `ttl_expiry` (strict `>`), or a `Suspended` one past its suspension
/// cap. A terminal session is never re-expired (it falls through to
/// [`MacpError::SessionNotOpen`]), and neither is a suspended one within its
/// cap.
pub fn check_preconditions(
    session: &Session,
    env: &Envelope,
    now_ms: i64,
) -> Result<Precheck, MacpError> {
    check_envelope_shape(env)?;
    if session.seen_message_ids.contains(&env.message_id) {
        return Ok(Precheck::Duplicate);
    }
    if env.mode != session.mode || env.session_id != session.session_id {
        return Err(MacpError::InvalidEnvelope);
    }
    if session.deadline_passed(now_ms) {
        return Ok(Precheck::Expired);
    }
    if session.state != SessionState::Open {
        return Err(MacpError::SessionNotOpen);
    }
    Ok(Precheck::Proceed)
}

/// Sender authorization, then the client boundary
/// ([`Mode::validate_client_envelope`]). Pure. Runs before synthesis, so an
/// unauthorized or forged envelope can never provoke a synthetic append.
///
/// The client boundary rejects envelope shapes that are legal as *recorded
/// history* but illegal as *client submissions* (the reserved
/// `implicit-accept:` id namespace, for one). Replay must not call this — it
/// re-reads entries that already passed it.
pub fn admit(session: &Session, env: &Envelope, mode: &dyn Mode) -> Result<(), MacpError> {
    mode.authorize_sender(session, env)?;
    mode.validate_client_envelope(session, env)
}

/// Mode dispatch at the injected acceptance clock: [`Mode::on_message_at`]
/// with `accepted_at_ms = now_ms`. Pure — returns the [`ModeResponse`] to
/// apply. This is the clock modes measure time-dependent rules against
/// (handoff's implicit-accept timeout at `semantics_rev >= 1`); the envelope's
/// own `timestamp_unix_ms` is client-supplied and never trusted for that.
pub fn dispatch(
    session: &Session,
    env: &Envelope,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<ModeResponse, MacpError> {
    mode.on_message_at(session, env, &MessageContext::new(now_ms))
}

/// [`admit`] then [`dispatch`]: the whole mode-dependent validation of a
/// client message, for a consumer that has already settled synthesis.
pub fn validate_message(
    session: &Session,
    env: &Envelope,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<ModeResponse, MacpError> {
    admit(session, env, mode)?;
    dispatch(session, env, mode, now_ms)
}

/// Commit a validated message into the session: consume the dedup slot, record
/// participant activity, and apply the mode response. Returns the resulting
/// session state.
///
/// A durable consumer MUST call this only after the message has been durably
/// recorded, so a failed write never consumes a dedup slot. Because nothing
/// here mutates the session until validation has already succeeded, a rejected
/// message likewise leaves `seen_message_ids` untouched.
pub fn commit(
    session: &mut Session,
    env: &Envelope,
    response: ModeResponse,
    now_ms: i64,
) -> SessionState {
    session.seen_message_ids.insert(env.message_id.clone());
    session.record_participant_activity(&env.sender, now_ms);
    session.apply_mode_response(response);
    session.state.clone()
}

/// A runtime-originated envelope that is due, already validated against the
/// session, and not yet applied. Produced by [`next_synthetic`], applied by
/// [`commit_synthetic`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Synthetic {
    /// The envelope to append to accepted history. Record it with
    /// `received_at_ms == envelope.timestamp_unix_ms` (the computed deadline,
    /// never wall-clock): replay dispatches each entry at its recorded receipt
    /// time, and any other stamp forks the replayed state from the live one.
    pub envelope: Envelope,
    response: ModeResponse,
}

/// The next envelope the mode says MUST enter accepted history before anything
/// else is evaluated at `now_ms` — the kernel half of
/// [`Mode::due_synthetic_envelope`]'s contract — dispatched and validated but
/// not applied. Pure.
///
/// `None` when the session is not `Open` (a suspended session ticks no
/// unsuspended time, and both computations behind the synthetic ignore an
/// in-flight pause, so asking would bake a wrong timestamp into history), when
/// nothing is due, or when the envelope's id is already recorded (a backstop
/// for a mode that forgets its own idempotence).
pub fn next_synthetic(
    session: &Session,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<Option<Synthetic>, MacpError> {
    if session.state != SessionState::Open {
        return Ok(None);
    }
    let Some(envelope) = mode.due_synthetic_envelope(session, now_ms) else {
        return Ok(None);
    };
    if session.seen_message_ids.contains(&envelope.message_id) {
        return Ok(None);
    }
    // Mirrors replay, which authorizes every accepted entry.
    mode.authorize_sender(session, &envelope)?;
    let response = mode.on_message_at(
        session,
        &envelope,
        &MessageContext::new(envelope.timestamp_unix_ms),
    )?;
    Ok(Some(Synthetic { envelope, response }))
}

/// Apply a [`Synthetic`]: consume its dedup slot and apply its mode response.
/// Durable consumers call this only after appending `syn.envelope`.
///
/// Deliberately **no** participant activity: replay never records activity for
/// any entry, so crediting the synthetic's sender live would diverge from what
/// a rebuild produces — and the sender did not act.
pub fn commit_synthetic(session: &mut Session, syn: Synthetic) -> Envelope {
    session
        .seen_message_ids
        .insert(syn.envelope.message_id.clone());
    session.apply_mode_response(syn.response);
    syn.envelope
}

/// Emit every envelope due at `now_ms` into the session, in order, and return
/// them for the caller to record. In-memory: applies each immediately. Needs
/// no triggering message, so it is also the step-only embedder's timer tick —
/// the library equivalent of the server's eager sweep
/// (`Runtime::sweep_due_synthetic_accepts`).
pub fn advance(
    session: &mut Session,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<Vec<Envelope>, MacpError> {
    let mut emitted = Vec::new();
    while let Some(syn) = next_synthetic(session, mode, now_ms)? {
        if emitted.len() == MAX_SYNTHETIC_PER_STEP {
            return Err(MacpError::InvalidModeState);
        }
        emitted.push(commit_synthetic(session, syn));
    }
    Ok(emitted)
}

/// Outcome of [`step`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum StepOutcome {
    /// `message_id` already accepted — nothing changed.
    Duplicate,
    /// Message validated, committed, and applied.
    #[non_exhaustive]
    Accepted {
        /// The resulting session state.
        state: SessionState,
        /// Runtime-originated envelopes that entered accepted history
        /// *before* this message, in order (RFC-MACP-0010 §5.1(2)). A durable
        /// embedder appends these, then the message itself.
        synthesized: Vec<Envelope>,
    },
}

/// All-in-one per-message step: preconditions → [`admit`] → [`advance`] →
/// [`dispatch`] → [`commit`], the server kernel's exact order. Expiry marks the
/// session `Expired` and returns [`MacpError::TtlExpired`]; a duplicate is
/// [`StepOutcome::Duplicate`]; any other rejection returns its error without
/// consuming the message's dedup slot or applying its state.
///
/// A rejection *after* synthesis leaves the synthesized envelopes applied (they
/// were due regardless of the trigger) but does not return them; see the
/// module docs for the [`advance`]-first pattern a durable embedder uses.
pub fn step(
    session: &mut Session,
    env: &Envelope,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<StepOutcome, MacpError> {
    match check_preconditions(session, env, now_ms)? {
        Precheck::Duplicate => Ok(StepOutcome::Duplicate),
        Precheck::Expired => {
            session.expire()?;
            Err(MacpError::TtlExpired)
        }
        Precheck::Proceed => {
            admit(session, env, mode)?;
            let synthesized = advance(session, mode, now_ms)?;
            let response = dispatch(session, env, mode, now_ms)?;
            let state = commit(session, env, response, now_ms);
            Ok(StepOutcome::Accepted { state, synthesized })
        }
    }
}

// ── SessionStart ────────────────────────────────────────────────────────────

/// A deployment-specific session-id rule, replacing
/// [`validate_session_id_for_acceptance`] (UUID v4/v7 or a 22+ character
/// base64url token). Use it only to be *stricter*: RFC-MACP-0001's id format
/// is the floor every peer assumes.
pub type SessionIdValidator = Arc<dyn Fn(&str) -> Result<(), MacpError> + Send + Sync>;

/// Knobs for [`accept_session_start`]. Construct with `StartOptions::default()`
/// and the `with_*` methods; new knobs are additive.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct StartOptions {
    /// Whether the canonical (standards-track) SessionStart contract applies.
    /// `None`: [`requires_strict_session_start`] decides from the mode name.
    pub strict: Option<bool>,
    /// The mode descriptor's version. A payload naming a different
    /// `mode_version` is refused; a payload omitting it (only legal for a
    /// non-strict mode) is bound to this, so the Commitment version check is
    /// never vacuous.
    pub descriptor_mode_version: Option<String>,
    /// The suspension cap bound when the payload sets none. `None`:
    /// [`MAX_SUSPEND_MS`].
    pub default_max_suspend_ms: Option<i64>,
    /// Overrides the session-id format check.
    pub session_id_validator: Option<SessionIdValidator>,
}

impl std::fmt::Debug for StartOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartOptions")
            .field("strict", &self.strict)
            .field("descriptor_mode_version", &self.descriptor_mode_version)
            .field("default_max_suspend_ms", &self.default_max_suspend_ms)
            .field(
                "session_id_validator",
                &self.session_id_validator.as_ref().map(|_| "custom"),
            )
            .finish()
    }
}

impl StartOptions {
    /// See [`StartOptions::strict`].
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = Some(strict);
        self
    }
    /// See [`StartOptions::descriptor_mode_version`].
    pub fn with_descriptor_mode_version(mut self, version: impl Into<String>) -> Self {
        self.descriptor_mode_version = Some(version.into());
        self
    }
    /// See [`StartOptions::default_max_suspend_ms`].
    pub fn with_default_max_suspend_ms(mut self, ms: i64) -> Self {
        self.default_max_suspend_ms = Some(ms);
        self
    }
    /// See [`StartOptions::session_id_validator`].
    pub fn with_session_id_validator(
        mut self,
        validator: impl Fn(&str) -> Result<(), MacpError> + Send + Sync + 'static,
    ) -> Self {
        self.session_id_validator = Some(Arc::new(validator));
        self
    }
}

/// A `SessionStart` whose envelope and payload passed every check that needs
/// neither the mode nor the policy. Produced by [`validate_session_start`];
/// finished by [`ValidatedStart::accept`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ValidatedStart {
    /// The decoded payload.
    pub payload: SessionStartPayload,
    /// Set only when the payload omitted `mode_version` and the descriptor's
    /// version was bound in its place. A durable embedder records this with
    /// the SessionStart entry so replay uses the recorded binding, never a
    /// live registry.
    pub bound_mode_version: Option<String>,
    /// The validated `ttl_ms`.
    pub ttl_ms: i64,
    /// `ttl_base + ttl_ms`, where `ttl_base` is the envelope's timestamp when
    /// positive, else `now_ms` (RFC-MACP-0003 §2: the deadline is a function of
    /// the recorded start, so replay reproduces it).
    pub ttl_expiry: i64,
    /// The resolved suspension cap: the payload's positive value, else the
    /// option's default. Record it; replay must not consult configuration.
    pub max_suspend_ms: i64,
    /// The acceptance clock this start was validated at.
    pub accepted_at_ms: i64,
}

/// The mode- and policy-independent half of [`accept_session_start`], in
/// order: envelope shape and `message_type == "SessionStart"` → the session-id
/// format → payload decode → the canonical contract (when strict) →
/// `mode_version` against the descriptor → `ttl_ms`. Pure.
///
/// Split out so a durable consumer can interpose its own checks between the
/// halves — the server checks for an existing session and resolves the policy
/// here — without changing which error a doubly-invalid start reports.
pub fn validate_session_start(
    env: &Envelope,
    now_ms: i64,
    opts: &StartOptions,
) -> Result<ValidatedStart, MacpError> {
    check_envelope_shape(env)?;
    if env.message_type != "SessionStart" || env.mode.trim().is_empty() {
        return Err(MacpError::InvalidEnvelope);
    }
    match &opts.session_id_validator {
        Some(validate) => validate(&env.session_id)?,
        None => validate_session_id_for_acceptance(&env.session_id)?,
    }
    let payload = parse_session_start_payload(&env.payload)?;
    if opts
        .strict
        .unwrap_or_else(|| requires_strict_session_start(&env.mode))
    {
        validate_canonical_session_start_payload_for_mode(&env.mode, &payload)?;
    }
    if let Some(descriptor) = &opts.descriptor_mode_version {
        if !payload.mode_version.is_empty() && &payload.mode_version != descriptor {
            return Err(MacpError::InvalidEnvelope);
        }
    }
    let bound_mode_version = if payload.mode_version.is_empty() {
        opts.descriptor_mode_version.clone()
    } else {
        None
    };
    let ttl_ms = extract_ttl_ms(&payload)?;
    let ttl_base = if env.timestamp_unix_ms > 0 {
        env.timestamp_unix_ms
    } else {
        now_ms
    };
    let max_suspend_ms = if payload.max_suspend_ms > 0 {
        payload.max_suspend_ms
    } else {
        opts.default_max_suspend_ms.unwrap_or(MAX_SUSPEND_MS)
    };
    Ok(ValidatedStart {
        ttl_expiry: ttl_base.saturating_add(ttl_ms),
        payload,
        bound_mode_version,
        ttl_ms,
        max_suspend_ms,
        accepted_at_ms: now_ms,
    })
}

impl ValidatedStart {
    /// The `policy_version` the caller must resolve before [`Self::accept`]:
    /// the payload's, or `""` when it names none (the caller then supplies its
    /// default policy, e.g. `policy.default`, or `None` for no governance).
    pub fn requested_policy_version(&self) -> &str {
        &self.payload.policy_version
    }

    /// Finish the start: bind the resolved policy (RFC-MACP-0012 §6.1 step 5),
    /// build the session at [`CURRENT_SEMANTICS_REV`], run
    /// [`Mode::on_session_start`] and the client boundary, apply the mode's
    /// initial state, and consume the start's dedup slot.
    ///
    /// `policy` is the definition the caller resolved for
    /// [`Self::requested_policy_version`]:
    /// - a named version with no definition → [`MacpError::UnknownPolicyVersion`];
    /// - a definition whose `policy_id` is not the named version →
    ///   [`MacpError::UnknownPolicyVersion`] (the name did not resolve to it);
    /// - a definition bound to another mode (`mode` neither `"*"` nor this
    ///   session's mode) → [`MacpError::InvalidPolicyDefinition`].
    ///
    /// When no version is named, the session binds the supplied definition's
    /// `policy_id` (or `""` with `None`).
    pub fn accept(
        self,
        env: &Envelope,
        mode: &dyn Mode,
        policy: Option<PolicyDefinition>,
    ) -> Result<Session, MacpError> {
        let requested = self.payload.policy_version.as_str();
        let policy_version = match (&policy, requested.is_empty()) {
            (None, false) => return Err(MacpError::UnknownPolicyVersion),
            (Some(p), false) if p.policy_id != requested => {
                return Err(MacpError::UnknownPolicyVersion)
            }
            (Some(p), true) => p.policy_id.clone(),
            _ => requested.to_string(),
        };
        if let Some(p) = &policy {
            if p.mode != "*" && p.mode != env.mode {
                return Err(MacpError::InvalidPolicyDefinition);
            }
        }
        let mode_version = self
            .bound_mode_version
            .clone()
            .unwrap_or_else(|| self.payload.mode_version.clone());
        let mut session =
            Session::builder(env.session_id.clone(), env.mode.clone(), env.sender.clone())
                .ttl_expiry(self.ttl_expiry)
                .ttl_ms(self.ttl_ms)
                .max_suspend_ms(self.max_suspend_ms)
                .started_at_unix_ms(self.accepted_at_ms)
                .participants(self.payload.participants)
                .intent(self.payload.intent)
                .mode_version(mode_version)
                .configuration_version(self.payload.configuration_version)
                .policy_version(policy_version)
                .context_id(self.payload.context_id)
                .extensions(self.payload.extensions)
                .roots(self.payload.roots)
                .policy_definition(policy)
                .semantics_rev(CURRENT_SEMANTICS_REV)
                .build();
        let response = mode.on_session_start(&session, env)?;
        // The reserved id namespace is squattable through SessionStart too,
        // whose id enters the dedup set below.
        mode.validate_client_envelope(&session, env)?;
        session.seen_message_ids.insert(env.message_id.clone());
        session.apply_mode_response(response);
        Ok(session)
    }
}

/// Accept a client `SessionStart` into a new [`Session`] — the only supported
/// constructor for a session a client asked for, and the same code the server
/// kernel runs. [`validate_session_start`] then [`ValidatedStart::accept`].
///
/// `now_ms` is the acceptance clock (the session's `started_at_unix_ms`, and
/// the TTL base when the envelope carries no timestamp). The returned session
/// already holds the mode's initial state and the start's dedup slot; a durable
/// embedder records the envelope (with [`ValidatedStart::bound_mode_version`],
/// `session.max_suspend_ms` and `session.semantics_rev`, which replay needs)
/// before exposing the session.
pub fn accept_session_start(
    env: &Envelope,
    now_ms: i64,
    mode: &dyn Mode,
    policy: Option<PolicyDefinition>,
    opts: &StartOptions,
) -> Result<Session, MacpError> {
    validate_session_start(env, now_ms, opts)?.accept(env, mode, policy)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODE: &str = "macp.mode.test.v1";

    // A trivial mode: every participant may send; a `Commitment` resolves the
    // session, anything else just persists. Lets us exercise the step invariants
    // without policy or protobuf payloads.
    struct TestMode;
    impl Mode for TestMode {
        fn on_session_start(&self, _s: &Session, _e: &Envelope) -> Result<ModeResponse, MacpError> {
            Ok(ModeResponse::PersistState(vec![]))
        }
        fn on_message(&self, _s: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
            if env.message_type == "Commitment" {
                Ok(ModeResponse::PersistAndResolve {
                    state: vec![1],
                    resolution: vec![2],
                })
            } else {
                Ok(ModeResponse::PersistState(vec![1]))
            }
        }
        // default authorize_sender: sender must be a declared participant.
    }

    fn session() -> Session {
        Session::builder("11111111-1111-4111-8111-111111111111", MODE, "agent://a")
            .ttl_expiry(10_000)
            .ttl_ms(10_000)
            .participants(vec!["agent://a".into(), "agent://b".into()])
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .build()
    }

    fn env(sender: &str, message_type: &str, message_id: &str) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: MODE.into(),
            message_type: message_type.into(),
            message_id: message_id.into(),
            session_id: "11111111-1111-4111-8111-111111111111".into(),
            sender: sender.into(),
            timestamp_unix_ms: 0,
            payload: vec![],
        }
    }

    #[test]
    fn duplicate_is_reported_and_changes_nothing() {
        let mut s = session();
        s.seen_message_ids.insert("m1".into());
        let before = s.seen_message_ids.len();
        let out = step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, 1).unwrap();
        assert_eq!(out, StepOutcome::Duplicate);
        assert_eq!(s.seen_message_ids.len(), before);
        assert_eq!(s.state, SessionState::Open);
    }

    #[test]
    fn mode_binding_mismatch_rejected() {
        let mut s = session();
        let mut e = env("agent://a", "Msg", "m1");
        e.mode = "macp.mode.other.v1".into();
        assert!(matches!(
            step(&mut s, &e, &TestMode, 1).unwrap_err(),
            MacpError::InvalidEnvelope
        ));
        assert!(s.seen_message_ids.is_empty());
    }

    #[test]
    fn ttl_strict_boundary_does_not_expire_but_past_does() {
        // now == ttl_expiry: NOT expired (strict `>`), message is accepted.
        let mut s = session();
        let deadline = s.ttl_expiry;
        let out = step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, deadline).unwrap();
        assert_eq!(
            out,
            StepOutcome::Accepted {
                state: SessionState::Open,
                synthesized: vec![]
            }
        );

        // now > ttl_expiry: expired, session marked Expired, dedup untouched.
        let mut s2 = session();
        let past = s2.ttl_expiry + 1;
        let err = step(&mut s2, &env("agent://a", "Msg", "m2"), &TestMode, past).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s2.state, SessionState::Expired);
        assert!(s2.seen_message_ids.is_empty());
    }

    #[test]
    fn ttl_does_not_re_expire_a_resolved_session() {
        // A resolved session past its original ttl must report SessionNotOpen,
        // not flip to Expired or return TtlExpired (matches maybe_expire_session).
        let mut s = session();
        s.state = SessionState::Resolved;
        let past = s.ttl_expiry + 5_000;
        let err = step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, past).unwrap_err();
        assert!(matches!(err, MacpError::SessionNotOpen));
        assert_eq!(s.state, SessionState::Resolved);
    }

    #[test]
    fn non_open_session_rejected() {
        for st in [SessionState::Resolved, SessionState::Expired] {
            let mut s = session();
            s.state = st.clone();
            assert!(matches!(
                step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, 1).unwrap_err(),
                MacpError::SessionNotOpen
            ));
        }
    }

    #[test]
    fn accepted_consumes_dedup_records_activity_and_applies_state() {
        let mut s = session();
        let out = step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, 42).unwrap();
        assert_eq!(
            out,
            StepOutcome::Accepted {
                state: SessionState::Open,
                synthesized: vec![]
            }
        );
        assert!(s.seen_message_ids.contains("m1"));
        assert_eq!(s.mode_state, vec![1]);
        assert_eq!(s.participant_last_seen.get("agent://a"), Some(&42));
    }

    #[test]
    fn commitment_resolves() {
        let mut s = session();
        let out = step(&mut s, &env("agent://a", "Commitment", "c1"), &TestMode, 1).unwrap();
        assert_eq!(
            out,
            StepOutcome::Accepted {
                state: SessionState::Resolved,
                synthesized: vec![]
            }
        );
        assert_eq!(s.state, SessionState::Resolved);
        assert_eq!(s.resolution, Some(vec![2]));
    }

    #[test]
    fn rejected_validation_does_not_consume_dedup_slot() {
        // Dedup invariant (CLAUDE.md §8): a message rejected by mode validation
        // must NOT consume its dedup slot — a later valid message with the same
        // id is accepted normally.
        let mut s = session();
        let err = step(&mut s, &env("agent://stranger", "Msg", "m1"), &TestMode, 1).unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));
        assert!(!s.seen_message_ids.contains("m1"));
        // Same id, now from an authorized participant: accepted.
        let out = step(&mut s, &env("agent://a", "Msg", "m1"), &TestMode, 1).unwrap();
        assert_eq!(
            out,
            StepOutcome::Accepted {
                state: SessionState::Open,
                synthesized: vec![]
            }
        );
        assert!(s.seen_message_ids.contains("m1"));
    }

    #[test]
    fn clock_is_injected_not_wall_clock() {
        // Expiry is decided purely by the injected now_ms, independent of the
        // wall clock — a far-future deadline never expires, a past one does.
        let mut s = session();
        s.ttl_expiry = i64::MAX;
        assert!(matches!(
            check_preconditions(&s, &env("agent://a", "Msg", "m1"), i64::MAX - 1),
            Ok(Precheck::Proceed)
        ));
        let mut s2 = session();
        s2.ttl_expiry = 0;
        assert!(matches!(
            check_preconditions(&s2, &env("agent://a", "Msg", "m1"), 1),
            Ok(Precheck::Expired)
        ));
    }

    // A mode with a client boundary: it refuses one reserved `message_id` and
    // records whether dispatch was reached, so a test can tell "rejected at
    // the boundary" from "rejected by the mode's own rules".
    struct BoundaryMode {
        dispatched: std::sync::atomic::AtomicBool,
    }
    impl BoundaryMode {
        fn new() -> Self {
            Self {
                dispatched: std::sync::atomic::AtomicBool::new(false),
            }
        }
        fn dispatched(&self) -> bool {
            self.dispatched.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    impl Mode for BoundaryMode {
        fn on_session_start(&self, _s: &Session, _e: &Envelope) -> Result<ModeResponse, MacpError> {
            Ok(ModeResponse::PersistState(vec![]))
        }
        fn on_message(&self, _s: &Session, _e: &Envelope) -> Result<ModeResponse, MacpError> {
            self.dispatched
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ModeResponse::PersistState(vec![1]))
        }
        fn validate_client_envelope(&self, _s: &Session, env: &Envelope) -> Result<(), MacpError> {
            if env.message_id == "reserved" {
                return Err(MacpError::InvalidEnvelope);
            }
            Ok(())
        }
        // default authorize_sender: sender must be a declared participant.
    }

    /// `validate_message` enforces the client boundary (Phase 11c criterion 4),
    /// so a library consumer driving the phases through this helper inherits it
    /// without knowing the hook exists.
    ///
    /// Asserts the *position* too, not just the error: dispatch must not have
    /// been reached, and the envelope must be rejected **after**
    /// `authorize_sender` so the pre-existing Forbidden-before-payload error
    /// ordering is untouched. `step` inherits it by construction (it calls
    /// `validate_message`), and a rejection consumes no dedup slot.
    #[test]
    fn validate_message_enforces_the_client_boundary() {
        let s = session();
        let mode = BoundaryMode::new();

        let err = validate_message(&s, &env("agent://a", "Msg", "reserved"), &mode, 1).unwrap_err();
        assert!(matches!(err, MacpError::InvalidEnvelope));
        assert!(
            !mode.dispatched(),
            "the boundary must reject before dispatch"
        );

        // Ordering: unauthorized *and* reserved reports the authorization
        // error, because the hook runs after `authorize_sender`.
        let err = validate_message(&s, &env("agent://stranger", "Msg", "reserved"), &mode, 1)
            .unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));

        // A non-reserved id from the same sender passes the boundary and
        // reaches dispatch, so the rejection above was the hook's doing.
        validate_message(&s, &env("agent://a", "Msg", "m1"), &mode, 1).unwrap();
        assert!(mode.dispatched());

        // Through `step`: rejected, and no dedup slot consumed.
        let mut s2 = session();
        let err = step(&mut s2, &env("agent://a", "Msg", "reserved"), &mode, 1).unwrap_err();
        assert!(matches!(err, MacpError::InvalidEnvelope));
        assert!(s2.seen_message_ids.is_empty());
        assert_eq!(s2.state, SessionState::Open);
    }

    /// The default hook is `Ok(())`, so every mode that has no client-boundary
    /// rules is unaffected by the phase (semver-minor, no behavior change).
    #[test]
    fn default_client_boundary_is_open() {
        let s = session();
        for message_id in ["m1", "reserved", "implicit-accept:h1"] {
            assert!(
                TestMode
                    .validate_client_envelope(&s, &env("agent://a", "Msg", message_id))
                    .is_ok(),
                "default hook must accept {message_id}"
            );
        }
    }

    #[test]
    fn phases_compose_like_step_for_durable_consumers() {
        // The runtime path: check_preconditions -> validate_message -> commit.
        let mut s = session();
        let e = env("agent://b", "Msg", "m1");
        assert_eq!(check_preconditions(&s, &e, 5).unwrap(), Precheck::Proceed);
        let resp = validate_message(&s, &e, &TestMode, 5).unwrap();
        // Nothing applied until commit.
        assert!(s.seen_message_ids.is_empty());
        let state = commit(&mut s, &e, resp, 5);
        assert_eq!(state, SessionState::Open);
        assert!(s.seen_message_ids.contains("m1"));
    }
}
