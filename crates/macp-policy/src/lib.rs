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
pub use macp_core::policy::{
    CommitmentContext, CommitmentMode, PolicyDecision, PolicyDefinition, PolicyError,
    PolicyEvaluator,
};

/// The default [`PolicyEvaluator`], evaluating commitments against the RFC-MACP
/// rule schemas. Stateless — construct with `DefaultPolicyEvaluator` directly.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPolicyEvaluator;

impl PolicyEvaluator for DefaultPolicyEvaluator {
    fn evaluate_commitment(&self, ctx: &CommitmentContext<'_>) -> PolicyDecision {
        match ctx.mode {
            CommitmentMode::Decision { state } => evaluator::evaluate_decision_commitment_outcome(
                ctx.policy,
                state,
                ctx.participants,
                ctx.outcome_positive,
            ),
            CommitmentMode::Proposal {
                counter_proposal_count,
            } => evaluator::evaluate_proposal_commitment_outcome(
                ctx.policy,
                counter_proposal_count,
                ctx.outcome_positive,
            ),
            CommitmentMode::Task { has_output } => evaluator::evaluate_task_commitment_outcome(
                ctx.policy,
                has_output,
                ctx.outcome_positive,
            ),
            CommitmentMode::Handoff => {
                evaluator::evaluate_handoff_commitment_outcome(ctx.policy, ctx.outcome_positive)
            }
            CommitmentMode::Quorum {
                approve_count,
                reject_count,
                abstain_count,
            } => evaluator::evaluate_quorum_commitment_outcome(
                ctx.policy,
                approve_count,
                reject_count,
                abstain_count,
                ctx.participants.len(),
                ctx.outcome_positive,
            ),
            // CommitmentMode is #[non_exhaustive]: fail closed on modes this
            // evaluator does not know how to govern.
            _ => PolicyDecision::Deny {
                reasons: vec!["unrecognized commitment mode".into()],
            },
        }
    }
}
