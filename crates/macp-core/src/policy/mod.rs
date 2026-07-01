//! Policy vocabulary and the pluggable evaluation trait.
//!
//! Core holds the types modes and the kernel must name: the policy
//! definition/decision/error, the per-mode [`rules`] schemas (read by modes to
//! drive policy-parameterized behavior and by evaluators to decide commitments),
//! and the [`PolicyEvaluator`] trait that modes call through. The concrete
//! default evaluator lives in the `macp-policy` crate; a third party can supply
//! its own `PolicyEvaluator` and inject it without forking the kernel.

pub mod rules;

use crate::decision::DecisionState;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyDefinition {
    pub policy_id: String,
    pub mode: String,
    pub description: String,
    pub rules: serde_json::Value,
    pub schema_version: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PolicyDecision {
    Allow { reasons: Vec<String> },
    Deny { reasons: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum PolicyError {
    UnknownPolicy(String),
    InvalidDefinition(String),
    PolicyDenied(String),
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::UnknownPolicy(id) => write!(f, "unknown policy: {}", id),
            PolicyError::InvalidDefinition(msg) => write!(f, "invalid policy definition: {}", msg),
            PolicyError::PolicyDenied(reason) => write!(f, "policy denied: {}", reason),
        }
    }
}

impl std::error::Error for PolicyError {}

/// Commitment rules shared across all mode policy schemas (RFC-MACP-0012).
///
/// This `commitment` sub-object appears in every mode's rule schema and is read
/// directly by the modes (to authorize who may emit a `Commitment`), so it
/// lives in core rather than in `macp-policy`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitmentRules {
    #[serde(default = "default_authority")]
    pub authority: String,
    #[serde(default)]
    pub designated_roles: Vec<String>,
    #[serde(default)]
    pub require_vote_quorum: bool,
    /// When `true`, an authorized initiator may finalize a *decline*
    /// (`outcome_positive = false`) even when the vote passed the approval
    /// threshold — the "executive veto" pattern. Defaults to `false`, which
    /// preserves the conservative behavior: a passing vote only authorizes a
    /// positive commitment. See RFC-MACP-0007 §6 (negative committed outcomes).
    #[serde(default)]
    pub allow_decline_over_approval: bool,
}

impl Default for CommitmentRules {
    fn default() -> Self {
        Self {
            authority: default_authority(),
            designated_roles: Vec::new(),
            require_vote_quorum: false,
            allow_decline_over_approval: false,
        }
    }
}

fn default_authority() -> String {
    "initiator_only".into()
}

/// Extract the `commitment` section from any mode's policy rules JSON.
/// All RFC mode schemas include a `commitment` sub-object with `authority` and
/// `designated_roles`.
pub fn extract_commitment_rules(rules: &serde_json::Value) -> CommitmentRules {
    rules
        .get("commitment")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default()
}

/// Governance policy evaluation at commitment time.
///
/// The runtime resolves a [`PolicyDefinition`] at `SessionStart` and stores it
/// on the session; at commitment time a mode calls the matching method here.
/// The default implementation lives in `macp-policy`
/// (`macp_policy::DefaultPolicyEvaluator`); consumers may provide their own.
pub trait PolicyEvaluator: Send + Sync {
    fn evaluate_decision_commitment(
        &self,
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
    ) -> PolicyDecision;

    /// Outcome-aware variant of [`Self::evaluate_decision_commitment`].
    ///
    /// Decision Mode permits both positive and negative committed outcomes
    /// (RFC-MACP-0007 §6). A positive commitment must clear the approval bar; a
    /// negative (decline) commitment must be backed by a conclusive
    /// non-approval. The `outcome_positive` flag is taken from the
    /// `CommitmentPayload`.
    ///
    /// This method is **defaulted** so existing external `PolicyEvaluator`
    /// implementations compile unchanged: the default body ignores
    /// `outcome_positive` and delegates to the outcome-unaware method, exactly
    /// reproducing their prior behavior. `macp-policy`'s
    /// `DefaultPolicyEvaluator` overrides it to apply outcome-aware gating.
    fn evaluate_decision_commitment_outcome(
        &self,
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
        outcome_positive: bool,
    ) -> PolicyDecision {
        let _ = outcome_positive;
        self.evaluate_decision_commitment(policy, state, participants)
    }

    fn evaluate_proposal_commitment(
        &self,
        policy: &PolicyDefinition,
        counter_proposal_count: usize,
    ) -> PolicyDecision;

    fn evaluate_task_commitment(
        &self,
        policy: &PolicyDefinition,
        has_output: bool,
    ) -> PolicyDecision;

    fn evaluate_handoff_commitment(&self, policy: &PolicyDefinition) -> PolicyDecision;

    fn evaluate_quorum_commitment(
        &self,
        policy: &PolicyDefinition,
        approve_count: usize,
        reject_count: usize,
        abstain_count: usize,
        total_participants: usize,
    ) -> PolicyDecision;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_error_display() {
        let e = PolicyError::UnknownPolicy("p1".into());
        assert_eq!(e.to_string(), "unknown policy: p1");

        let e = PolicyError::InvalidDefinition("bad".into());
        assert_eq!(e.to_string(), "invalid policy definition: bad");

        let e = PolicyError::PolicyDenied("nope".into());
        assert_eq!(e.to_string(), "policy denied: nope");
    }

    #[test]
    fn policy_definition_serialization_round_trip() {
        let def = PolicyDefinition {
            policy_id: "test".into(),
            mode: "*".into(),
            description: "test policy".into(),
            rules: serde_json::json!({"voting": {"algorithm": "none"}}),
            schema_version: 1,
        };
        let json = serde_json::to_string(&def).unwrap();
        let parsed: PolicyDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.policy_id, "test");
        assert_eq!(parsed.schema_version, 1);
    }

    #[test]
    fn commitment_rules_default_is_initiator_only() {
        let rules = CommitmentRules::default();
        assert_eq!(rules.authority, "initiator_only");
        assert!(rules.designated_roles.is_empty());
        assert!(!rules.require_vote_quorum);
    }

    #[test]
    fn extract_commitment_rules_reads_nested_object() {
        let rules = serde_json::json!({
            "commitment": { "authority": "designated_role", "designated_roles": ["agent://lead"] }
        });
        let parsed = extract_commitment_rules(&rules);
        assert_eq!(parsed.authority, "designated_role");
        assert_eq!(parsed.designated_roles, vec!["agent://lead".to_string()]);
    }
}
