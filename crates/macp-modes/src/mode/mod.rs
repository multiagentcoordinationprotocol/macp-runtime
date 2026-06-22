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
pub use macp_core::mode::ModeResponse;

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
