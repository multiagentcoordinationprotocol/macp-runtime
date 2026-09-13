use crate::mode::util::{
    check_commitment_authority, enforce_commitment_policy, is_declared_participant,
    validate_commitment_payload_for_session,
};
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::handoff_pb::{
    HandoffAcceptPayload, HandoffContextPayload, HandoffDeclinePayload, HandoffOfferPayload,
};
use macp_pb::pb::Envelope;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `message_id` namespace reserved for the runtime-synthesized implicit
/// `HandoffAccept` (RFC-MACP-0010 §5.1(3), which fixes the id as
/// `implicit-accept:<handoff_id>`).
///
/// At semantics rev >= 2 no client-submitted envelope in a handoff session may
/// carry a `message_id` with this prefix, whatever its message type — see
/// [`Mode::validate_client_envelope`]. Reserving the whole prefix (rather than
/// the one exact id) keeps the failure loud: a client that squats the id a
/// future offer would use consumes that dedup slot, after which the runtime's
/// own synthesis would be silently skipped and the session could never reach a
/// `Commitment`. The parties able to do it are the session's own initiator and
/// the offerer, so this is fail-fast conformance, not attack mitigation.
///
/// The match is **case-sensitive**, which is sufficient rather than sloppy: the
/// synthesized id is always built lowercase from this const plus the client's
/// own `handoff_id`, so a differently-cased squat (`Implicit-Accept:h1`) can
/// never collide with the id the runtime will later insert and so can never
/// consume its dedup slot. It is accepted as an ordinary client id.
pub const IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX: &str = "implicit-accept:";

/// `reason` carried by the implicit accept, and therefore the offer's
/// `outcome_reason` once it is applied.
///
/// **Byte-frozen.** It lives in serialized `mode_state`, so changing it breaks
/// replay of every already-persisted rev <= 1 history that implicitly
/// accepted — those logs were written by a binary carrying the old literal and
/// cannot be rewritten. The guards that actually catch a change are the
/// hand-built histories in `src/replay.rs`'s test module (eight of them) plus
/// `synthetic_payload_bytes_are_pinned` below. Note it is **not**
/// `assert_replay_equivalence` in `tests/conformance_loader.rs`, as this note
/// first claimed: that compares live against replayed *within one binary*, so
/// both sides move together and a changed literal slips through it. It is also
/// what keeps the synthesized accept
/// (RFC-MACP-0010 §5.1(2)) and the interim in-`Commitment` path below
/// indistinguishable in `mode_state` — the reason both share this one const
/// rather than repeating the literal.
const IMPLICIT_ACCEPT_REASON: &str = "implicit accept (timeout)";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HandoffDisposition {
    Offered,
    Accepted,
    Declined,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffOfferRecord {
    pub handoff_id: String,
    pub target_participant: String,
    pub scope: String,
    pub reason: String,
    pub offered_by: String,
    pub disposition: HandoffDisposition,
    pub accepted_by: Option<String>,
    pub declined_by: Option<String>,
    pub outcome_reason: Option<String>,
    #[serde(default)]
    pub offered_at_ms: i64,
    /// `Session::accumulated_suspended_ms` as it stood when the offer was
    /// recorded — a snapshot, never a live read.
    ///
    /// RFC-MACP-0010 §5.1(1): time the session spends `Suspended` must not
    /// count toward the implicit-accept deadline, and the suspension accrued
    /// *since this offer* is `session.accumulated_suspended_ms` minus this
    /// value. Both terms are on the recorded timeline (suspend/resume replay
    /// from the log), so the subtraction is replay-deterministic.
    ///
    /// Legacy `mode_state` recorded before this field existed deserializes as
    /// `0` (serde default), which is also the value for an offer made on a
    /// session that had never been suspended.
    #[serde(default)]
    pub suspended_ms_at_offer: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffContextRecord {
    pub content_type: String,
    pub context: Vec<u8>,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HandoffState {
    pub offers: BTreeMap<String, HandoffOfferRecord>,
    pub contexts: BTreeMap<String, Vec<HandoffContextRecord>>,
}

pub struct HandoffMode {
    evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>,
}

impl HandoffMode {
    /// Construct the mode with an injected governance policy evaluator.
    pub fn new(evaluator: std::sync::Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        Self { evaluator }
    }

    fn encode_state(state: &HandoffState) -> Vec<u8> {
        crate::mode::util::encode_mode_state(state)
    }

    fn decode_state(data: &[u8]) -> Result<HandoffState, MacpError> {
        crate::mode::util::decode_mode_state(data)
    }

    /// Elapsed time an outstanding offer's `implicit_accept_timeout_ms` is
    /// measured against, selected by the session's semantics revision.
    ///
    /// Rev <= 1 keeps the raw difference verbatim — suspended time included —
    /// so legacy histories replay to the outcome they were accepted with, even
    /// though that outcome violates RFC-MACP-0010 §5.1(1). The correction is
    /// deliberately gated rather than applied to every revision: a history is
    /// only replayable under the semantics it was accepted with, and
    /// retroactively un-accepting an offer a rev-1 session already committed on
    /// would make the log unreplayable. Rev >= 2 gets the corrected deadline;
    /// see [`Self::rev2_elapsed_ms`].
    fn implicit_accept_elapsed_ms(
        session: &Session,
        offer: &HandoffOfferRecord,
        now_ms: i64,
    ) -> i64 {
        if session.semantics_rev >= 2 {
            Self::rev2_elapsed_ms(session, offer, now_ms)
        } else {
            // Deliberately non-saturating, unlike the rev >= 2 arm. Rev 0
            // reads `env.timestamp_unix_ms`, which the server boundary never
            // range-checks, so this subtraction can overflow in principle —
            // but retrofitting saturation here would change rev 0/1 outcomes,
            // which must be preserved exactly. It also adds nothing to the
            // attack surface: rev 0 already lets a client forge elapsed time
            // directly by back-dating the offer envelope, which is the defect
            // rev 1 fixed and rev 0 intentionally keeps.
            now_ms - offer.offered_at_ms
        }
    }

    /// The rev >= 2 elapsed computation: time since the offer, minus the time
    /// the session spent `Suspended` within that window (RFC-MACP-0010
    /// §5.1(1) — a suspended session must not tick toward the implicit-accept
    /// deadline).
    ///
    /// Both terms are on the recorded timeline — `offered_at_ms` is the
    /// acceptance clock, and `accumulated_suspended_ms` is banked from the
    /// recorded `received_at_ms` of the suspend/resume entries — so the result
    /// is replay-deterministic.
    ///
    /// `accumulated_suspended_ms` alone is complete here: there is
    /// deliberately **no** in-flight `now_ms - suspended_at_ms` term, because
    /// this code only ever runs while the session is `Open`, so every pause
    /// that has occurred is already banked. `Session::resume` is the only
    /// writer that ever *increases* `accumulated_suspended_ms` after
    /// construction — the other writer, `SessionBuilder`, only sets it at
    /// construction time from already-banked persisted state (snapshot and
    /// checkpoint loads go through `From<PersistedSession> for Session`) — and
    /// both paths that reach this function refuse to dispatch a message to a
    /// non-`Open` session:
    /// `crate::step::check_preconditions` returns `SessionNotOpen` before the
    /// kernel calls `on_message_at`, and replay skips Incoming entries whose
    /// session is not `Open` (`src/replay.rs`). An in-flight term would
    /// therefore be untestable dead code — see `Session::suspend_cap_exceeded`
    /// for the shape it must take if a *future* caller can observe a suspended
    /// session (e.g. an eager sweep running outside the message path).
    ///
    /// Arithmetic is saturating and the suspension term is floored at zero.
    /// The floor cannot trigger from runtime-written state — the snapshot is
    /// taken from the same monotonically non-decreasing counter this reads —
    /// but if corrupted or hand-edited persisted state ever made the term
    /// negative, adding it back would *inflate* elapsed time and implicitly
    /// accept an offer the target never accepted. Flooring degrades to the
    /// rev-1 arithmetic instead, which is the conservative direction.
    fn rev2_elapsed_ms(session: &Session, offer: &HandoffOfferRecord, now_ms: i64) -> i64 {
        let suspended_since_offer = session
            .accumulated_suspended_ms
            .saturating_sub(offer.suspended_ms_at_offer)
            .max(0);
        now_ms
            .saturating_sub(offer.offered_at_ms)
            .saturating_sub(suspended_since_offer)
    }

    /// The bound governance policy's `acceptance.implicit_accept_timeout_ms`,
    /// or `0` when no policy is bound or its rules do not parse.
    ///
    /// `0` means implicit accept never fires, so an unparseable rules document
    /// degrades to "no implicit accept" rather than to some default timeout.
    /// This mirrors the interim in-`Commitment` path's `unwrap_or_default()`
    /// exactly, deliberately: until that path is retired the two must agree, or
    /// a session could implicitly accept through one and not the other.
    fn implicit_accept_timeout_ms(session: &Session) -> i64 {
        let Some(policy) = session.policy_definition.as_ref() else {
            return 0;
        };
        let rules: macp_core::policy::rules::HandoffPolicyRules =
            serde_json::from_value(policy.rules.clone()).unwrap_or_default();
        rules.acceptance.implicit_accept_timeout_ms as i64
    }

    fn commitment_ready(state: &HandoffState) -> bool {
        state.offers.values().any(|offer| {
            offer.disposition == HandoffDisposition::Accepted
                || offer.disposition == HandoffDisposition::Declined
        })
    }
}

impl Mode for HandoffMode {
    /// RFC-MACP-0010 §5.1(3): a client-submitted `HandoffAccept` carrying
    /// `implicit = true` MUST be rejected, and the synthetic accept's
    /// `message_id` namespace is reserved.
    ///
    /// Gated to `semantics_rev >= 2` so rev <= 1 wire behavior is
    /// byte-identical: a legacy session's client could send an
    /// `implicit-accept:`-prefixed id and be accepted, and its history must
    /// stay replayable under the semantics it was accepted with.
    ///
    /// Two rules, in this order:
    /// 1. any `message_id` in the reserved namespace -> `InvalidEnvelope`
    ///    (checked first, and for every message type: the squat works through
    ///    `SessionStart`, `Commitment` and `HandoffContext` too);
    /// 2. `HandoffAccept` whose payload decodes with `implicit = true` ->
    ///    `InvalidPayload` — the same code `handle_message` returns for the
    ///    same envelope today, so the rev-2 error surface does not shift.
    ///
    /// Rule 2 is what the mode itself will *stop* being able to enforce once
    /// the runtime synthesizes implicit accepts: at rev >= 2 a well-formed
    /// implicit accept with the deterministic `message_id` is exactly what
    /// dispatch must accept on replay, and the mode cannot tell client
    /// provenance from runtime provenance. This boundary can.
    fn validate_client_envelope(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        if session.semantics_rev < 2 {
            return Ok(());
        }
        if env
            .message_id
            .starts_with(IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX)
        {
            return Err(MacpError::InvalidEnvelope);
        }
        if env.message_type == "HandoffAccept" {
            // A payload that does not decode is left to `handle_message`,
            // which rejects it `InvalidPayload` on the same grounds. Deciding
            // it here would only duplicate that.
            if let Ok(payload) = HandoffAcceptPayload::decode(&*env.payload) {
                if payload.implicit {
                    return Err(MacpError::InvalidPayload);
                }
            }
        }
        Ok(())
    }

    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        match env.message_type.as_str() {
            "Commitment" => check_commitment_authority(session, &env.sender),
            // HandoffOffer: only initiator can offer
            "HandoffOffer" if env.sender == session.initiator_sender => Ok(()),
            "HandoffOffer" => Err(MacpError::Forbidden),
            // HandoffContext: any declared participant (on_message enforces offerer match)
            _ if is_declared_participant(&session.participants, &env.sender) => Ok(()),
            _ => Err(MacpError::Forbidden),
        }
    }

    fn on_session_start(
        &self,
        session: &Session,
        _env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        // RFC-MACP-0010 §2 (delegated model): the accepted SessionStart
        // sender IS the current responsibility owner, and §3 binds
        // `participants` as "current owner and eligible targets". Both checks
        // below are stricter than the literal §3 text but follow from the
        // model: the owner must be in the list, alongside ≥1 eligible target.
        // (Unlike Task/Decision/Quorum, initiator membership is intrinsic
        // here — the initiator is a transfer party, not just a coordinator.)
        if session.participants.len() < 2 {
            return Err(MacpError::InvalidPayload);
        }
        if !session
            .participants
            .iter()
            .any(|p| p == &session.initiator_sender)
        {
            return Err(MacpError::InvalidPayload);
        }
        Ok(ModeResponse::PersistState(Self::encode_state(
            &HandoffState::default(),
        )))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        // Legacy clock (semantics rev 0): the client-supplied envelope
        // timestamp. The kernel calls `on_message_at`, which selects the
        // acceptance clock for rev >= 1 sessions; this path remains for
        // legacy-history replay and direct library callers.
        self.handle_message(session, env, env.timestamp_unix_ms)
    }

    fn on_message_at(
        &self,
        session: &Session,
        env: &Envelope,
        ctx: &macp_core::mode::MessageContext,
    ) -> Result<ModeResponse, MacpError> {
        // Rev >= 1: the implicit-accept timeout is measured against the
        // runtime's acceptance clock, which the initiator cannot forge (the
        // envelope timestamp let an initiator post-date a Commitment to
        // finalize an offer the target never accepted). Legacy (rev 0)
        // sessions keep the envelope clock so their histories replay to the
        // same outcome they were accepted with.
        let clock_ms = if session.semantics_rev >= 1 {
            ctx.accepted_at_ms
        } else {
            env.timestamp_unix_ms
        };
        self.handle_message(session, env, clock_ms)
    }

    /// RFC-MACP-0010 §5.1(2)-(3): the synthetic implicit `HandoffAccept`, due
    /// once the outstanding offer's `implicit_accept_timeout_ms` has elapsed in
    /// *unsuspended* session time.
    ///
    /// Gated to `semantics_rev >= 2`. Rev <= 1 sessions get `None` forever and
    /// keep resolving through the interim in-`Commitment` path, so their
    /// histories replay to the outcome they were accepted with.
    ///
    /// # Decision vs. timestamp — two different functions, on purpose
    ///
    /// The **decision** ("has the timeout elapsed?") is the scalar
    /// `implicit_accept_elapsed_ms`, which is exact for this purpose:
    /// `elapsed(T) >= timeout` iff `T >= D`, because every pause banked since
    /// the offer lies inside `[offered_at, T]`.
    ///
    /// Only the **timestamp** needs the interval walk
    /// [`Session::unsuspended_deadline`]. §5.1(3) fixes it at "offer acceptance
    /// time + timeout + suspended time *within the window*", which is **not**
    /// the naive `offered_at + timeout + banked_since_offer`: that form counts
    /// pauses beginning *after* the deadline — fully reachable, since
    /// `SuspendSession`/`ResumeSession` are RPCs and need no session-scoped
    /// message — and it would make a permanently-recorded timestamp depend on
    /// when it happened to be observed.
    ///
    /// The two are consistent in the safe direction: an under-recorded
    /// `suspension_intervals` vec (see `unsuspended_deadline`'s under-count
    /// invariant) can only move `D` earlier, never later, so the
    /// `debug_assert!(D <= now_ms)` below holds even on a legacy rev-2
    /// snapshot.
    ///
    /// # Cost
    ///
    /// One `mode_state` decode per call for handoff sessions with a bound
    /// timeout, and an allocation only when an envelope is actually due. A
    /// cached "next deadline" flag on the session was considered and rejected
    /// as premature: it would need a writer on the resume path, which is not
    /// mode-dispatched at all.
    fn due_synthetic_envelope(&self, session: &Session, now_ms: i64) -> Option<Envelope> {
        if session.semantics_rev < 2 {
            return None;
        }
        let timeout = Self::implicit_accept_timeout_ms(session);
        if timeout <= 0 {
            return None;
        }
        if session.mode_state.is_empty() {
            return None;
        }
        let state = Self::decode_state(&session.mode_state).ok()?;
        // RFC-MACP-0010 §5(5): at most one offer is ever outstanding, and once
        // one is accepted no further offers may be issued — hence `Option`, not
        // a queue. `values()` is a `BTreeMap` walk, so even a corrupted state
        // with two outstanding offers picks deterministically.
        let offer = state
            .offers
            .values()
            .find(|offer| offer.disposition == HandoffDisposition::Offered)?;
        // Unreachable at rev >= 2 (rev >= 1 records the acceptance clock on
        // every offer), but kept: it costs nothing, the interim path has the
        // same guard, and a zero here would otherwise measure the timeout from
        // the epoch.
        if offer.offered_at_ms <= 0 {
            return None;
        }
        if Self::implicit_accept_elapsed_ms(session, offer, now_ms) < timeout {
            return None;
        }
        // Completed pauses only: an in-progress suspension is not in
        // `suspension_intervals`, and a suspended session ticks no unsuspended
        // time anyway. Kernel callers must skip non-`Open` sessions.
        debug_assert!(
            session.suspended_at_ms.is_none(),
            "due_synthetic_envelope must not be asked about a suspended session"
        );
        let deadline = session.unsuspended_deadline(offer.offered_at_ms, timeout);
        debug_assert!(
            deadline <= now_ms,
            "deadline {deadline} is in the future of the observation {now_ms}"
        );
        Some(Envelope {
            macp_version: "1.0".into(),
            mode: session.mode.clone(),
            message_type: "HandoffAccept".into(),
            message_id: format!(
                "{IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX}{}",
                offer.handoff_id.as_str()
            ),
            session_id: session.session_id.clone(),
            sender: offer.target_participant.clone(),
            // §5.1(3) SHOULD, adopted here as a MUST: the recorded clocks are
            // the computed deadline, never the observation time.
            timestamp_unix_ms: deadline,
            payload: HandoffAcceptPayload {
                handoff_id: offer.handoff_id.clone(),
                accepted_by: offer.target_participant.clone(),
                reason: IMPLICIT_ACCEPT_REASON.into(),
                implicit: true,
            }
            .encode_to_vec(),
        })
    }
}

impl HandoffMode {
    fn handle_message(
        &self,
        session: &Session,
        env: &Envelope,
        clock_ms: i64,
    ) -> Result<ModeResponse, MacpError> {
        let mut state = if session.mode_state.is_empty() {
            HandoffState::default()
        } else {
            Self::decode_state(&session.mode_state)?
        };

        match env.message_type.as_str() {
            "HandoffOffer" => {
                let payload = HandoffOfferPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                // RFC-MACP-0010: At most one offer may be outstanding at any time.
                // Once an offer is accepted, no further offers may be issued.
                if payload.handoff_id.is_empty()
                    || payload.target_participant.is_empty()
                    || state.offers.contains_key(&payload.handoff_id)
                    || !is_declared_participant(&session.participants, &payload.target_participant)
                    || payload.target_participant == env.sender
                    || state
                        .offers
                        .values()
                        .any(|o| o.disposition == HandoffDisposition::Offered)
                    || state
                        .offers
                        .values()
                        .any(|o| o.disposition == HandoffDisposition::Accepted)
                {
                    return Err(MacpError::InvalidPayload);
                }
                state.offers.insert(
                    payload.handoff_id.clone(),
                    HandoffOfferRecord {
                        handoff_id: payload.handoff_id,
                        target_participant: payload.target_participant,
                        scope: payload.scope,
                        reason: payload.reason,
                        offered_by: env.sender.clone(),
                        disposition: HandoffDisposition::Offered,
                        accepted_by: None,
                        declined_by: None,
                        outcome_reason: None,
                        // Rev >= 1: record the runtime acceptance clock (the
                        // same value the log entry records, so replay is
                        // identical). The client envelope timestamp is
                        // unvalidated — recording it here let an offering
                        // participant BACK-date the offer and immediately
                        // commit, forging elapsed time past the implicit-
                        // accept timeout (the same attack as post-dating the
                        // commitment, relocated to the offer side).
                        offered_at_ms: if session.semantics_rev >= 1 {
                            clock_ms
                        } else {
                            env.timestamp_unix_ms
                        },
                        // Recorded unconditionally (it is read only under the
                        // rev >= 2 branch of `implicit_accept_elapsed_ms`, so
                        // recording it on every session is behavior-neutral
                        // and keeps the field trustworthy wherever it is
                        // later consulted).
                        suspended_ms_at_offer: session.accumulated_suspended_ms,
                    },
                );
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffContext" => {
                let payload = HandoffContextPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let offer = state
                    .offers
                    .get(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.offered_by != env.sender {
                    return Err(MacpError::Forbidden);
                }
                // RFC-MACP-0010 §2.1: Late context (sent after accept/decline) is
                // permitted as supplementary documentation. No disposition check.
                state
                    .contexts
                    .entry(payload.handoff_id)
                    .or_default()
                    .push(HandoffContextRecord {
                        content_type: payload.content_type,
                        context: payload.context,
                        sender: env.sender.clone(),
                    });
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffAccept" => {
                let payload = HandoffAcceptPayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                if payload.implicit {
                    return self.dispatch_implicit_accept(session, env, payload, state);
                }
                let offer = state
                    .offers
                    .get_mut(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.target_participant != env.sender {
                    return Err(MacpError::Forbidden);
                }
                if !payload.accepted_by.is_empty() && payload.accepted_by != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if offer.disposition != HandoffDisposition::Offered {
                    return Err(MacpError::InvalidPayload);
                }
                offer.disposition = HandoffDisposition::Accepted;
                offer.accepted_by = Some(env.sender.clone());
                offer.outcome_reason = Some(payload.reason);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "HandoffDecline" => {
                let payload = HandoffDeclinePayload::decode(&*env.payload)
                    .map_err(|_| MacpError::InvalidPayload)?;
                let offer = state
                    .offers
                    .get_mut(&payload.handoff_id)
                    .ok_or(MacpError::InvalidPayload)?;
                if offer.target_participant != env.sender {
                    return Err(MacpError::Forbidden);
                }
                if !payload.declined_by.is_empty() && payload.declined_by != env.sender {
                    return Err(MacpError::InvalidPayload);
                }
                if offer.disposition != HandoffDisposition::Offered {
                    return Err(MacpError::InvalidPayload);
                }
                offer.disposition = HandoffDisposition::Declined;
                offer.declined_by = Some(env.sender.clone());
                offer.outcome_reason = Some(payload.reason);
                Ok(ModeResponse::PersistState(Self::encode_state(&state)))
            }
            "Commitment" => {
                let commitment = validate_commitment_payload_for_session(session, &env.payload)?;
                // RFC-MACP-0012: lazy implicit_accept_timeout_ms check
                if let Some(ref policy) = session.policy_definition {
                    let rules: macp_core::policy::rules::HandoffPolicyRules =
                        serde_json::from_value(policy.rules.clone()).unwrap_or_default();
                    if rules.acceptance.implicit_accept_timeout_ms > 0 {
                        // Clock selected by `on_message_at` per the session's
                        // semantics revision: acceptance time (rev >= 1) or the
                        // legacy envelope timestamp (rev 0). Both are
                        // log-recorded, so replay is deterministic either way.
                        let now_ms = clock_ms;
                        let timeout = rules.acceptance.implicit_accept_timeout_ms as i64;
                        for offer in state.offers.values_mut() {
                            if offer.disposition == HandoffDisposition::Offered
                                && offer.offered_at_ms > 0
                                && Self::implicit_accept_elapsed_ms(session, offer, now_ms)
                                    >= timeout
                            {
                                offer.disposition = HandoffDisposition::Accepted;
                                offer.accepted_by = Some(offer.target_participant.clone());
                                offer.outcome_reason = Some(IMPLICIT_ACCEPT_REASON.into());
                            }
                        }
                    }
                }
                if !Self::commitment_ready(&state) {
                    return Err(MacpError::InvalidPayload);
                }
                // Governance policy gate (shared): fail closed, only
                // an explicit Allow proceeds.
                enforce_commitment_policy(
                    session,
                    macp_core::policy::CommitmentMode::Handoff,
                    commitment.outcome_positive,
                    &*self.evaluator,
                )?;
                Ok(ModeResponse::PersistAndResolve {
                    state: Self::encode_state(&state),
                    resolution: env.payload.clone(),
                })
            }
            _ => Err(MacpError::InvalidPayload),
        }
    }

    /// The `HandoffAccept` arm for a payload carrying `implicit = true`.
    ///
    /// **Rev <= 1: reject, unconditionally** — RFC-MACP-0010 §5.1(3), and
    /// today's behavior preserved verbatim so legacy histories replay
    /// bit-identically. No rev <= 1 log can contain such an entry, because no
    /// rev <= 1 runtime ever accepted or synthesized one.
    ///
    /// **Rev >= 2: validate strictly and accept.** The runtime synthesizes this
    /// message into accepted history itself (§5.1(2)), so dispatch MUST accept
    /// the well-formed shape — on replay of the recorded entry, and on the live
    /// emission path. The mode cannot keep clients out here, because it cannot
    /// tell client provenance from runtime provenance; that is
    /// [`Mode::validate_client_envelope`]'s job (§5.1(3) scopes the client
    /// prohibition to submission "via `Send`").
    ///
    /// # This arm MUST NOT re-verify the deadline arithmetic
    ///
    /// It validates identity and shape only — never time. On replay the entry
    /// is dispatched with `ctx.accepted_at_ms = received_at_ms = D`, the
    /// deadline it was emitted at, but `session.accumulated_suspended_ms` at
    /// that point in the replay already includes pauses that happened *after*
    /// D: the `SessionSuspend`/`SessionResume` entries between D and the
    /// message that triggered the synthesis sit **earlier in the log** than the
    /// synthetic entry does (its `received_at_ms` is D, which is why the log's
    /// `received_at_ms` is locally non-monotonic in exactly this case — nothing
    /// orders by it, replay and ordinals are positional). So re-checking
    /// `implicit_accept_elapsed_ms(session, offer, D) >= timeout` would subtract
    /// suspension from outside the window, compute *less* unsuspended time than
    /// the timeout, and reject an entry that was emitted correctly — failing
    /// replay of a valid log.
    ///
    /// [`Self::due_synthetic_envelope`] is the single place the timeout is
    /// decided, once, at emission. Do not add a time check here.
    fn dispatch_implicit_accept(
        &self,
        session: &Session,
        env: &Envelope,
        payload: HandoffAcceptPayload,
        mut state: HandoffState,
    ) -> Result<ModeResponse, MacpError> {
        if session.semantics_rev < 2 {
            return Err(MacpError::InvalidPayload);
        }
        let offer = state
            .offers
            .get_mut(&payload.handoff_id)
            .ok_or(MacpError::InvalidPayload)?;
        // Same code as the explicit arm for the same condition, so the error
        // surface does not depend on the flag.
        if offer.target_participant != env.sender {
            return Err(MacpError::Forbidden);
        }
        // Stricter than the explicit arm, which tolerates an empty
        // `accepted_by`: the synthetic envelope always names the target
        // explicitly, so anything else is not the envelope this runtime emits.
        if payload.accepted_by != offer.target_participant {
            return Err(MacpError::InvalidPayload);
        }
        // The deterministic id from §5.1(3). Reserved at the client boundary at
        // rev >= 2, so an envelope that reaches here carrying it is either
        // replayed history or the kernel's own synthesis.
        if env.message_id
            != format!(
                "{IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX}{}",
                offer.handoff_id.as_str()
            )
        {
            return Err(MacpError::InvalidPayload);
        }
        if offer.disposition != HandoffDisposition::Offered {
            return Err(MacpError::InvalidPayload);
        }
        // Exactly the mutation an explicit accept applies, and exactly the one
        // the interim in-`Commitment` path applies — so a history carrying the
        // synthetic entry rebuilds byte-identical `mode_state`.
        //
        // That byte-identity is guaranteed by `due_synthetic_envelope`, which
        // is what supplies `IMPLICIT_ACCEPT_REASON`; this arm takes whatever
        // `reason` the payload carries, because RFC-MACP-0010 makes the
        // payload authoritative. A direct library caller at rev >= 2 can
        // therefore hand-build a well-formed implicit accept with some other
        // `reason` and land non-canonical `mode_state`. Unreachable through
        // the wire (the 11c client boundary refuses the reserved id), and not
        // this arm's job to police.
        offer.disposition = HandoffDisposition::Accepted;
        offer.accepted_by = Some(offer.target_participant.clone());
        offer.outcome_reason = Some(payload.reason);
        Ok(ModeResponse::PersistState(Self::encode_state(&state)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::session::Session;
    use macp_pb::pb::CommitmentPayload;

    fn base_session() -> Session {
        Session::builder("s1", "macp.mode.handoff.v1", "owner")
            .ttl_ms(60_000)
            .participants(vec!["owner".into(), "target".into()])
            .mode_version("1.0.0")
            .configuration_version("config")
            .policy_version("policy")
            .build()
    }

    fn env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "macp.mode.handoff.v1".into(),
            message_type: message_type.into(),
            message_id: format!("{}-{}", sender, message_type),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
            payload,
        }
    }

    fn commitment_payload() -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "accepted".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec()
    }

    fn apply(session: &mut Session, result: ModeResponse) {
        match result {
            ModeResponse::PersistState(data) => session.mode_state = data,
            ModeResponse::PersistAndResolve { state, .. } => session.mode_state = state,
            _ => {}
        }
    }

    fn make_offer(handoff_id: &str, target: &str) -> Vec<u8> {
        HandoffOfferPayload {
            handoff_id: handoff_id.into(),
            target_participant: target.into(),
            scope: "support".into(),
            reason: "escalate".into(),
        }
        .encode_to_vec()
    }

    fn make_context(handoff_id: &str) -> Vec<u8> {
        HandoffContextPayload {
            handoff_id: handoff_id.into(),
            content_type: "text/plain".into(),
            context: b"background info".to_vec(),
        }
        .encode_to_vec()
    }

    fn make_accept(handoff_id: &str, accepted_by: &str) -> Vec<u8> {
        HandoffAcceptPayload {
            handoff_id: handoff_id.into(),
            accepted_by: accepted_by.into(),
            reason: "ready".into(),
            implicit: false,
        }
        .encode_to_vec()
    }

    fn make_decline(handoff_id: &str, declined_by: &str) -> Vec<u8> {
        HandoffDeclinePayload {
            handoff_id: handoff_id.into(),
            declined_by: declined_by.into(),
            reason: "busy".into(),
        }
        .encode_to_vec()
    }

    // --- Session Start ---

    #[test]
    fn session_start_initializes_state() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert!(state.offers.is_empty());
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn session_start_requires_two_participants() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into()]; // only 1
        let err = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn session_start_rejects_when_initiator_not_participant() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["target".into(), "other".into()]; // owner not included
        let err = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- HandoffOffer ---

    #[test]
    fn offer_creates_entry() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert!(state.offers.contains_key("h1"));
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Offered);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn duplicate_offer_id_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn offer_to_self_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "owner")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn offer_to_non_participant_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "outsider")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- HandoffContext ---

    #[test]
    fn context_for_existing_offer() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffContext", make_context("h1")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.contexts["h1"].len(), 1);
                assert_eq!(state.contexts["h1"][0].content_type, "text/plain");
                assert_eq!(state.contexts["h1"][0].sender, "owner");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn context_from_non_offerer_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("target", "HandoffContext", make_context("h1")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    // --- HandoffAccept / HandoffDecline ---

    #[test]
    fn target_can_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Accepted);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    /// A `HandoffAccept` carrying `implicit = true` under a `message_id`
    /// **outside** the reserved namespace is rejected `InvalidPayload`.
    ///
    /// This test used to be named `client_submitted_implicit_accept_is_rejected`
    /// and claimed to prove RFC-MACP-0010 §5.1(3)'s client prohibition. Since
    /// 11d it does not: at `semantics_rev >= 2` dispatch *must* accept a
    /// well-formed implicit accept (it is what replay feeds back in), so what
    /// still fails here is the `message_id` check — the `env()` helper produces
    /// `"target-HandoffAccept"`, not `"implicit-accept:h1"`. Renamed rather than
    /// left green under a false name.
    ///
    /// The §5.1(3) claim is pinned in the two places it still holds:
    /// - the **rev-1 arm below**, where the flag rejection is still
    ///   unconditional whatever the `message_id`;
    /// - at the **client boundary** for rev >= 2 —
    ///   `client_implicit_accept_rejected_at_the_boundary` case (b) submits the
    ///   fully well-formed synthetic shape, reserved `message_id` included, and
    ///   the boundary refuses it. That is the only layer that can, because the
    ///   mode cannot tell client provenance from runtime provenance.
    #[test]
    fn implicit_accept_with_a_non_reserved_message_id_is_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let forged_accept = HandoffAcceptPayload {
            handoff_id: "h1".into(),
            accepted_by: "target".into(),
            reason: "ready".into(),
            implicit: true,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("target", "HandoffAccept", forged_accept))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // Rev <= 1: the flag rejection is still UNCONDITIONAL — the original
        // §5.1(3) claim, preserved at the revision where it holds. Even the
        // otherwise perfect synthetic shape (reserved `message_id`, right
        // sender, right `accepted_by`) is refused, because no rev <= 1 runtime
        // ever synthesized one and no rev <= 1 log can contain one.
        let mut legacy = session.clone();
        legacy.semantics_rev = 1;
        let mut perfect = env(
            "target",
            "HandoffAccept",
            make_implicit_accept("h1", "target"),
        );
        perfect.message_id = reserved_id("h1");
        assert_eq!(
            mode.on_message(&legacy, &perfect).unwrap_err().to_string(),
            "InvalidPayload"
        );
        // ... while at the current revision the identical envelope is
        // ACCEPTED through dispatch (see `implicit_accept_dispatch_accepted_at_rev2`).
        // The differential is the point: without it, deleting the rev gate
        // would leave this test green.
        assert!(mode.on_message(&session, &perfect).is_ok());
    }

    #[test]
    fn wrong_target_cannot_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffAccept", make_accept("h1", "owner")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn target_can_decline() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: HandoffState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Declined);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn cannot_accept_already_accepted() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Commitment ---

    #[test]
    fn commitment_after_accept() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_after_decline() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    #[test]
    fn commitment_without_response_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn commitment_with_no_offers_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Full lifecycle ---

    #[test]
    fn full_handoff_lifecycle() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffContext", make_context("h1")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Serial offer enforcement ---

    #[test]
    fn second_offer_while_first_pending_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn second_offer_after_first_accepted_is_rejected() {
        // RFC-MACP-0010: "Once an offer is accepted, no further offers may be issued."
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn second_offer_after_first_declined_succeeds() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffDecline", make_decline("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        mode.on_message(
            &session,
            &env("owner", "HandoffOffer", make_offer("h2", "other")),
        )
        .unwrap();
    }

    // --- Commitment version mismatch ---

    #[test]
    fn commitment_version_mismatch_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let bad_commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "accepted".into(),
            mode_version: "wrong".into(),
            policy_version: "policy".into(),
            configuration_version: "config".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let err = mode
            .on_message(&session, &env("owner", "Commitment", bad_commitment))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- Unknown message type ---

    #[test]
    fn unknown_message_type_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let err = mode
            .on_message(&session, &env("owner", "CustomType", vec![]))
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn context_after_accept_is_permitted() {
        // RFC-MACP-0010 §2.1: Late context after accept/decline is permitted
        // as supplementary documentation.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let resp = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, resp);
        let resp = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, resp);
        // Late context after accept should succeed
        let result = mode.on_message(
            &session,
            &env("owner", "HandoffContext", make_context("h1")),
        );
        assert!(
            result.is_ok(),
            "late HandoffContext should be permitted per RFC"
        );
    }

    // --- Policy ---

    #[test]
    fn handoff_policy_evaluator_always_allows() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "test-handoff".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "handoff policy".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 0 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        // Handoff policy evaluator always allows — commitment should succeed
        let result = mode
            .on_message(&session, &env("owner", "Commitment", commitment_payload()))
            .unwrap();
        assert!(matches!(result, ModeResponse::PersistAndResolve { .. }));
    }

    // --- Second HandoffOffer while first pending ---

    #[test]
    fn second_offer_to_different_target_while_first_pending_rejected() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "targetA".into(), "targetB".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // First offer to targetA — succeeds
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "targetA")),
            )
            .unwrap();
        apply(&mut session, result);
        // Second offer to targetB while h1 is still pending — rejected
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "targetB")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    // --- After HandoffAccept, further offers are allowed (prior resolved) ---

    #[test]
    fn offer_after_accept_blocked_per_rfc() {
        // RFC-MACP-0010: "Once an offer is accepted, no further offers may be issued
        // for the Session. Only one final Commitment may resolve the Session."
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into(), "other".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("target", "HandoffAccept", make_accept("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        // New HandoffOffer MUST be rejected after an offer has been accepted
        let err = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h2", "other")),
            )
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(state.offers.len(), 1);
        assert_eq!(state.offers["h1"].disposition, HandoffDisposition::Accepted);
    }

    #[test]
    fn offered_at_ms_is_populated() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into()];
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let result = mode
            .on_message(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
            )
            .unwrap();
        apply(&mut session, result);
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert!(
            state.offers["h1"].offered_at_ms > 0,
            "offered_at_ms should be set"
        );
    }

    #[test]
    fn implicit_accept_timeout_fires() {
        // RFC-MACP-0010: when implicit_accept_timeout_ms policy is set and
        // sufficient time has elapsed, the offer is auto-accepted at commitment.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "auto-accept".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "short timeout".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 100 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        });
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        // Offer with a specific timestamp
        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);
        // Commitment with timestamp past the timeout (offer_time + 100ms = 1100)
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 200; // well past 100ms timeout
        let commit = mode.on_message(&session, &commit_env).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    fn auto_accept_policy() -> macp_core::policy::PolicyDefinition {
        macp_core::policy::PolicyDefinition {
            policy_id: "auto-accept".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "short timeout".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 100 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        }
    }

    /// Semantics rev >= 1: the implicit-accept timeout is measured against the
    /// runtime acceptance clock, so an initiator post-dating the Commitment
    /// envelope can no longer finalize an offer the target never accepted.
    #[test]
    fn implicit_accept_ignores_forged_envelope_timestamp_on_rev1() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        assert!(session.semantics_rev >= 1, "builder default is current rev");
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);

        // Initiator forges a far-future envelope timestamp, but the runtime's
        // acceptance clock says only 50ms elapsed: no implicit accept, and the
        // commitment is not ready (no accepted offer) -> rejected.
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 1_000_000;
        let ctx = macp_core::mode::MessageContext::new(offer_time + 50);
        let err = mode.on_message_at(&session, &commit_env, &ctx).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // With genuine elapsed acceptance time past the timeout, it fires.
        let ctx = macp_core::mode::MessageContext::new(offer_time + 200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// Legacy sessions (rev 0) keep the envelope-timestamp clock through the
    /// kernel entry point, so pre-fix histories replay to the outcome they
    /// were accepted with.
    #[test]
    fn implicit_accept_legacy_rev0_keeps_envelope_clock() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.semantics_rev = 0;
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let offer_time = 1000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = offer_time;
        let result = mode.on_message(&session, &offer_env).unwrap();
        apply(&mut session, result);

        // Legacy semantics: the envelope timestamp drives the timeout even
        // when the acceptance clock disagrees (as it did before the fix).
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = offer_time + 200;
        let ctx = macp_core::mode::MessageContext::new(offer_time + 10);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// The offer-side twin of the forged-commitment test: on rev >= 1 the
    /// offer time is the runtime acceptance clock, so BACK-dating the
    /// HandoffOffer envelope no longer forges elapsed time past the
    /// implicit-accept timeout.
    #[test]
    fn implicit_accept_ignores_backdated_offer_timestamp_on_rev1() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        assert!(session.semantics_rev >= 1);
        session.participants = vec!["owner".into(), "target".into()];
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        // Offer envelope BACK-dated far into the past, but accepted "now".
        let now = 1_000_000i64;
        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = now - 1_000_000; // forged past
        let ctx = macp_core::mode::MessageContext::new(now);
        let result = mode.on_message_at(&session, &offer_env, &ctx).unwrap();
        apply(&mut session, result);

        // Commitment accepted 50ms later: elapsed (per the acceptance clock)
        // is 50ms < 100ms timeout — no implicit accept, commitment rejected.
        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = now + 50;
        let ctx = macp_core::mode::MessageContext::new(now + 50);
        let err = mode.on_message_at(&session, &commit_env, &ctx).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // With genuinely elapsed acceptance time, it fires.
        let ctx = macp_core::mode::MessageContext::new(now + 200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// The offer snapshots the session's cumulative suspension so the rev >= 2
    /// deadline can later subtract only the suspension accrued *after* the
    /// offer. Snapshot, not a live read.
    #[test]
    fn offer_records_suspension_snapshot() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        // The session was already suspended once before the offer landed.
        session.accumulated_suspended_ms = 5_000;
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let ctx = macp_core::mode::MessageContext::new(1_000);
        let result = mode
            .on_message_at(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
                &ctx,
            )
            .unwrap();
        apply(&mut session, result);

        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(state.offers["h1"].suspended_ms_at_offer, 5_000);
    }

    /// Legacy-state fixture: `mode_state` written before the snapshot field
    /// existed must still decode (serde default) and must still reach the
    /// outcome it was accepted with — the implicit-accept timeout fires off
    /// the recorded `offered_at_ms` exactly as before.
    #[test]
    fn legacy_offer_mode_state_without_suspension_snapshot_replays_unchanged() {
        let legacy_state = serde_json::json!({
            "offers": {
                "h1": {
                    "handoff_id": "h1",
                    "target_participant": "target",
                    "scope": "support",
                    "reason": "escalate",
                    "offered_by": "owner",
                    "disposition": "Offered",
                    "accepted_by": null,
                    "declined_by": null,
                    "outcome_reason": null,
                    "offered_at_ms": 1000
                }
            },
            "contexts": {}
        });
        let decoded: HandoffState = serde_json::from_value(legacy_state.clone()).unwrap();
        assert_eq!(decoded.offers["h1"].suspended_ms_at_offer, 0);
        assert_eq!(decoded.offers["h1"].offered_at_ms, 1000);

        // And it still drives the original outcome: 200ms of elapsed
        // acceptance time past a 100ms timeout implicitly accepts, so the
        // commitment resolves.
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(auto_accept_policy());
        session.mode_state = serde_json::to_vec(&legacy_state).unwrap();
        let commit_env = env("owner", "Commitment", commitment_payload());
        let ctx = macp_core::mode::MessageContext::new(1_200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        apply(&mut session, commit);
        let state: HandoffState = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(
            state.offers["h1"].outcome_reason.as_deref(),
            Some("implicit accept (timeout)")
        );
    }

    /// The offer time both revision tests anchor on.
    const OFFER_TIME_MS: i64 = 1_000;

    /// Drive one `offer -> [suspend/resume] -> Commitment` sequence under a
    /// chosen semantics revision and report whether the commitment resolved
    /// the session. Under [`auto_accept_policy`] the target never accepts
    /// explicitly, so `Ok(true)` can only mean the offer was implicitly
    /// accepted; `Err("InvalidPayload")` is the no-accepted-offer rejection.
    ///
    /// Both envelope timestamps are pinned to the acceptance clocks so the two
    /// clocks agree — that isolates the suspension term as the only thing the
    /// revision can change, and lets rev 0 (envelope clock) be driven here
    /// too.
    ///
    /// The pause runs through the real `Session::suspend`/`resume` pair, so
    /// `accumulated_suspended_ms` is banked exactly the way the kernel
    /// (`RuntimeCore::resume_session`) and replay (`replay_entry`'s
    /// `SessionResume` arm) bank it.
    fn implicit_accept_outcome(
        rev: u32,
        suspended_after_offer_ms: i64,
        commit_at: i64,
    ) -> Result<bool, String> {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.semantics_rev = rev;
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let mut offer_env = env("owner", "HandoffOffer", make_offer("h1", "target"));
        offer_env.timestamp_unix_ms = OFFER_TIME_MS;
        let offer_ctx = macp_core::mode::MessageContext::new(OFFER_TIME_MS);
        let result = mode
            .on_message_at(&session, &offer_env, &offer_ctx)
            .unwrap();
        apply(&mut session, result);

        // The pause happens between the offer and the commitment. Messages are
        // refused while suspended (`crate::step::check_preconditions`), so by
        // the time the commitment is processed the pause is always banked and
        // `suspended_at_ms` is back to `None`.
        if suspended_after_offer_ms > 0 {
            session.suspend(OFFER_TIME_MS).unwrap();
            session
                .resume(OFFER_TIME_MS + suspended_after_offer_ms)
                .unwrap();
            assert_eq!(session.accumulated_suspended_ms, suspended_after_offer_ms);
            assert_eq!(session.suspended_at_ms, None);
        }

        let mut commit_env = env("owner", "Commitment", commitment_payload());
        commit_env.timestamp_unix_ms = commit_at;
        let ctx = macp_core::mode::MessageContext::new(commit_at);
        mode.on_message_at(&session, &commit_env, &ctx)
            .map(|r| matches!(r, ModeResponse::PersistAndResolve { .. }))
            .map_err(|e| e.to_string())
    }

    /// RFC-MACP-0010 §5.1(1): time the session spends `Suspended` must not
    /// count toward `implicit_accept_timeout_ms`. Rev 2 honors that; revs 0
    /// and 1 kept counting it and MUST keep counting it, or histories they
    /// already resolved stop replaying.
    ///
    /// The offer is outstanding for 300ms, 250ms of it suspended: 50ms of live
    /// time against the policy's 100ms timeout.
    #[test]
    fn rev2_stops_counting_suspended_time_toward_implicit_accept() {
        let commit_at = OFFER_TIME_MS + 300;
        assert_eq!(
            implicit_accept_outcome(2, 250, commit_at),
            Err("InvalidPayload".into()),
            "rev 2: 50ms of unsuspended time must not implicitly accept"
        );
        assert_eq!(
            implicit_accept_outcome(1, 250, commit_at),
            Ok(true),
            "rev 1 must keep the legacy arithmetic exactly (suspended time counts)"
        );
        assert_eq!(
            implicit_accept_outcome(0, 250, commit_at),
            Ok(true),
            "rev 0 must keep the legacy arithmetic exactly (suspended time counts)"
        );
    }

    /// The correction changes *only* the suspended case: with no suspension
    /// every revision agrees, and a suspension that still leaves the timeout
    /// cleared on live time alone accepts under rev 2 as well — including
    /// exactly at the boundary, since the comparison stays `>=`.
    #[test]
    fn rev2_matches_legacy_arithmetic_when_nothing_was_suspended() {
        for (suspended, commit_at, expected) in [
            // No suspension at all: identical to rev 1 either side of the timeout.
            (0i64, OFFER_TIME_MS + 200, Ok(true)),
            (0, OFFER_TIME_MS + 50, Err("InvalidPayload".to_string())),
            // 200ms suspended out of 300ms: exactly 100ms live == the timeout.
            (200, OFFER_TIME_MS + 300, Ok(true)),
            // One millisecond short of the boundary.
            (201, OFFER_TIME_MS + 300, Err("InvalidPayload".to_string())),
        ] {
            assert_eq!(
                implicit_accept_outcome(2, suspended, commit_at),
                expected,
                "rev 2 (suspended={suspended}, commit_at={commit_at})"
            );
            if suspended == 0 {
                assert_eq!(
                    implicit_accept_outcome(1, suspended, commit_at),
                    expected,
                    "rev 1 must agree when nothing was suspended"
                );
            }
        }
    }

    /// Only suspension accrued *after* the offer is excluded. A session that
    /// was paused before the offer was ever made has that pause in
    /// `accumulated_suspended_ms`, and subtracting it would push the deadline
    /// out for a window the offer did not exist in — which is why the offer
    /// snapshots the counter.
    #[test]
    fn rev2_subtracts_only_suspension_accrued_after_the_offer() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(auto_accept_policy());
        // A 5s pause that ended before the offer was made.
        session.accumulated_suspended_ms = 5_000;
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        let offer_ctx = macp_core::mode::MessageContext::new(OFFER_TIME_MS);
        let result = mode
            .on_message_at(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
                &offer_ctx,
            )
            .unwrap();
        apply(&mut session, result);

        // 200ms of live time, nothing suspended since the offer: accepts.
        let commit_env = env("owner", "Commitment", commitment_payload());
        let ctx = macp_core::mode::MessageContext::new(OFFER_TIME_MS + 200);
        let commit = mode.on_message_at(&session, &commit_env, &ctx).unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));
    }

    /// Unit-level guards on the rev-2 arithmetic itself. Neither input is
    /// reachable from runtime-written state (`accumulated_suspended_ms` only
    /// ever grows, and the snapshot is taken from that same counter), so these
    /// pin the behavior for corrupted or hand-edited persisted state: degrade
    /// to the legacy difference, never inflate elapsed time, never panic.
    #[test]
    fn rev2_elapsed_ms_is_saturating_and_floors_the_suspension_term() {
        let mut session = base_session();
        let mut offer = HandoffOfferRecord {
            handoff_id: "h1".into(),
            target_participant: "target".into(),
            scope: "support".into(),
            reason: "escalate".into(),
            offered_by: "owner".into(),
            disposition: HandoffDisposition::Offered,
            accepted_by: None,
            declined_by: None,
            outcome_reason: None,
            offered_at_ms: 1_000,
            suspended_ms_at_offer: 0,
        };

        // Baseline: no suspension since the offer -> the raw difference.
        assert_eq!(HandoffMode::rev2_elapsed_ms(&session, &offer, 1_300), 300);

        // A negative suspension term must NOT be added back (that would
        // implicitly accept an offer the target never accepted); it floors to
        // the legacy difference.
        offer.suspended_ms_at_offer = 5_000;
        session.accumulated_suspended_ms = 1_000;
        assert_eq!(HandoffMode::rev2_elapsed_ms(&session, &offer, 1_300), 300);

        // Overflow in either subtraction saturates instead of panicking.
        offer.suspended_ms_at_offer = 0;
        session.accumulated_suspended_ms = 0;
        offer.offered_at_ms = i64::MIN;
        assert_eq!(
            HandoffMode::rev2_elapsed_ms(&session, &offer, i64::MAX),
            i64::MAX
        );
        offer.offered_at_ms = 0;
        session.accumulated_suspended_ms = i64::MAX;
        assert_eq!(
            HandoffMode::rev2_elapsed_ms(&session, &offer, i64::MIN),
            i64::MIN
        );
    }

    /// The rev >= 2 branch is wired to the live constant, not to a hard-coded
    /// 2: a session built today is on the current revision and takes that
    /// branch.
    #[test]
    fn builder_default_session_is_on_current_semantics_rev() {
        assert_eq!(
            base_session().semantics_rev,
            macp_core::session::CURRENT_SEMANTICS_REV
        );
        // Compile-time: the rev >= 2 branch is reachable for new sessions at
        // all only while the constant stays there.
        const _: () = assert!(macp_core::session::CURRENT_SEMANTICS_REV >= 2);
    }

    // --- The client boundary: `validate_client_envelope` (Phase 11c) ---
    //
    // RFC-MACP-0010 §5.1(3). The runtime-level halves of these criteria live
    // in `src/runtime.rs`
    // (`reserved_message_id_namespace_is_rejected_at_rev2`,
    // `reserved_message_id_is_rejected_on_the_session_start_path`,
    // `client_implicit_accept_rejected_through_the_runtime`), and the proof
    // that the hook is NOT on the replay path lives in `src/replay.rs`
    // (`reserved_prefix_entry_replays_at_every_rev`).

    fn make_implicit_accept(handoff_id: &str, accepted_by: &str) -> Vec<u8> {
        HandoffAcceptPayload {
            handoff_id: handoff_id.into(),
            accepted_by: accepted_by.into(),
            reason: "implicit accept (timeout)".into(),
            implicit: true,
        }
        .encode_to_vec()
    }

    fn reserved_id(handoff_id: &str) -> String {
        format!("{IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX}{handoff_id}")
    }

    /// A client-submitted `HandoffAccept` carrying `implicit = true` is
    /// rejected at the boundary (RFC-MACP-0010 §5.1(3) MUST).
    ///
    /// **Two envelopes, deliberately**, so neither rule can pass for the
    /// other's reason:
    /// (a) `implicit = true` with the natural (non-reserved) `message_id` —
    ///     isolates the `implicit` rule, `InvalidPayload` (the same code
    ///     `handle_message` returns for the same envelope today, so the rev-2
    ///     error surface does not shift);
    /// (b) `implicit = true` with the **reserved** `message_id` and correct
    ///     sender/`accepted_by` — byte-for-byte the envelope the runtime will
    ///     synthesize from 11e, and therefore the only place the flag's
    ///     *client provenance* can be pinned once 11d requires dispatch to
    ///     accept exactly that shape. It reports `InvalidEnvelope`, not
    ///     `InvalidPayload`, because the reserved-namespace rule is checked
    ///     first — asserted rather than glossed, since the ordering is what
    ///     makes this assertion mutation-sensitive after 11d lands.
    #[test]
    fn client_implicit_accept_rejected_at_the_boundary() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();

        // (a) the `implicit` rule, isolated.
        let mut a = env(
            "target",
            "HandoffAccept",
            make_implicit_accept("h1", "target"),
        );
        assert!(
            !a.message_id.starts_with(IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX),
            "case (a) must not also trip the reserved-namespace rule"
        );
        assert!(matches!(
            mode.validate_client_envelope(&session, &a).unwrap_err(),
            MacpError::InvalidPayload
        ));

        // Same envelope with `implicit = false` passes the boundary: the rule
        // discriminates on the flag, not on the message type.
        a.payload = make_accept("h1", "target");
        assert!(mode.validate_client_envelope(&session, &a).is_ok());

        // (b) the runtime's own synthetic shape, submitted by a client.
        let mut b = env(
            "target",
            "HandoffAccept",
            make_implicit_accept("h1", "target"),
        );
        b.message_id = reserved_id("h1");
        assert!(matches!(
            mode.validate_client_envelope(&session, &b).unwrap_err(),
            MacpError::InvalidEnvelope
        ));
    }

    /// The reserved namespace is reserved for **every** message type, not just
    /// `HandoffAccept`: the squat works through `SessionStart` (initiator),
    /// `Commitment` (commitment authority) and `HandoffContext` (the offerer)
    /// too, and consuming that dedup slot would make the runtime's own later
    /// synthesis silently skipped.
    ///
    /// Also pins that it is a **prefix** reservation, not one exact id: the
    /// squattable id is the one a *future* offer would use, which the boundary
    /// cannot enumerate.
    #[test]
    fn reserved_prefix_is_rejected_for_every_message_type() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let session = base_session();

        for message_type in [
            "SessionStart",
            "Commitment",
            "HandoffOffer",
            "HandoffContext",
            "HandoffAccept",
            "HandoffDecline",
            "SomeUnknownType",
        ] {
            for suffix in ["h1", "", "a-handoff-id-that-does-not-exist-yet"] {
                let mut e = env("owner", message_type, vec![]);
                e.message_id = format!("{IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX}{suffix}");
                assert!(
                    matches!(
                        mode.validate_client_envelope(&session, &e).unwrap_err(),
                        MacpError::InvalidEnvelope
                    ),
                    "{message_type} with id {} must be rejected",
                    e.message_id
                );
            }

            // A near-miss id is untouched: the reservation is the prefix, and
            // nothing wider.
            let mut ok = env("owner", message_type, vec![]);
            ok.message_id = "implicit-accept".into(); // no trailing colon
            assert!(
                mode.validate_client_envelope(&session, &ok).is_ok(),
                "{message_type} with a near-miss id must pass"
            );
        }
    }

    /// Rev <= 1 is untouched, so legacy histories replay bit-identically: the
    /// hook returns `Ok` for both rules on a legacy session, while the
    /// identical envelopes are rejected at the current revision.
    ///
    /// The differential half is the point — without it, deleting the rev gate
    /// would leave this test green.
    #[test]
    fn reserved_namespace_is_rev_gated() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));

        let mut reserved = env("owner", "HandoffContext", make_context("h1"));
        reserved.message_id = reserved_id("h1");
        let implicit = env(
            "target",
            "HandoffAccept",
            make_implicit_accept("h1", "target"),
        );

        for rev in [0, 1] {
            let mut legacy = base_session();
            legacy.semantics_rev = rev;
            assert!(
                mode.validate_client_envelope(&legacy, &reserved).is_ok(),
                "rev {rev} must accept a reserved-prefix id"
            );
            assert!(
                mode.validate_client_envelope(&legacy, &implicit).is_ok(),
                "rev {rev} must leave the implicit flag to dispatch"
            );
        }

        let current = base_session();
        assert_eq!(
            current.semantics_rev,
            macp_core::session::CURRENT_SEMANTICS_REV
        );
        assert!(mode.validate_client_envelope(&current, &reserved).is_err());
        assert!(mode.validate_client_envelope(&current, &implicit).is_err());
    }

    /// A `HandoffAccept` whose payload does not decode is deliberately **not**
    /// decided at the boundary — the hook returns `Ok` and `handle_message`
    /// rejects it `InvalidPayload` on its own grounds. Duplicating the
    /// decision here would only give two places to keep in sync.
    #[test]
    fn undecodable_handoff_accept_is_left_to_dispatch() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();

        // A payload that is not a valid `HandoffAcceptPayload` (field 1 is a
        // string here, declared as a group-start wire type).
        let garbage = vec![0xffu8, 0xff, 0xff, 0xff];
        assert!(HandoffAcceptPayload::decode(&*garbage).is_err());

        let e = env("target", "HandoffAccept", garbage);
        assert!(
            mode.validate_client_envelope(&session, &e).is_ok(),
            "the boundary must not decide an undecodable payload"
        );

        // And dispatch still rejects it, so nothing is let through.
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        assert!(matches!(
            mode.on_message(&session, &e).unwrap_err(),
            MacpError::InvalidPayload
        ));
    }

    /// Error ordering is unchanged: an envelope that is both unauthorized and
    /// carries a reserved id reports the **authorization** error. The hook
    /// runs after `authorize_sender` precisely so the pre-existing
    /// Forbidden-before-payload ordering does not shift at rev 2.
    #[test]
    fn client_boundary_runs_after_sender_authorization() {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);

        // `stranger` is not a declared participant -> Forbidden from
        // `authorize_sender`, even though the id is reserved and the payload
        // sets `implicit`.
        let mut e = env(
            "stranger",
            "HandoffAccept",
            make_implicit_accept("h1", "stranger"),
        );
        e.message_id = reserved_id("h1");
        assert!(matches!(
            crate::step::validate_message(&session, &e, &mode).unwrap_err(),
            MacpError::Forbidden
        ));

        // The same envelope from the authorized sender does reach the hook.
        let mut authorized = e.clone();
        authorized.sender = "target".into();
        assert!(matches!(
            crate::step::validate_message(&session, &authorized, &mode).unwrap_err(),
            MacpError::InvalidEnvelope
        ));
    }

    // --- The synthesis contract: `due_synthetic_envelope` (Phase 11d) ---
    //
    // RFC-MACP-0010 §5.1(2)-(3). Nothing calls the hook yet — the kernel
    // wiring is 11e — so these drive it by hand with injected clocks.

    /// A current-revision session with [`auto_accept_policy`] bound and one
    /// outstanding offer `h1` to `target`, accepted at [`OFFER_TIME_MS`].
    fn offered_session() -> (HandoffMode, Session) {
        let mode = HandoffMode::new(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator));
        let mut session = base_session();
        session.policy_definition = Some(auto_accept_policy());
        let result = mode
            .on_session_start(&session, &env("owner", "SessionStart", vec![]))
            .unwrap();
        apply(&mut session, result);
        let ctx = macp_core::mode::MessageContext::new(OFFER_TIME_MS);
        let result = mode
            .on_message_at(
                &session,
                &env("owner", "HandoffOffer", make_offer("h1", "target")),
                &ctx,
            )
            .unwrap();
        apply(&mut session, result);
        (mode, session)
    }

    /// The bound timeout, read back through the production resolver rather
    /// than re-hard-coded from the policy JSON.
    fn bound_timeout_ms(session: &Session) -> i64 {
        let timeout = HandoffMode::implicit_accept_timeout_ms(session);
        assert_eq!(timeout, 100, "auto_accept_policy binds a 100ms timeout");
        timeout
    }

    /// RFC-MACP-0010 §5.1(3): the synthetic accept becomes due exactly when
    /// the offer's timeout has elapsed in *unsuspended* time, and its
    /// `timestamp_unix_ms` is the computed deadline D — not the observation
    /// time, and not the naive
    /// `offered_at + timeout + banked_suspension_since_the_offer`.
    ///
    /// The last case is the one that separates the three: a pause that begins
    /// **after** D must not move D at all, while both wrong implementations
    /// move it.
    #[test]
    fn due_synthetic_envelope_emits_at_the_deadline() {
        let (mode, session) = offered_session();
        let timeout = bound_timeout_ms(&session);
        let deadline = OFFER_TIME_MS + timeout;

        // Not due one millisecond early.
        assert!(mode
            .due_synthetic_envelope(&session, deadline - 1)
            .is_none());

        // Due at D, and at every later observation — with the *same* envelope,
        // because nothing in it may depend on when it was asked.
        let at_deadline = mode
            .due_synthetic_envelope(&session, deadline)
            .expect("due at the deadline");
        let much_later = mode
            .due_synthetic_envelope(&session, deadline + 10_000)
            .expect("still due long after the deadline");
        assert_eq!(at_deadline, much_later);

        // Field by field: the constants §5.1(3) fixes.
        assert_eq!(at_deadline.macp_version, "1.0");
        assert_eq!(at_deadline.mode, session.mode);
        assert_eq!(at_deadline.session_id, session.session_id);
        assert_eq!(at_deadline.message_type, "HandoffAccept");
        assert_eq!(at_deadline.message_id, "implicit-accept:h1");
        assert_eq!(at_deadline.sender, "target");
        assert_eq!(at_deadline.timestamp_unix_ms, deadline);
        let payload = HandoffAcceptPayload::decode(&*at_deadline.payload).unwrap();
        assert_eq!(payload.handoff_id, "h1");
        assert_eq!(payload.accepted_by, "target");
        assert_eq!(payload.reason, IMPLICIT_ACCEPT_REASON);
        assert!(payload.implicit);

        // A pause *inside* the window pushes D out by its width (§5.1(1)):
        // 50ms of live time, a 150ms pause, then the remaining 50ms.
        let mut paused = session.clone();
        paused.suspend(OFFER_TIME_MS + 50).unwrap();
        paused.resume(OFFER_TIME_MS + 200).unwrap();
        assert_eq!(paused.suspension_intervals, vec![(1_050, 1_200)]);
        assert!(mode
            .due_synthetic_envelope(&paused, OFFER_TIME_MS + 249)
            .is_none());
        assert_eq!(
            mode.due_synthetic_envelope(&paused, OFFER_TIME_MS + 250)
                .expect("due at the walked deadline")
                .timestamp_unix_ms,
            OFFER_TIME_MS + 250
        );

        // THE KILLER CASE — a pause that begins *after* D does not move D.
        // §5.1(3) counts "suspended time within the window"; this pause is
        // outside it. The offer's unsuspended time had already hit the timeout
        // at D, so D is what permanent history must record no matter how long
        // afterwards (or how many pauses later) the runtime gets round to
        // looking.
        let mut late_pause = session.clone();
        late_pause.suspend(deadline + 50).unwrap();
        late_pause.resume(deadline + 250).unwrap();
        let observed_at = deadline + 400;
        let late = mode
            .due_synthetic_envelope(&late_pause, observed_at)
            .expect("a pause after the deadline cannot un-due the accept");
        assert_eq!(
            late.timestamp_unix_ms, deadline,
            "a pause beginning after D must not move D"
        );
        // The two wrong implementations this pins out, named so a future
        // refactor cannot reintroduce either by accident:
        assert_ne!(
            late.timestamp_unix_ms, observed_at,
            "observation time is not the deadline"
        );
        assert_eq!(late_pause.accumulated_suspended_ms, 200);
        assert_ne!(
            late.timestamp_unix_ms,
            OFFER_TIME_MS + timeout + late_pause.accumulated_suspended_ms,
            "the naive offered_at + timeout + banked-suspension formula counts \
             pauses outside the window"
        );
    }

    /// Everything that makes an implicit accept *not* due. Each arm is one
    /// guard in `due_synthetic_envelope`.
    #[test]
    fn due_synthetic_envelope_returns_none_unless_an_offer_is_due() {
        let (mode, session) = offered_session();
        let deadline = OFFER_TIME_MS + bound_timeout_ms(&session);
        let long_after = deadline + 1_000_000;

        // Rev gate: legacy sessions never synthesize — they keep resolving
        // through the interim in-`Commitment` path, so their histories replay
        // to the outcome they were accepted with.
        for rev in [0, 1] {
            let mut legacy = session.clone();
            legacy.semantics_rev = rev;
            assert!(
                mode.due_synthetic_envelope(&legacy, long_after).is_none(),
                "rev {rev} must never synthesize"
            );
        }

        // No policy bound at all.
        let mut unbound = session.clone();
        unbound.policy_definition = None;
        assert!(mode.due_synthetic_envelope(&unbound, long_after).is_none());

        // Policy bound but the feature switched off (timeout 0), and rules
        // that do not parse — both degrade to "never", matching the interim
        // path's `unwrap_or_default`.
        for rules in [
            serde_json::json!({ "acceptance": { "implicit_accept_timeout_ms": 0 } }),
            serde_json::json!({ "acceptance": "not an object" }),
        ] {
            let mut off = session.clone();
            off.policy_definition = Some(macp_core::policy::PolicyDefinition {
                policy_id: "off".into(),
                mode: "macp.mode.handoff.v1".into(),
                description: "no implicit accept".into(),
                rules,
                schema_version: 1,
            });
            assert_eq!(HandoffMode::implicit_accept_timeout_ms(&off), 0);
            assert!(mode.due_synthetic_envelope(&off, long_after).is_none());
        }

        // No offer yet (session start only), and undecodable `mode_state`.
        let mut no_offer = session.clone();
        no_offer.mode_state = HandoffMode::encode_state(&HandoffState::default());
        assert!(mode.due_synthetic_envelope(&no_offer, long_after).is_none());
        let mut empty = session.clone();
        empty.mode_state = vec![];
        assert!(mode.due_synthetic_envelope(&empty, long_after).is_none());
        let mut garbage = session.clone();
        garbage.mode_state = b"not json".to_vec();
        assert!(mode.due_synthetic_envelope(&garbage, long_after).is_none());

        // The offer is no longer outstanding.
        for disposition in [HandoffDisposition::Accepted, HandoffDisposition::Declined] {
            let mut settled = session.clone();
            let mut state: HandoffState = serde_json::from_slice(&settled.mode_state).unwrap();
            state.offers.get_mut("h1").unwrap().disposition = disposition;
            settled.mode_state = serde_json::to_vec(&state).unwrap();
            assert!(mode.due_synthetic_envelope(&settled, long_after).is_none());
        }

        // A legacy offer with no acceptance clock recorded (unreachable at
        // rev >= 2, guarded anyway).
        let mut clockless = session.clone();
        let mut state: HandoffState = serde_json::from_slice(&clockless.mode_state).unwrap();
        state.offers.get_mut("h1").unwrap().offered_at_ms = 0;
        clockless.mode_state = serde_json::to_vec(&state).unwrap();
        assert!(mode
            .due_synthetic_envelope(&clockless, long_after)
            .is_none());
    }

    /// The synthetic payload's prost encoding, pinned byte-for-byte. It is
    /// written into permanent history, so a field reordering or a changed
    /// `reason` would silently break byte-identical replay of every log that
    /// carries one — and Phase 12's byte-identity criterion outright.
    #[test]
    fn synthetic_payload_bytes_are_pinned() {
        let (mode, session) = offered_session();
        let deadline = OFFER_TIME_MS + bound_timeout_ms(&session);
        let synthetic = mode
            .due_synthetic_envelope(&session, deadline)
            .expect("due at the deadline");

        let expected: Vec<u8> = [
            b"\x0a\x02h1".as_slice(),             // 1: handoff_id
            b"\x12\x06target",                    // 2: accepted_by
            b"\x1a\x19implicit accept (timeout)", // 3: reason
            b"\x20\x01",                          // 4: implicit = true
        ]
        .concat();
        assert_eq!(synthetic.payload, expected);
    }

    /// Dispatch accepts the synthetic envelope at rev >= 2 — this is what
    /// replay does with the recorded entry — and applies exactly the mutation
    /// the interim in-`Commitment` path applies, so `mode_state` is
    /// byte-identical either way.
    ///
    /// Rejected when malformed in any single field, and at rev 1.
    #[test]
    fn implicit_accept_dispatch_accepted_at_rev2() {
        let (mode, session) = offered_session();
        let deadline = OFFER_TIME_MS + bound_timeout_ms(&session);
        let synthetic = mode
            .due_synthetic_envelope(&session, deadline)
            .expect("due at the deadline");
        // Replay dispatches the entry with `accepted_at_ms == received_at_ms`,
        // which for this entry is D.
        let ctx = macp_core::mode::MessageContext::new(synthetic.timestamp_unix_ms);

        let mut accepted = session.clone();
        let resp = mode.on_message_at(&accepted, &synthetic, &ctx).unwrap();
        apply(&mut accepted, resp);
        let state: HandoffState = serde_json::from_slice(&accepted.mode_state).unwrap();
        let offer = &state.offers["h1"];
        assert_eq!(offer.disposition, HandoffDisposition::Accepted);
        assert_eq!(offer.accepted_by.as_deref(), Some("target"));
        assert_eq!(
            offer.outcome_reason.as_deref(),
            Some(IMPLICIT_ACCEPT_REASON)
        );

        // ... and the commitment the synthesis exists to unblock now resolves.
        let commit = mode
            .on_message_at(
                &accepted,
                &env("owner", "Commitment", commitment_payload()),
                &macp_core::mode::MessageContext::new(deadline + 10),
            )
            .unwrap();
        assert!(matches!(commit, ModeResponse::PersistAndResolve { .. }));

        // Idempotent: nothing is due once the offer is settled, and the same
        // entry cannot be applied twice.
        assert!(mode
            .due_synthetic_envelope(&accepted, deadline + 10_000)
            .is_none());
        assert!(matches!(
            mode.on_message_at(&accepted, &synthetic, &ctx).unwrap_err(),
            MacpError::InvalidPayload
        ));

        // One field off the synthetic shape is one rejection. Each of these
        // would otherwise be an envelope a library caller could hand-build.
        let mut wrong_sender = synthetic.clone();
        wrong_sender.sender = "owner".into();
        assert!(matches!(
            mode.on_message_at(&session, &wrong_sender, &ctx)
                .unwrap_err(),
            MacpError::Forbidden
        ));

        let mut wrong_accepted_by = synthetic.clone();
        wrong_accepted_by.payload = make_implicit_accept("h1", "owner");
        assert!(matches!(
            mode.on_message_at(&session, &wrong_accepted_by, &ctx)
                .unwrap_err(),
            MacpError::InvalidPayload
        ));

        let mut empty_accepted_by = synthetic.clone();
        empty_accepted_by.payload = make_implicit_accept("h1", "");
        assert!(
            matches!(
                mode.on_message_at(&session, &empty_accepted_by, &ctx)
                    .unwrap_err(),
                MacpError::InvalidPayload
            ),
            "stricter than the explicit arm: the synthetic always names the target"
        );

        let mut wrong_id = synthetic.clone();
        wrong_id.message_id = "accept-1".into();
        assert!(matches!(
            mode.on_message_at(&session, &wrong_id, &ctx).unwrap_err(),
            MacpError::InvalidPayload
        ));

        let mut unknown_offer = synthetic.clone();
        unknown_offer.message_id = reserved_id("h2");
        unknown_offer.payload = make_implicit_accept("h2", "target");
        assert!(matches!(
            mode.on_message_at(&session, &unknown_offer, &ctx)
                .unwrap_err(),
            MacpError::InvalidPayload
        ));

        // Rev 1 refuses the identical, perfectly-formed envelope.
        let mut legacy = session.clone();
        legacy.semantics_rev = 1;
        assert!(matches!(
            mode.on_message_at(&legacy, &synthetic, &ctx).unwrap_err(),
            MacpError::InvalidPayload
        ));
    }

    /// The negative rule of this phase, made killable instead of merely
    /// absent: **dispatch must not re-verify the deadline arithmetic.**
    ///
    /// This reproduces the exact replay state that a re-check would fail on. A
    /// pause runs from D+50 to D+250, *after* the deadline, so by the time the
    /// synthetic entry is dispatched on replay the session has already banked
    /// 200ms of suspension — because the `SessionSuspend`/`SessionResume`
    /// entries sit earlier in the log than the synthetic entry does (whose
    /// `received_at_ms` is D). Dispatch sees `accepted_at_ms = D` together with
    /// `accumulated_suspended_ms = 200`, so
    /// `implicit_accept_elapsed_ms(session, offer, D)` is **negative** — a
    /// re-check would reject a correctly-emitted entry and fail replay of a
    /// valid log.
    ///
    /// Add a time check to `dispatch_implicit_accept` and this test goes red.
    #[test]
    fn implicit_accept_dispatch_does_not_reverify_the_deadline() {
        let (mode, session) = offered_session();
        let timeout = bound_timeout_ms(&session);
        let deadline = OFFER_TIME_MS + timeout;
        let synthetic = mode
            .due_synthetic_envelope(&session, deadline)
            .expect("due at the deadline");
        assert_eq!(synthetic.timestamp_unix_ms, deadline);

        // The pause that lands after D, banked before the synthetic entry is
        // replayed.
        let mut replayed = session.clone();
        replayed.suspend(deadline + 50).unwrap();
        replayed.resume(deadline + 250).unwrap();
        assert_eq!(replayed.accumulated_suspended_ms, 200);

        // What a re-check would compute at the replay clock: less than nothing.
        let state: HandoffState = serde_json::from_slice(&replayed.mode_state).unwrap();
        assert!(
            HandoffMode::implicit_accept_elapsed_ms(&replayed, &state.offers["h1"], deadline)
                < timeout,
            "the scalar is below the timeout here — that is the whole hazard"
        );

        // Dispatch accepts anyway, because it validates identity and shape
        // only.
        let ctx = macp_core::mode::MessageContext::new(synthetic.timestamp_unix_ms);
        let resp = mode.on_message_at(&replayed, &synthetic, &ctx).unwrap();
        apply(&mut replayed, resp);
        let state: HandoffState = serde_json::from_slice(&replayed.mode_state).unwrap();
        assert_eq!(
            state.offers["h1"].disposition,
            HandoffDisposition::Accepted,
            "a correctly-emitted synthetic entry must replay through dispatch"
        );
    }
}
