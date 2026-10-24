//! The in-process target: the Embedded profile, over `macp_modes::step` and
//! `macp_modes::replay` with no transport and no storage.
//!
//! It runs the acceptance sequence `macp-runtime`'s kernel runs (the same
//! `step` calls in the same order, with the same registry lookups between
//! them), keeping history as a [`ReplayEvent`] list so it can replay any
//! session and compare. Its clock is injectable: a clocked fixture runs on a
//! [`ManualClock`] the runner sets, an unclocked one on the system clock.

use crate::target::{
    Capabilities, ConformanceTarget, HistoryEntry, Observation, Profile, Submission, TargetError,
};
use async_trait::async_trait;
use macp_core::commitment_hash::{commitment_hash, verify_commitment_hash, VerifyOutcome};
use macp_core::session::{validate_session_id_for_acceptance, CURRENT_SEMANTICS_REV};
use macp_core::{
    Clock, MacpError, ManualClock, PolicyDefinition, Session, SessionState, SystemClock,
};
use macp_modes::mode_registry::ModeRegistry;
use macp_modes::replay::{replay, RecordedStart, ReplayEvent, ReplayOptions};
use macp_modes::step::{self, Precheck, StartOptions};
use macp_pb::pb::{CommitmentPayload, Envelope};
use macp_policy::defaults::DEFAULT_POLICY_ID;
use macp_policy::registry::PolicyRegistry;
use std::collections::HashMap;
use std::sync::Arc;

struct Recorded {
    session: Session,
    start: Envelope,
    bound_mode_version: Option<String>,
    events: Vec<ReplayEvent>,
}

/// See the module docs. Every [`ConformanceTarget::begin_fixture`] starts
/// from fresh registries, so fixtures cannot see each other's sessions or
/// policies.
pub struct InProcessTarget {
    modes: ModeRegistry,
    policies: PolicyRegistry,
    clock: Arc<dyn Clock>,
    manual: Option<Arc<ManualClock>>,
    sessions: HashMap<String, Recorded>,
}

impl Default for InProcessTarget {
    fn default() -> Self {
        Self::new()
    }
}

fn registries() -> (ModeRegistry, PolicyRegistry) {
    (
        ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator)),
        PolicyRegistry::new(),
    )
}

/// The fixture vocabulary for a state.
pub fn state_name(state: &SessionState) -> &'static str {
    match state {
        SessionState::Open => "Open",
        SessionState::Suspended => "Suspended",
        SessionState::Resolved => "Resolved",
        SessionState::Expired => "Expired",
        SessionState::Cancelled => "Cancelled",
    }
}

impl InProcessTarget {
    pub fn new() -> Self {
        let (modes, policies) = registries();
        Self {
            modes,
            policies,
            clock: Arc::new(SystemClock),
            manual: None,
            sessions: HashMap::new(),
        }
    }

    fn start(&mut self, env: &Envelope) -> Result<Submission, MacpError> {
        if env.mode.trim().is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        validate_session_id_for_acceptance(&env.session_id)?;
        let name = env.mode.as_str();
        let mode = self
            .modes
            .get_mode(name)
            .ok_or(MacpError::UnknownMode)?
            .instantiate()?;
        let mut opts =
            StartOptions::default().with_strict(self.modes.requires_strict_session_start(name));
        if let Some(v) = self.modes.get_mode_version(name) {
            opts = opts.with_descriptor_mode_version(v);
        }
        let validated = step::validate_session_start(env, self.clock.now_ms(), &opts)?;
        if let Some(existing) = self.sessions.get(&env.session_id) {
            if existing.session.seen_message_ids.contains(&env.message_id) {
                return Ok(Submission::Accepted { duplicate: true });
            }
            return Err(MacpError::SessionAlreadyExists);
        }
        let requested = validated.requested_policy_version();
        let version = if requested.is_empty() {
            DEFAULT_POLICY_ID
        } else {
            requested
        };
        let policy = self
            .policies
            .resolve(version)
            .map_err(|_| MacpError::UnknownPolicyVersion)?;
        let bound_mode_version = validated.bound_mode_version.clone();
        let session = validated.accept(env, mode.as_ref(), Some(policy))?;
        self.sessions.insert(
            env.session_id.clone(),
            Recorded {
                session,
                start: env.clone(),
                bound_mode_version,
                events: Vec::new(),
            },
        );
        Ok(Submission::Accepted { duplicate: false })
    }

    fn message(&mut self, env: &Envelope) -> Result<Submission, MacpError> {
        let now = self.clock.now_ms();
        let rec = self
            .sessions
            .get_mut(&env.session_id)
            .ok_or(MacpError::UnknownSession)?;
        match step::check_preconditions(&rec.session, env, now)? {
            Precheck::Duplicate => return Ok(Submission::Accepted { duplicate: true }),
            Precheck::Expired => {
                rec.session.expire()?;
                rec.events.push(ReplayEvent::Expired);
                return Err(MacpError::TtlExpired);
            }
            Precheck::Proceed => {}
        }
        let mode = self
            .modes
            .get_mode(&rec.session.mode)
            .ok_or(MacpError::UnknownMode)?
            .instantiate()?;
        step::admit(&rec.session, env, mode.as_ref())?;
        if let Some(syn) = step::next_synthetic(&rec.session, mode.as_ref(), now)? {
            let envelope = step::commit_synthetic(&mut rec.session, syn);
            rec.events.push(ReplayEvent::Synthetic { envelope });
        }
        let response = step::dispatch(&rec.session, env, mode.as_ref(), now)?;
        rec.events.push(ReplayEvent::Message {
            envelope: env.clone(),
            accepted_at_ms: now,
        });
        step::commit(&mut rec.session, env, response, now);
        Ok(Submission::Accepted { duplicate: false })
    }

    /// Signals and Progress are ambient: validated, never recorded.
    fn ambient(env: &Envelope) -> Result<Submission, MacpError> {
        use prost::Message;
        if env.message_type == "Signal" && !env.payload.is_empty() {
            let s = macp_pb::pb::SignalPayload::decode(&*env.payload)
                .map_err(|_| MacpError::InvalidPayload)?;
            if s.signal_type.trim().is_empty() {
                return Err(MacpError::InvalidPayload);
            }
        }
        if env.message_type == "Progress" && !env.payload.is_empty() {
            macp_pb::pb::ProgressPayload::decode(&*env.payload)
                .map_err(|_| MacpError::InvalidPayload)?;
        }
        Ok(Submission::Accepted { duplicate: false })
    }

    /// Expire at read time when the deadline passed, as the runtime's
    /// `get_session_checked` does.
    fn expire_if_due(&mut self, session_id: &str) -> Option<&Recorded> {
        let now = self.clock.now_ms();
        let rec = self.sessions.get_mut(session_id)?;
        if rec.session.deadline_passed(now) && rec.session.expire().is_ok() {
            rec.events.push(ReplayEvent::Expired);
        }
        Some(rec)
    }
}

#[async_trait]
impl ConformanceTarget for InProcessTarget {
    fn profile(&self) -> Profile {
        Profile::Embedded
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
            .with_clock_control(true)
            .with_replay(true)
            .with_commitment_hash(true)
    }

    fn describe(&self) -> String {
        format!("in-process macp-modes {}", env!("CARGO_PKG_VERSION"))
    }

    fn semantics_rev(&self) -> Option<u32> {
        Some(CURRENT_SEMANTICS_REV)
    }

    async fn begin_fixture(&mut self, clock_start: Option<i64>) -> Result<(), TargetError> {
        let (modes, policies) = registries();
        self.modes = modes;
        self.policies = policies;
        self.sessions.clear();
        match clock_start {
            Some(at) => {
                let manual = Arc::new(ManualClock::new(at));
                self.clock = manual.clone();
                self.manual = Some(manual);
            }
            None => {
                self.clock = Arc::new(SystemClock);
                self.manual = None;
            }
        }
        Ok(())
    }

    async fn register_policy(&mut self, definition: PolicyDefinition) -> Result<(), TargetError> {
        let _ = self.policies.unregister(&definition.policy_id);
        self.policies.register(definition).map_err(TargetError)
    }

    async fn set_clock(&mut self, now_ms: i64) -> Result<(), TargetError> {
        match &self.manual {
            Some(c) => {
                c.set(now_ms);
                Ok(())
            }
            None => Err(TargetError("set_clock on an unclocked fixture".into())),
        }
    }

    async fn submit(&mut self, envelope: Envelope) -> Result<Submission, TargetError> {
        let result = match envelope.message_type.as_str() {
            "SessionStart" => self.start(&envelope),
            "Signal" | "Progress" => Self::ambient(&envelope),
            _ => self.message(&envelope),
        };
        Ok(result.unwrap_or_else(|e| Submission::Rejected {
            code: e.error_code().to_string(),
        }))
    }

    async fn observe(
        &mut self,
        session_id: &str,
        _initiator: &str,
    ) -> Result<Observation, TargetError> {
        let rec = self
            .expire_if_due(session_id)
            .ok_or_else(|| TargetError(format!("no session {session_id}")))?;
        let s = &rec.session;
        let history = rec
            .events
            .iter()
            .filter_map(|e| match e {
                ReplayEvent::Message { envelope, .. } | ReplayEvent::Synthetic { envelope } => {
                    Some(HistoryEntry::from_envelope(envelope))
                }
                _ => None,
            })
            .collect();
        let mode_state = if s.mode_state.is_empty() {
            None
        } else {
            serde_json::from_slice(&s.mode_state).ok()
        };
        Ok(Observation::new(state_name(&s.state))
            .with_resolution(s.resolution.clone())
            .with_mode_state(mode_state)
            .with_history(history))
    }

    async fn check_replay(&mut self, session_id: &str) -> Result<Result<(), String>, TargetError> {
        let rec = self
            .sessions
            .get(session_id)
            .ok_or_else(|| TargetError(format!("no session {session_id}")))?;
        let live = &rec.session;
        let mode = self
            .modes
            .get_mode(&live.mode)
            .ok_or_else(|| TargetError(format!("mode {} vanished", live.mode)))?
            .instantiate()
            .map_err(|e| TargetError(e.to_string()))?;
        let start =
            RecordedStart::from_session(rec.start.clone(), live, rec.bound_mode_version.clone());
        let replayed = match replay(
            &start,
            rec.events.iter().cloned(),
            mode.as_ref(),
            &ReplayOptions::default(),
        ) {
            Ok(s) => s,
            Err(e) => return Ok(Err(format!("replay failed: {e}"))),
        };
        if replayed.state != live.state {
            return Ok(Err(format!(
                "state: replayed {:?}, live {:?}",
                replayed.state, live.state
            )));
        }
        if replayed.resolution != live.resolution {
            return Ok(Err("resolution differs".into()));
        }
        if replayed.mode_state != live.mode_state {
            return Ok(Err("mode_state differs".into()));
        }
        if replayed.seen_message_ids != live.seen_message_ids {
            return Ok(Err("dedup state (seen_message_ids) differs".into()));
        }
        Ok(Ok(()))
    }

    fn commitment_hash(&self, payload: &CommitmentPayload) -> Option<String> {
        Some(commitment_hash(payload))
    }

    fn verify_commitment_hash(&self, wire: &[u8], claimed: &str) -> Option<String> {
        Some(
            match verify_commitment_hash(wire, claimed) {
                VerifyOutcome::Match => "match",
                VerifyOutcome::Mismatch { .. } => "mismatch",
                VerifyOutcome::CannotVerify(_) => "cannot_verify",
                // A future outcome matches no `expected_outcome`, so it fails.
                _ => "unknown",
            }
            .to_string(),
        )
    }
}
