//! Rebuild a [`Session`] from recorded history (RFC-MACP-0003), with no
//! storage, transport or registry — the library half of the server's log
//! replay, which delegates here.
//!
//! An embedder that drives sessions through [`crate::step`] records, per
//! session:
//!
//! - the accepted `SessionStart` envelope and what [`crate::step`] resolved
//!   for it — [`RecordedStart`];
//! - every later event, in order — [`ReplayEvent`].
//!
//! [`replay`] re-drives that history through the mode at each **recorded**
//! acceptance time, never wall-clock, and without the client boundary
//! ([`crate::step::admit`]'s `validate_client_envelope`): recorded entries
//! already passed it, and some legal history (the handoff synthetic accept's
//! reserved `implicit-accept:` id) would fail it.
//!
//! Bindings come from the record, never from live configuration: the
//! `semantics_rev`, the suspension cap and the policy definition a session was
//! accepted under are replay inputs (RFC-MACP-0003 §2, RFC-MACP-0012 §8), so a
//! runtime upgrade or a policy edit never changes what a history rebuilds to.

use macp_core::error::MacpError;
use macp_core::mode::MessageContext;
use macp_core::policy::PolicyDefinition;
use macp_core::session::{
    extract_ttl_ms, parse_session_start_payload, requires_strict_session_start,
    validate_canonical_session_start_payload_for_mode, Session, SessionState,
};
use macp_pb::pb::{Envelope, SessionStartPayload};

use crate::mode::Mode;

/// The TTL a non-strict (experimental extension) session started with no
/// `ttl_ms` replays under — the legacy acceptance default.
pub const LEGACY_EXTENSION_TTL_MS: i64 = 60_000;

/// What a durable embedder recorded when it accepted a `SessionStart`.
///
/// Build it with [`RecordedStart::new`] from the accepted envelope and the
/// acceptance clock, then set what [`crate::step::accept_session_start`]
/// resolved — every field here is one replay must not re-derive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RecordedStart {
    /// The accepted `SessionStart` envelope, exactly as recorded.
    pub envelope: Envelope,
    /// The acceptance clock the start was accepted at (the session's
    /// `started_at_unix_ms`).
    pub accepted_at_ms: i64,
    /// The `Session::semantics_rev` it was accepted under. Legacy records
    /// with none replay as `0`.
    pub semantics_rev: u32,
    /// The resolved suspension cap (`Session::max_suspend_ms`). `0` = unbound
    /// (legacy): the default cap applies.
    pub max_suspend_ms: i64,
    /// `ValidatedStart::bound_mode_version`, when the descriptor's version
    /// was bound in place of an omitted payload `mode_version`.
    pub bound_mode_version: Option<String>,
    /// The policy definition the session bound (`Session::policy_definition`).
    /// When the payload named no `policy_version`, the session binds this
    /// definition's id, as [`crate::step::ValidatedStart::accept`] did.
    pub policy: Option<PolicyDefinition>,
    /// Whether the canonical SessionStart contract applies. `None`:
    /// [`requires_strict_session_start`] decides from the mode name, as
    /// [`crate::step::StartOptions::strict`] does.
    pub strict: Option<bool>,
}

impl RecordedStart {
    /// A record with no bindings beyond the envelope: `semantics_rev` 0, the
    /// default suspension cap, no policy. Set the rest with the `with_*`
    /// methods, or use [`RecordedStart::from_session`].
    pub fn new(envelope: Envelope, accepted_at_ms: i64) -> Self {
        Self {
            envelope,
            accepted_at_ms,
            semantics_rev: 0,
            max_suspend_ms: 0,
            bound_mode_version: None,
            policy: None,
            strict: None,
        }
    }

    /// The record for a session [`crate::step::accept_session_start`] just
    /// returned: every binding copied from it. `bound_mode_version` is not on
    /// the session; pass `ValidatedStart::bound_mode_version`.
    pub fn from_session(
        envelope: Envelope,
        session: &Session,
        bound_mode_version: Option<String>,
    ) -> Self {
        Self {
            envelope,
            accepted_at_ms: session.started_at_unix_ms,
            semantics_rev: session.semantics_rev,
            max_suspend_ms: session.max_suspend_ms,
            bound_mode_version,
            policy: session.policy_definition.clone(),
            strict: None,
        }
    }

    pub fn with_semantics_rev(mut self, rev: u32) -> Self {
        self.semantics_rev = rev;
        self
    }

    pub fn with_max_suspend_ms(mut self, ms: i64) -> Self {
        self.max_suspend_ms = ms;
        self
    }

    pub fn with_bound_mode_version(mut self, version: Option<String>) -> Self {
        self.bound_mode_version = version;
        self
    }

    pub fn with_policy(mut self, policy: Option<PolicyDefinition>) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict = Some(strict);
        self
    }
}

/// One recorded event after the `SessionStart`, in history order.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ReplayEvent {
    /// An envelope a participant sent, accepted at `accepted_at_ms`
    /// ([`crate::step::commit`]). Credits participant activity when
    /// [`ReplayOptions::credit_participant_activity`] is set.
    Message {
        envelope: Envelope,
        accepted_at_ms: i64,
    },
    /// A runtime-originated envelope ([`crate::step::commit_synthetic`]),
    /// dispatched at its own `timestamp_unix_ms` (the computed deadline) and
    /// never credited as participant activity.
    Synthetic { envelope: Envelope },
    /// `Session::suspend` at `at_ms`.
    Suspended { at_ms: i64 },
    /// `Session::resume` at `at_ms`.
    Resumed { at_ms: i64 },
    /// `Session::cancel`.
    Cancelled,
    /// The TTL elapsed (or the suspension cap force-expired the session).
    Expired,
}

/// Replay knobs that are properties of the *recorder*, not of the session.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReplayOptions {
    /// Credit each [`ReplayEvent::Message`]'s sender with activity at its
    /// acceptance time, as [`crate::step::commit`] did live, so
    /// `participant_message_counts`/`participant_last_seen` rebuild too.
    /// Default `true`. A recorder that cannot tell participant messages from
    /// runtime-originated ones (the server's log, which records both as
    /// `Incoming`) sets it `false` rather than credit a synthetic accept.
    pub credit_participant_activity: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            credit_participant_activity: true,
        }
    }
}

impl ReplayOptions {
    pub fn with_credit_participant_activity(mut self, credit: bool) -> Self {
        self.credit_participant_activity = credit;
        self
    }
}

/// Rebuild the session `start` began, then apply `events` in order.
///
/// Errors when the start no longer decodes or validates, or when the mode
/// refuses a recorded event — either means the history and this mode disagree
/// (corruption, or a mode that changed behaviour without a `semantics_rev`
/// gate), and a silently different session would be worse than none.
pub fn replay(
    start: &RecordedStart,
    events: impl IntoIterator<Item = ReplayEvent>,
    mode: &dyn Mode,
    opts: &ReplayOptions,
) -> Result<Session, MacpError> {
    let mut session = replay_start(start, mode)?;
    for event in events {
        apply_event(&mut session, event, mode, opts)?;
    }
    Ok(session)
}

/// The session `start` began, before any later event.
pub fn replay_start(start: &RecordedStart, mode: &dyn Mode) -> Result<Session, MacpError> {
    let env = &start.envelope;
    let strict = start
        .strict
        .unwrap_or_else(|| requires_strict_session_start(&env.mode));
    let payload = if env.payload.is_empty() && !strict {
        SessionStartPayload::default()
    } else {
        parse_session_start_payload(&env.payload)?
    };
    if strict {
        validate_canonical_session_start_payload_for_mode(&env.mode, &payload)?;
    }
    let ttl_ms = if !strict && payload.ttl_ms == 0 {
        LEGACY_EXTENSION_TTL_MS
    } else {
        extract_ttl_ms(&payload)?
    };
    // RFC-MACP-0003 §2: the deadline is the SessionStart envelope's timestamp
    // plus ttl_ms — the same base `step::validate_session_start` bound live —
    // falling back to the acceptance clock for an envelope that carried none.
    let ttl_base = if env.timestamp_unix_ms > 0 {
        env.timestamp_unix_ms
    } else {
        start.accepted_at_ms
    };
    // `ValidatedStart::accept`'s rule: a payload that named no policy binds
    // the id of the definition it resolved to (RFC-MACP-0012 §6.1).
    let policy_version = match (&start.policy, payload.policy_version.is_empty()) {
        (Some(p), true) => p.policy_id.clone(),
        _ => payload.policy_version.clone(),
    };
    let mode_version = start
        .bound_mode_version
        .clone()
        .unwrap_or_else(|| payload.mode_version.clone());
    let mut session =
        Session::builder(env.session_id.clone(), env.mode.clone(), env.sender.clone())
            .semantics_rev(start.semantics_rev)
            .max_suspend_ms(start.max_suspend_ms)
            .ttl_expiry(ttl_base.saturating_add(ttl_ms))
            .ttl_ms(ttl_ms)
            .started_at_unix_ms(start.accepted_at_ms)
            .participants(payload.participants)
            .intent(payload.intent)
            .mode_version(mode_version)
            .configuration_version(payload.configuration_version)
            .policy_version(policy_version)
            .context_id(payload.context_id)
            .extensions(payload.extensions)
            .roots(payload.roots)
            .policy_definition(start.policy.clone())
            .build();
    let response = mode.on_session_start(&session, env)?;
    session.seen_message_ids.insert(env.message_id.clone());
    session.apply_mode_response(response);
    Ok(session)
}

/// Apply one recorded event to a replaying session — the per-entry step of
/// [`replay`], public for a recorder that resumes from its own checkpoint.
///
/// An accepted envelope arriving after the session left `Open` (recorded
/// before the terminal transition was observed) only re-consumes its dedup
/// slot: the live kernel accepted it, and its id must stay taken.
pub fn apply_event(
    session: &mut Session,
    event: ReplayEvent,
    mode: &dyn Mode,
    opts: &ReplayOptions,
) -> Result<(), MacpError> {
    let (envelope, accepted_at_ms, credit) = match event {
        ReplayEvent::Message {
            envelope,
            accepted_at_ms,
        } => (envelope, accepted_at_ms, opts.credit_participant_activity),
        ReplayEvent::Synthetic { envelope } => {
            let at = envelope.timestamp_unix_ms;
            (envelope, at, false)
        }
        ReplayEvent::Suspended { at_ms } => {
            let _ = session.suspend(at_ms);
            return Ok(());
        }
        ReplayEvent::Resumed { at_ms } => {
            let _ = session.resume(at_ms);
            return Ok(());
        }
        ReplayEvent::Cancelled => {
            let _ = session.cancel();
            return Ok(());
        }
        ReplayEvent::Expired => {
            session.state = SessionState::Expired;
            return Ok(());
        }
    };
    if session.state != SessionState::Open {
        if !envelope.message_id.is_empty() {
            session.seen_message_ids.insert(envelope.message_id);
        }
        return Ok(());
    }
    mode.authorize_sender(session, &envelope)?;
    let response = mode.on_message_at(session, &envelope, &MessageContext::new(accepted_at_ms))?;
    session.apply_mode_response(response);
    if credit {
        session.record_participant_activity(&envelope.sender, accepted_at_ms);
    }
    if !envelope.message_id.is_empty() {
        session.seen_message_ids.insert(envelope.message_id);
    }
    Ok(())
}
