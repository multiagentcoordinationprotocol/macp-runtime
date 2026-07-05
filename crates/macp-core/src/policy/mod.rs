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

/// `#[non_exhaustive]`: consumers MUST treat any non-`Allow` decision as a
/// denial (fail closed). Never `if let Deny` — that fails open on new variants.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyDecision {
    Allow { reasons: Vec<String> },
    Deny { reasons: Vec<String> },
}

#[non_exhaustive]
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

/// Everything a policy evaluator may consult when gating a commitment.
///
/// Built by the mode at commitment time. `outcome_positive` comes from the
/// validated `CommitmentPayload` and makes every mode's evaluation
/// outcome-aware: a negative (decline) commitment is a legitimate terminal
/// outcome and must not be denied by checks that only make sense for positive
/// outcomes (RFC-MACP-0007 §6 and the schema_version 2 decline semantics).
pub struct CommitmentContext<'a> {
    pub policy: &'a PolicyDefinition,
    pub participants: &'a [String],
    pub outcome_positive: bool,
    pub mode: CommitmentMode<'a>,
}

/// Per-mode accumulated state relevant to commitment evaluation.
///
/// Carries exactly the data each mode already computes: `DecisionState` is a
/// core domain type (passed whole); the other modes summarize their internal
/// state into scalars. `#[non_exhaustive]`: evaluators must carry a wildcard
/// arm and treat unknown modes as a denial (fail closed).
#[non_exhaustive]
pub enum CommitmentMode<'a> {
    Decision {
        state: &'a DecisionState,
    },
    Proposal {
        counter_proposal_count: usize,
    },
    Task {
        has_output: bool,
    },
    Handoff,
    Quorum {
        approve_count: usize,
        reject_count: usize,
        abstain_count: usize,
    },
}

/// Governance policy evaluation at commitment time.
///
/// The runtime resolves a [`PolicyDefinition`] at `SessionStart` and stores it
/// on the session; at commitment time a mode builds a [`CommitmentContext`]
/// and calls [`PolicyEvaluator::evaluate_commitment`]. The default
/// implementation lives in `macp-policy` (`macp_policy::DefaultPolicyEvaluator`);
/// consumers may provide their own.
///
/// The per-mode methods are deprecated shims kept for one release; they build
/// a `CommitmentContext` and delegate to `evaluate_commitment`.
pub trait PolicyEvaluator: Send + Sync {
    /// Single evaluation entry point. Only an explicit
    /// [`PolicyDecision::Allow`] permits the commitment — callers must treat
    /// any other decision as a denial (fail closed).
    fn evaluate_commitment(&self, ctx: &CommitmentContext<'_>) -> PolicyDecision;

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_decision_commitment(
        &self,
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
    ) -> PolicyDecision {
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants,
            outcome_positive: true,
            mode: CommitmentMode::Decision { state },
        })
    }

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_decision_commitment_outcome(
        &self,
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
        outcome_positive: bool,
    ) -> PolicyDecision {
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants,
            outcome_positive,
            mode: CommitmentMode::Decision { state },
        })
    }

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_proposal_commitment(
        &self,
        policy: &PolicyDefinition,
        counter_proposal_count: usize,
    ) -> PolicyDecision {
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants: &[],
            outcome_positive: true,
            mode: CommitmentMode::Proposal {
                counter_proposal_count,
            },
        })
    }

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_task_commitment(
        &self,
        policy: &PolicyDefinition,
        has_output: bool,
    ) -> PolicyDecision {
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants: &[],
            outcome_positive: true,
            mode: CommitmentMode::Task { has_output },
        })
    }

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_handoff_commitment(&self, policy: &PolicyDefinition) -> PolicyDecision {
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants: &[],
            outcome_positive: true,
            mode: CommitmentMode::Handoff,
        })
    }

    #[deprecated(note = "build a CommitmentContext and call evaluate_commitment")]
    fn evaluate_quorum_commitment(
        &self,
        policy: &PolicyDefinition,
        approve_count: usize,
        reject_count: usize,
        abstain_count: usize,
        total_participants: usize,
    ) -> PolicyDecision {
        // The legacy signature carried an explicit participant total; the
        // context derives it from `participants`, which the shim cannot
        // reconstruct — evaluators needing the total should count ballots or
        // use `participants.len()`. Legacy callers are inside this workspace
        // only and have been migrated.
        let _ = total_participants;
        self.evaluate_commitment(&CommitmentContext {
            policy,
            participants: &[],
            outcome_positive: true,
            mode: CommitmentMode::Quorum {
                approve_count,
                reject_count,
                abstain_count,
            },
        })
    }
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
