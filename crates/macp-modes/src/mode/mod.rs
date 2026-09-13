pub mod decision;
pub mod handoff;
pub mod multi_round;
pub mod passthrough;
pub mod proposal;
pub mod quorum;
pub mod task;
pub mod util;

use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::{Envelope, ModeDescriptor};
use std::collections::HashMap;

/// The canonical standards-track modes implemented by this runtime.
pub const STANDARD_MODE_NAMES: &[&str] = &[
    "macp.mode.decision.v1",
    "macp.mode.proposal.v1",
    "macp.mode.task.v1",
    "macp.mode.handoff.v1",
    "macp.mode.quorum.v1",
];

/// Built-in extension modes shipped with this runtime but not yet standards-track.
pub const EXTENSION_MODE_NAMES: &[&str] = &["ext.multi_round.v1"];

// `ModeResponse` (data) lives in `macp-core` so `Session::apply_mode_response`
// can consume it without a core->modes cycle. The `Mode` trait (behavior) stays
// here. Re-exported so `crate::mode::ModeResponse` keeps resolving.
pub use macp_core::mode::{MessageContext, ModeResponse};

/// Trait that coordination modes implement.
/// Modes receive immutable session references and return a ModeResponse.
/// The runtime kernel is responsible for applying the response.
pub trait Mode: Send + Sync {
    fn on_session_start(
        &self,
        session: &Session,
        env: &Envelope,
    ) -> Result<ModeResponse, MacpError>;

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError>;

    /// Kernel entry point: `on_message` plus the runtime's
    /// [`macp_core::mode::MessageContext`] (acceptance clock). Defaulted to plain
    /// `on_message` so most modes ignore it; modes that need a trustworthy
    /// time source (Handoff) override this instead of reading the forgeable
    /// `Envelope.timestamp_unix_ms`. The runtime and replay always call this,
    /// with the same clock value that the log entry records.
    fn on_message_at(
        &self,
        session: &Session,
        env: &Envelope,
        ctx: &macp_core::mode::MessageContext,
    ) -> Result<ModeResponse, MacpError> {
        let _ = ctx;
        self.on_message(session, env)
    }

    /// The client boundary: validate an envelope that a *client* submitted on
    /// the live path, before it is dispatched.
    ///
    /// Called on live client-submitted envelopes **only** — never on replay,
    /// and never on a runtime-synthesized envelope. Library kernels MUST call
    /// it on inbound traffic. Default is `Ok(())`, so a mode with no
    /// client-boundary rules needs no override.
    ///
    /// # Why this is a hook and not a check inside `on_message`
    ///
    /// Some envelope shapes are legitimate *as recorded history* but illegal
    /// *as client submissions*. The motivating case is the handoff mode's
    /// runtime-synthesized implicit `HandoffAccept` (RFC-MACP-0010 §5.1(3),
    /// whose prohibition is scoped to submission "via `Send`"): once such an
    /// entry is in the append-only log, replay must dispatch it through the
    /// ordinary mode path, so the mode cannot refuse the shape outright. Only
    /// the live boundary can tell the two apart, because only the live
    /// boundary knows the envelope came from a client. Keeping the rejection
    /// here — rather than marking the log entry with a discriminator — is what
    /// makes the recorded payload itself a trustworthy provenance signal, and
    /// it keeps a *stale reader* loud: an old binary replaying a newer log
    /// rejects the entry in its own mode and fails replay visibly, instead of
    /// silently skipping an entry kind it does not recognise.
    ///
    /// # Hazard: this hook is fail-open by construction
    ///
    /// Nothing forces a caller to invoke it — a kernel that drives the phases
    /// by hand and never calls it simply has no client boundary, and still
    /// compiles. This runtime is the worked example of why that matters:
    /// `crate::step::validate_message` does call the hook, but the runtime
    /// does **not** go through `step::validate_message` — `process_message`
    /// calls [`Mode::authorize_sender`] and [`Mode::on_message_at`] directly so
    /// it can interpose its durable append between validation and commit. So
    /// wiring the hook into `step` alone would have left the runtime
    /// unprotected; the runtime calls it explicitly at its own two live entry
    /// points (`process_message` and `process_session_start`), which together
    /// cover every client envelope (`Send` and `StreamSession` both funnel
    /// into `Runtime::process`), while replay and crash recovery only re-read
    /// entries that already passed the hook when they were first accepted.
    /// There is no compile-time forcing function here — only this note.
    fn validate_client_envelope(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        let _ = (session, env);
        Ok(())
    }

    /// The synthesis seam: given this session and this clock reading, the
    /// envelope (if any) that MUST enter accepted history *before* the message
    /// currently being processed.
    ///
    /// Default `None` — nothing is ever due, which is the answer for every
    /// mode but Handoff. The motivating case is RFC-MACP-0010 §5.1(2): once an
    /// outstanding handoff offer's `implicit_accept_timeout_ms` has elapsed,
    /// the runtime "MUST append a synthetic `HandoffAccept` envelope to the
    /// session's accepted history — before evaluating any subsequent message
    /// against the offer's acceptance state, and in particular before any
    /// `Commitment` evaluation".
    ///
    /// # The kernel contract
    ///
    /// A kernel that calls this MUST, holding the session's lock and *before*
    /// it validates or dispatches the triggering message:
    ///
    /// 1. dispatch the returned envelope through [`Mode::on_message_at`] with
    ///    `accepted_at_ms` equal to the envelope's own `timestamp_unix_ms`,
    /// 2. append it durably as an ordinary accepted (`Incoming`) history entry,
    ///    stamping `received_at_ms` with the envelope's own
    ///    `timestamp_unix_ms` — **not** wall-clock. Replay derives its dispatch
    ///    clock from `received_at_ms` (`src/replay.rs`), so stamping anything
    ///    else desynchronizes the live and replayed clocks for this entry and
    ///    breaks byte-identical rebuild of `mode_state`. Harmless for handoff
    ///    specifically, whose accept arm is time-blind, but the contract is
    ///    general and the next mode to use this hook may not be.
    /// 3. commit the resulting session state and insert the envelope's
    ///    `message_id` into the dedup set,
    /// 4. publish it to the session's subscribers,
    ///
    /// and only then process the triggering message. Appending without
    /// dispatching, or dispatching without appending, forks live state from
    /// what replay will rebuild from the log.
    ///
    /// # Never called on replay
    ///
    /// The recorded entry *is* the product: replay dispatches it through the
    /// ordinary message path like any other accepted entry, so the timer stays
    /// outside the replay boundary while its recorded product is inside — the
    /// same construction as the runtime-emitted lifecycle envelopes of
    /// RFC-MACP-0001 §7.5. An implementation must therefore never read a clock
    /// of its own: every field of the returned envelope, `timestamp_unix_ms`
    /// included, has to be a pure function of the session state and `now_ms`,
    /// because it is baked into permanent history and an observation-dependent
    /// value could never be reproduced.
    ///
    /// # Idempotence
    ///
    /// Implementations MUST return `None` once the returned envelope has been
    /// applied to the session: a second emission would append a duplicate
    /// entry whose deterministic `message_id` already holds a dedup slot.
    ///
    /// Like [`Mode::validate_client_envelope`], this hook is fail-open by
    /// construction — a kernel that never calls it simply never synthesizes,
    /// and still compiles. There is no compile-time forcing function, only
    /// this note.
    fn due_synthetic_envelope(&self, session: &Session, now_ms: i64) -> Option<Envelope> {
        let _ = (session, now_ms);
        None
    }

    /// Authorize the sender for this message. Modes can override to customize
    /// authorization (e.g., allowing orchestrator bypass for Commitment messages).
    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        if !session.participants.is_empty() && !session.participants.contains(&env.sender) {
            return Err(MacpError::Forbidden);
        }
        Ok(())
    }
}

pub fn standard_mode_names() -> &'static [&'static str] {
    STANDARD_MODE_NAMES
}

pub fn extension_mode_names() -> &'static [&'static str] {
    EXTENSION_MODE_NAMES
}

fn schema_map(path: &str) -> HashMap<String, String> {
    HashMap::from([("protobuf".to_string(), path.to_string())])
}

pub fn standard_mode_descriptors() -> Vec<ModeDescriptor> {
    vec![
        ModeDescriptor {
            mode: "macp.mode.decision.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Decision Mode".into(),
            description: "Structured decision making with proposals, evaluations, objections, votes, and a terminal Commitment.".into(),
            determinism_class: "semantic-deterministic".into(),
            participant_model: "declared".into(),
            message_types: vec![
                "SessionStart".into(),
                "Proposal".into(),
                "Evaluation".into(),
                "Objection".into(),
                "Vote".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
        },
        ModeDescriptor {
            mode: "macp.mode.proposal.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Proposal Mode".into(),
            description: "Negotiation with proposals, counterproposals, accepts, rejects, withdrawals, and a terminal Commitment.".into(),
            determinism_class: "semantic-deterministic".into(),
            participant_model: "peer".into(),
            message_types: vec![
                "SessionStart".into(),
                "Proposal".into(),
                "CounterProposal".into(),
                "Accept".into(),
                "Reject".into(),
                "Withdraw".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
        },
        ModeDescriptor {
            mode: "macp.mode.task.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Task Mode".into(),
            description: "One bounded delegated task with assignee responses, progress, completion/failure reports, and a terminal Commitment.".into(),
            determinism_class: "structural-only".into(),
            participant_model: "orchestrated".into(),
            message_types: vec![
                "SessionStart".into(),
                "TaskRequest".into(),
                "TaskAccept".into(),
                "TaskReject".into(),
                "TaskUpdate".into(),
                "TaskComplete".into(),
                "TaskFail".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
        },
        ModeDescriptor {
            mode: "macp.mode.handoff.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Handoff Mode".into(),
            description: "Scoped responsibility transfer with handoff offers, context, target responses, and a terminal Commitment.".into(),
            determinism_class: "context-frozen".into(),
            participant_model: "delegated".into(),
            message_types: vec![
                "SessionStart".into(),
                "HandoffOffer".into(),
                "HandoffContext".into(),
                "HandoffAccept".into(),
                "HandoffDecline".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
        },
        ModeDescriptor {
            mode: "macp.mode.quorum.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Quorum Mode".into(),
            description: "Threshold approval with one approval request, participant ballots, and a terminal Commitment.".into(),
            determinism_class: "semantic-deterministic".into(),
            participant_model: "quorum".into(),
            message_types: vec![
                "SessionStart".into(),
                "ApprovalRequest".into(),
                "Approve".into(),
                "Reject".into(),
                "Abstain".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
        },
    ]
}

pub fn extension_mode_descriptors() -> Vec<ModeDescriptor> {
    vec![ModeDescriptor {
        mode: "ext.multi_round.v1".into(),
        mode_version: "1.0.0".into(),
        title: "Multi-Round Mode".into(),
        description: "Iterative convergence through multiple contribution rounds until all participants agree, with a terminal Commitment.".into(),
        determinism_class: "semantic-deterministic".into(),
        participant_model: "peer".into(),
        message_types: vec![
            "SessionStart".into(),
            "Contribute".into(),
            "Commitment".into(),
        ],
        terminal_message_types: vec!["Commitment".into()],
        schema_uris: schema_map("buf.build/multiagentcoordinationprotocol/macp"),
    }]
}

pub fn all_mode_descriptors() -> Vec<ModeDescriptor> {
    let mut all = standard_mode_descriptors();
    all.extend(extension_mode_descriptors());
    all
}
