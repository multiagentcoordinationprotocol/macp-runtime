//! Decision mode domain types.
//!
//! These plain data structs model the Decision mode's accumulated state. They
//! live in core (rather than in `macp-modes`) because the [`crate::policy`]
//! evaluation trait names them: `evaluate_decision_commitment` inspects a
//! [`DecisionState`]. Keeping them here lets `macp-policy` evaluate decisions
//! without depending on `macp-modes`, breaking the historical mode<->policy
//! cycle.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum DecisionPhase {
    #[default]
    Proposal,
    Evaluation,
    Voting,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionState {
    pub proposals: BTreeMap<String, Proposal>,
    pub evaluations: Vec<Evaluation>,
    pub objections: Vec<Objection>,
    pub votes: BTreeMap<String, BTreeMap<String, Vote>>,
    pub phase: DecisionPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub proposal_id: String,
    pub option: String,
    pub rationale: String,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluation {
    pub proposal_id: String,
    pub recommendation: String,
    pub confidence: f64,
    pub reason: String,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Objection {
    pub proposal_id: String,
    pub reason: String,
    pub severity: String,
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub proposal_id: String,
    pub vote: String,
    pub reason: String,
    pub sender: String,
}
