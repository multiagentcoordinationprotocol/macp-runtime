//! What-if replay: re-drive a recorded session under a **candidate** policy and
//! report how its outcome would differ from what was recorded.
//!
//! [`crate::replay`] rebuilds a session under the policy it was bound to, and
//! refuses a history the mode disagrees with — that is the replay-integrity
//! path, and nothing here changes it. [`replay_under_policy`] answers a
//! different question, for rolling out a policy change: had `candidate`
//! governed this session, would it still resolve, through which Commitment,
//! and which policy denials would fire?
//!
//! It runs two replays side by side over the same events: the recorded one,
//! which must replay cleanly (an error there is an integrity failure and is
//! returned as one), and the candidate one, which tolerates refusals. A
//! refused event is recorded and skipped, exactly as a live runtime would have
//! rejected it: it consumes no dedup slot and changes no state, and later
//! events are dispatched against the session as it then stands.
//!
//! Pure and deterministic: no storage, no clock, no registry. Every event is
//! dispatched at its recorded time, as in [`crate::replay`].
//!
//! # What is substituted
//!
//! Only the policy **definition** (`Session::policy_definition`). The session
//! keeps the `policy_version` it was bound to, so each recorded Commitment's
//! version echo still matches and the comparison isolates the rules. The
//! candidate must be valid for the session's mode; this crate does not
//! validate policy definitions (`macp_policy::PolicyRegistry` does).
//!
//! # Limits
//!
//! The history is fixed. Agents that would have behaved differently under the
//! candidate — retried, sent another Commitment — are not simulated, and a
//! runtime-originated event the candidate would have produced on its own (a
//! Handoff implicit accept under a shorter `implicit_accept_timeout_ms`) is not
//! synthesized: only recorded ones are re-applied, and the candidate may refuse
//! them.

use macp_core::error::MacpError;
use macp_core::policy::{DenyReason, PolicyDefinition};
use macp_core::session::{Session, SessionState};
use serde::Serialize;

use crate::mode::Mode;
use crate::replay::{apply_event, replay_start, RecordedStart, ReplayEvent, ReplayOptions};

/// Where in the history something happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "at", content = "index")]
#[non_exhaustive]
pub enum HistoryPoint {
    /// The `SessionStart`.
    SessionStart,
    /// The event at this index in the `events` passed in (0-based).
    Event(usize),
}

/// What one replay did with one history entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "outcome")]
#[non_exhaustive]
pub enum EntryOutcome {
    /// Applied. `state` is the session state after it.
    Applied { state: SessionState },
    /// A message that arrived once the session was no longer `Open`: replay
    /// only re-takes its dedup slot, as [`crate::replay::apply_event`] does.
    NotOpen,
    /// The mode refused it. `code` is the protocol error code
    /// ([`MacpError::error_code`]); `reasons` is non-empty for
    /// `POLICY_DENIED`.
    Refused {
        code: &'static str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        reasons: Vec<DenyReason>,
    },
    /// The candidate refused the `SessionStart`, so there is no session to
    /// apply this to.
    NoSession,
}

impl EntryOutcome {
    fn refused(err: &MacpError) -> Self {
        EntryOutcome::Refused {
            code: err.error_code(),
            reasons: err.deny_reasons().to_vec(),
        }
    }
}

/// One history entry, as each replay handled it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct EntryDiff {
    pub at: HistoryPoint,
    /// The entry's `message_id`, when it is an envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// The entry's `message_type`, when it is an envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    pub recorded: EntryOutcome,
    pub candidate: EntryOutcome,
}

impl EntryDiff {
    /// Whether the two replays handled this entry differently.
    pub fn diverges(&self) -> bool {
        self.recorded != self.candidate
    }
}

/// The envelope that moved a session to `Resolved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ResolvedBy {
    pub at: HistoryPoint,
    pub message_id: String,
    pub message_type: String,
}

/// How one replay ended.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Outcome {
    /// The final state. `None` when the candidate refused the `SessionStart`.
    pub state: Option<SessionState>,
    /// `Session::resolution`: the resolving Commitment's payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<ResolvedBy>,
}

impl Outcome {
    fn of(session: Option<&Session>, resolved_by: Option<ResolvedBy>) -> Self {
        Self {
            state: session.map(|s| s.state.clone()),
            resolution: session.and_then(|s| s.resolution.clone()),
            resolved_by,
        }
    }
}

/// A policy denial the candidate raised that the recorded replay did not.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Denial {
    pub at: HistoryPoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub reasons: Vec<DenyReason>,
}

/// The result of [`replay_under_policy`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct WhatIfReport {
    /// The candidate's `policy_id`.
    pub candidate_policy_id: String,
    /// The recorded policy's `policy_id`, if the session bound one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_policy_id: Option<String>,
    pub recorded: Outcome,
    pub candidate: Outcome,
    /// The first entry the two replays handled differently. `None`: the
    /// candidate changes nothing about this history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_divergence: Option<HistoryPoint>,
    /// Every `POLICY_DENIED` the candidate raised, in history order.
    pub denials: Vec<Denial>,
    /// Every entry, the `SessionStart` first.
    pub entries: Vec<EntryDiff>,
    /// The session the candidate replay ended with, for inspection beyond
    /// this report. `None` when it refused the `SessionStart`.
    #[serde(skip)]
    pub candidate_session: Option<Session>,
}

impl WhatIfReport {
    /// Whether the candidate changes the outcome: the final state, the
    /// resolution, or the Commitment that resolved it.
    pub fn outcome_changed(&self) -> bool {
        self.recorded != self.candidate
    }
}

/// Replay `start` and `events` under the recorded policy and under
/// `candidate`, and report the difference.
///
/// Errors only when the **recorded** replay fails — the same integrity
/// failure [`crate::replay::replay`] returns. Anything the candidate refuses is
/// reported, not returned.
pub fn replay_under_policy(
    start: &RecordedStart,
    events: impl IntoIterator<Item = ReplayEvent>,
    mode: &dyn Mode,
    opts: &ReplayOptions,
    candidate: &PolicyDefinition,
) -> Result<WhatIfReport, MacpError> {
    let mut recorded = replay_start(start, mode)?;
    let mut entries = Vec::new();
    let mut denials = Vec::new();

    let start_env = &start.envelope;
    let candidate_start = start.clone().with_policy(Some(candidate.clone()));
    let mut cand = match replay_start(&candidate_start, mode) {
        Ok(mut session) => {
            // Substitute the rules, not the binding: keep the recorded
            // policy_version so recorded Commitments still echo it.
            session.policy_version = recorded.policy_version.clone();
            entries.push(EntryDiff {
                at: HistoryPoint::SessionStart,
                message_id: Some(start_env.message_id.clone()),
                message_type: Some(start_env.message_type.clone()),
                recorded: EntryOutcome::Applied {
                    state: recorded.state.clone(),
                },
                candidate: EntryOutcome::Applied {
                    state: session.state.clone(),
                },
            });
            Some(session)
        }
        Err(err) => {
            push_denial(&mut denials, HistoryPoint::SessionStart, start_env, &err);
            entries.push(EntryDiff {
                at: HistoryPoint::SessionStart,
                message_id: Some(start_env.message_id.clone()),
                message_type: Some(start_env.message_type.clone()),
                recorded: EntryOutcome::Applied {
                    state: recorded.state.clone(),
                },
                candidate: EntryOutcome::refused(&err),
            });
            None
        }
    };

    let mut recorded_by = None;
    let mut cand_by = None;
    for (index, event) in events.into_iter().enumerate() {
        let at = HistoryPoint::Event(index);
        let envelope = match &event {
            ReplayEvent::Message { envelope, .. } | ReplayEvent::Synthetic { envelope } => {
                Some(envelope.clone())
            }
            _ => None,
        };

        let rec_outcome = step(&mut recorded, event.clone(), mode, opts)?;
        note_resolution(
            &recorded,
            &rec_outcome,
            at,
            envelope.as_ref(),
            &mut recorded_by,
        );

        let cand_outcome = match cand.as_mut() {
            None => EntryOutcome::NoSession,
            Some(session) => match step(session, event, mode, opts) {
                Ok(outcome) => {
                    note_resolution(session, &outcome, at, envelope.as_ref(), &mut cand_by);
                    outcome
                }
                Err(err) => {
                    if let Some(env) = &envelope {
                        push_denial(&mut denials, at, env, &err);
                    }
                    EntryOutcome::refused(&err)
                }
            },
        };

        entries.push(EntryDiff {
            at,
            message_id: envelope.as_ref().map(|e| e.message_id.clone()),
            message_type: envelope.as_ref().map(|e| e.message_type.clone()),
            recorded: rec_outcome,
            candidate: cand_outcome,
        });
    }

    let first_divergence = entries.iter().find(|e| e.diverges()).map(|e| e.at);
    Ok(WhatIfReport {
        candidate_policy_id: candidate.policy_id.clone(),
        recorded_policy_id: start.policy.as_ref().map(|p| p.policy_id.clone()),
        recorded: Outcome::of(Some(&recorded), recorded_by),
        candidate: Outcome::of(cand.as_ref(), cand_by),
        first_divergence,
        denials,
        entries,
        candidate_session: cand,
    })
}

/// Apply one event, classifying what happened. A refusal is the `Err`.
fn step(
    session: &mut Session,
    event: ReplayEvent,
    mode: &dyn Mode,
    opts: &ReplayOptions,
) -> Result<EntryOutcome, MacpError> {
    let is_envelope = matches!(
        event,
        ReplayEvent::Message { .. } | ReplayEvent::Synthetic { .. }
    );
    let not_open = is_envelope && session.state != SessionState::Open;
    apply_event(session, event, mode, opts)?;
    Ok(if not_open {
        EntryOutcome::NotOpen
    } else {
        EntryOutcome::Applied {
            state: session.state.clone(),
        }
    })
}

fn note_resolution(
    session: &Session,
    outcome: &EntryOutcome,
    at: HistoryPoint,
    envelope: Option<&macp_pb::pb::Envelope>,
    resolved_by: &mut Option<ResolvedBy>,
) {
    if resolved_by.is_some() || session.state != SessionState::Resolved {
        return;
    }
    if let (EntryOutcome::Applied { .. }, Some(env)) = (outcome, envelope) {
        *resolved_by = Some(ResolvedBy {
            at,
            message_id: env.message_id.clone(),
            message_type: env.message_type.clone(),
        });
    }
}

fn push_denial(
    denials: &mut Vec<Denial>,
    at: HistoryPoint,
    envelope: &macp_pb::pb::Envelope,
    err: &MacpError,
) {
    if let MacpError::PolicyDenied { reasons } = err {
        denials.push(Denial {
            at,
            message_id: Some(envelope.message_id.clone()),
            reasons: reasons.clone(),
        });
    }
}
