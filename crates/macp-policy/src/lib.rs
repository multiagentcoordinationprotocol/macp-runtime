//! `macp-policy` — the default MACP governance policy engine.
//!
//! Holds the per-mode rule schemas ([`rules`]), the policy [`registry`], the
//! built-in [`defaults`], and the commitment [`evaluator`] functions. These are
//! exposed both as free functions (used internally) and through
//! [`DefaultPolicyEvaluator`], the default implementation of
//! [`macp_core::PolicyEvaluator`]. A consumer that wants different governance
//! can implement `macp_core::PolicyEvaluator` itself and inject it instead.

pub mod defaults;
pub mod evaluator;
pub mod registry;

// Rule schemas are shared vocabulary (modes read them too), so they live in
// macp-core. Re-exported so `macp_policy::rules` and downstream
// `crate::policy::rules` paths keep resolving.
pub use macp_core::policy::rules;
pub use macp_core::policy::{PolicyDecision, PolicyDefinition, PolicyError, PolicyEvaluator};

use macp_core::decision::DecisionState;

/// The default [`PolicyEvaluator`], evaluating commitments against the RFC-MACP
/// rule schemas. Stateless — construct with `DefaultPolicyEvaluator` directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicyEvaluator;

impl PolicyEvaluator for DefaultPolicyEvaluator {
    fn evaluate_decision_commitment(
        &self,
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
    ) -> PolicyDecision {
        evaluator::evaluate_decision_commitment(policy, state, participants)
    }

    fn evaluate_proposal_commitment(
        &self,
        policy: &PolicyDefinition,
        counter_proposal_count: usize,
    ) -> PolicyDecision {
        evaluator::evaluate_proposal_commitment(policy, counter_proposal_count)
    }

    fn evaluate_task_commitment(
        &self,
        policy: &PolicyDefinition,
        has_output: bool,
    ) -> PolicyDecision {
        evaluator::evaluate_task_commitment(policy, has_output)
    }

    fn evaluate_handoff_commitment(&self, policy: &PolicyDefinition) -> PolicyDecision {
        evaluator::evaluate_handoff_commitment(policy)
    }

    fn evaluate_quorum_commitment(
        &self,
        policy: &PolicyDefinition,
        approve_count: usize,
        reject_count: usize,
        abstain_count: usize,
        total_participants: usize,
    ) -> PolicyDecision {
        evaluator::evaluate_quorum_commitment(
            policy,
            approve_count,
            reject_count,
            abstain_count,
            total_participants,
        )
    }
}
