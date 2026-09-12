//! The per-message coordination step — the pure, I/O-free kernel invariants.
//!
//! Every accepted MACP message passes the same per-message invariants: dedup
//! (RFC-MACP-0001 §8 idempotency), mode-binding, TTL, and the monotonic OPEN
//! gate (§7.2/§7.3), then mode validation, then commit. Historically these
//! lived welded into the gRPC server's `process_message`, so any other consumer
//! of the coordination core (e.g. an embedding library) had to re-implement
//! them and risk drift. This module hosts them once — synchronous and free of
//! tokio, storage, transport, and the wall clock (the caller injects `now_ms`).
//!
//! Two ways to drive it:
//! - [`step`] — all-in-one, for in-memory consumers that do not interpose
//!   durable storage between validation and commit.
//! - [`check_preconditions`] + [`validate_message`] + [`commit`] — the phases,
//!   for a durable consumer (the runtime) that must write the message to its
//!   append-only log *between* validation and commit, so a failed write never
//!   consumes a dedup slot.

use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::{Session, SessionState};
use macp_pb::pb::Envelope;

/// Outcome of the mode-independent precondition checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precheck {
    /// `message_id` already accepted — idempotent no-op.
    Duplicate,
    /// The session's TTL has elapsed; the caller must expire the session.
    Expired,
    /// Preconditions satisfied — proceed to mode validation.
    Proceed,
}

/// Mode-independent per-message invariants. Pure: no mutation, no I/O, no clock.
///
/// Order mirrors the runtime's `process_message` exactly: dedup → mode-binding
/// → TTL → the monotonic OPEN gate. `now_ms` is the injected clock (the
/// envelope/replay timestamp). The TTL check uses a strict `>` and is guarded
/// on `Open`, matching the runtime's `maybe_expire_session`: a message arriving
/// exactly at `ttl_expiry` does not expire, and a non-`Open` session is never
/// re-expired (it falls through to [`MacpError::SessionNotOpen`]).
pub fn check_preconditions(
    session: &Session,
    env: &Envelope,
    now_ms: i64,
) -> Result<Precheck, MacpError> {
    if session.seen_message_ids.contains(&env.message_id) {
        return Ok(Precheck::Duplicate);
    }
    if env.mode != session.mode {
        return Err(MacpError::InvalidEnvelope);
    }
    if session.state == SessionState::Open && now_ms > session.ttl_expiry {
        return Ok(Precheck::Expired);
    }
    if session.state != SessionState::Open {
        return Err(MacpError::SessionNotOpen);
    }
    Ok(Precheck::Proceed)
}

/// Mode-dependent validation: sender authorization + the client boundary +
/// mode rules. Pure — returns the [`ModeResponse`] to apply and mutates
/// nothing. Call only after [`check_preconditions`] returns
/// [`Precheck::Proceed`].
///
/// The middle phase is [`Mode::validate_client_envelope`], which rejects
/// envelope shapes that are legal as *recorded history* but illegal as *client
/// submissions* (see that method's contract). It runs after
/// [`Mode::authorize_sender`] so the existing authorization-before-payload
/// error ordering is untouched. Because it is a client-boundary check, replay
/// must not go through this function — this runtime's replay calls
/// `authorize_sender`/`on_message_at` directly and never reaches here.
///
/// A durable consumer that bypasses this helper (as the runtime does, to
/// interpose its append between validation and commit) MUST call
/// [`Mode::validate_client_envelope`] itself.
pub fn validate_message(
    session: &Session,
    env: &Envelope,
    mode: &dyn Mode,
) -> Result<ModeResponse, MacpError> {
    mode.authorize_sender(session, env)?;
    mode.validate_client_envelope(session, env)?;
    mode.on_message(session, env)
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

/// Outcome of [`step`].
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    /// `message_id` already accepted — nothing changed.
    Duplicate,
    /// Message validated, committed, and applied; carries the resulting state.
    Accepted { state: SessionState },
}

/// All-in-one per-message step for in-memory consumers: preconditions → mode
/// validation → commit, mirroring the runtime's external contract. Expiry marks
/// the session `Expired` and returns [`MacpError::TtlExpired`]; a duplicate is
/// reported as [`StepOutcome::Duplicate`]; any other rejection returns its error
/// without consuming a dedup slot or applying state.
///
/// A durable consumer should instead use [`check_preconditions`],
/// [`validate_message`], and [`commit`] so it can interpose its append-only
/// write between validation and commit (see the runtime's `process_message`).
pub fn step(
    session: &mut Session,
    env: &Envelope,
    mode: &dyn Mode,
    now_ms: i64,
) -> Result<StepOutcome, MacpError> {
    match check_preconditions(session, env, now_ms)? {
        Precheck::Duplicate => Ok(StepOutcome::Duplicate),
        Precheck::Expired => {
            session.state = SessionState::Expired;
            Err(MacpError::TtlExpired)
        }
        Precheck::Proceed => {
            let response = validate_message(session, env, mode)?;
            let state = commit(session, env, response, now_ms);
            Ok(StepOutcome::Accepted { state })
        }
    }
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
                state: SessionState::Open
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
                state: SessionState::Open
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
                state: SessionState::Resolved
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
                state: SessionState::Open
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

        let err = validate_message(&s, &env("agent://a", "Msg", "reserved"), &mode).unwrap_err();
        assert!(matches!(err, MacpError::InvalidEnvelope));
        assert!(
            !mode.dispatched(),
            "the boundary must reject before dispatch"
        );

        // Ordering: unauthorized *and* reserved reports the authorization
        // error, because the hook runs after `authorize_sender`.
        let err =
            validate_message(&s, &env("agent://stranger", "Msg", "reserved"), &mode).unwrap_err();
        assert!(matches!(err, MacpError::Forbidden));

        // A non-reserved id from the same sender passes the boundary and
        // reaches dispatch, so the rejection above was the hook's doing.
        validate_message(&s, &env("agent://a", "Msg", "m1"), &mode).unwrap();
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
        let resp = validate_message(&s, &e, &TestMode).unwrap();
        // Nothing applied until commit.
        assert!(s.seen_message_ids.is_empty());
        let state = commit(&mut s, &e, resp, 5);
        assert_eq!(state, SessionState::Open);
        assert!(s.seen_message_ids.contains("m1"));
    }
}
