use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Decision Policy Rules ───────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DecisionPolicyRules {
    #[serde(default)]
    pub voting: VotingRules,
    #[serde(default)]
    pub objection_handling: ObjectionHandlingRules,
    #[serde(default)]
    pub evaluation: EvaluationRules,
    #[serde(default)]
    pub commitment: CommitmentRules,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VotingRules {
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    #[serde(default = "default_threshold")]
    pub threshold: f64,
    #[serde(default)]
    pub quorum: QuorumRules,
    #[serde(default)]
    pub weights: HashMap<String, f64>,
}

impl Default for VotingRules {
    fn default() -> Self {
        Self {
            algorithm: default_algorithm(),
            threshold: default_threshold(),
            quorum: QuorumRules::default(),
            weights: HashMap::new(),
        }
    }
}

fn default_algorithm() -> String {
    "none".into()
}

fn default_threshold() -> f64 {
    0.5
}

/// Quorum rules used inside Decision mode's `voting.quorum`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuorumRules {
    #[serde(default = "default_quorum_type", rename = "type")]
    pub quorum_type: String,
    #[serde(default)]
    pub value: f64,
}

impl Default for QuorumRules {
    fn default() -> Self {
        Self {
            quorum_type: default_quorum_type(),
            value: 0.0,
        }
    }
}

fn default_quorum_type() -> String {
    "count".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectionHandlingRules {
    /// RFC-MACP-0012: objections with severity "critical" trigger veto logic.
    #[serde(default, alias = "critical_severity_vetoes")]
    pub critical_severity_vetoes: bool,
    #[serde(default = "default_veto_threshold")]
    pub veto_threshold: u32,
    /// What a triggered critical-objection veto does to a commitment.
    /// Defaults to [`CriticalObjectionAction::Deny`] — the historical hard-stop
    /// that blocks every commitment. Operators in adverse-action domains
    /// (claims/lending) generally want `deny` or `hold`; `finalize_decline` is
    /// opt-in because auto-finalizing a denial off a single critical objection
    /// is itself a regulated adverse action.
    #[serde(default)]
    pub critical_objection_action: CriticalObjectionAction,
}

impl Default for ObjectionHandlingRules {
    fn default() -> Self {
        Self {
            critical_severity_vetoes: false,
            veto_threshold: default_veto_threshold(),
            critical_objection_action: CriticalObjectionAction::default(),
        }
    }
}

fn default_veto_threshold() -> u32 {
    1
}

/// How a triggered critical-objection veto resolves a commitment attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CriticalObjectionAction {
    /// Hard-stop: the veto blocks every commitment, positive or negative
    /// (historical behavior, conservative default).
    #[default]
    Deny,
    /// The veto permits a *negative* commitment (`outcome_positive = false`) to
    /// finalize, while still blocking a positive one.
    FinalizeDecline,
    /// The veto blocks the commitment but signals the session should be held
    /// open for human escalation rather than treated as a permanent denial.
    /// At the evaluator layer this denies the commitment (leaving the session
    /// open); the distinct reason string marks it as an escalation hold.
    Hold,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationRules {
    #[serde(default)]
    pub required_before_voting: bool,
    #[serde(default)]
    pub minimum_confidence: f64,
}

impl Default for EvaluationRules {
    fn default() -> Self {
        Self {
            required_before_voting: false,
            minimum_confidence: 0.0,
        }
    }
}

// `CommitmentRules` is shared with the modes (which read it directly to
// authorize commitments), so it lives in `macp-core`. Re-exported here so the
// per-mode rule structs below and `crate::rules::CommitmentRules` keep
// resolving.
pub use super::CommitmentRules;

// ── Proposal Policy Rules (RFC-MACP-0012 Section 4.3) ──────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ProposalPolicyRules {
    #[serde(default)]
    pub acceptance: ProposalAcceptanceRules,
    #[serde(default)]
    pub counter_proposal: CounterProposalRules,
    #[serde(default)]
    pub rejection: RejectionRules,
    #[serde(default)]
    pub commitment: CommitmentRules,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalAcceptanceRules {
    #[serde(default = "default_acceptance_criterion")]
    pub criterion: String,
}

impl Default for ProposalAcceptanceRules {
    fn default() -> Self {
        Self {
            criterion: default_acceptance_criterion(),
        }
    }
}

fn default_acceptance_criterion() -> String {
    "all_parties".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CounterProposalRules {
    #[serde(default)]
    pub max_rounds: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RejectionRules {
    #[serde(default)]
    pub terminal_on_any_reject: bool,
}

// ── Task Policy Rules (RFC-MACP-0012 Section 4.4) ──────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct TaskPolicyRules {
    #[serde(default)]
    pub assignment: TaskAssignmentRules,
    #[serde(default)]
    pub completion: TaskCompletionRules,
    #[serde(default)]
    pub commitment: CommitmentRules,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskAssignmentRules {
    #[serde(default)]
    pub allow_reassignment_on_reject: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskCompletionRules {
    #[serde(default)]
    pub require_output: bool,
}

// ── Handoff Policy Rules (RFC-MACP-0012 Section 4.5) ───────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct HandoffPolicyRules {
    #[serde(default)]
    pub acceptance: HandoffAcceptanceRules,
    #[serde(default)]
    pub commitment: CommitmentRules,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HandoffAcceptanceRules {
    #[serde(default)]
    pub implicit_accept_timeout_ms: u64,
}

// ── Quorum Policy Rules (RFC-MACP-0012 Section 4.2) ────────────────

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct QuorumPolicyRules {
    #[serde(default)]
    pub threshold: QuorumThreshold,
    #[serde(default)]
    pub abstention: AbstentionRules,
    #[serde(default)]
    pub commitment: CommitmentRules,
}

/// Threshold rules for Quorum mode (distinct from `QuorumRules` used in Decision mode's `voting.quorum`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuorumThreshold {
    #[serde(default = "default_threshold_type", rename = "type")]
    pub threshold_type: String,
    #[serde(default)]
    pub value: f64,
}

impl Default for QuorumThreshold {
    fn default() -> Self {
        Self {
            threshold_type: default_threshold_type(),
            value: 0.0,
        }
    }
}

fn default_threshold_type() -> String {
    "n_of_m".into()
}

/// The approval bar a Quorum Mode [`QuorumThreshold`] imposes on one session.
///
/// Produced only by [`QuorumThreshold::effective`], which is the **single**
/// implementation of that rule. It lives in `macp-core` because two crates
/// need it — `QuorumMode::effective_threshold` in `macp-modes` and
/// `evaluate_quorum_commitment_outcome` in `macp-policy` — and when they each
/// carried their own copy they disagreed: the mode truncated a fractional
/// `value` (`0.5` → `0`) while the evaluator ceiled it (`0.5` → `1`), so one
/// policy produced two different thresholds. RFC-MACP-0011 §7 forbids exactly
/// that ("implementations MUST derive the same quorum state and the same
/// commitment eligibility"). A third caller must call this, not re-derive it.
///
/// **Deliberately not `#[non_exhaustive]`**, unlike its neighbours in this
/// crate ([`crate::error::MacpError`], [`crate::mode::ModeResponse`],
/// [`crate::mode::MessageContext`], [`crate::session::Session`],
/// [`super::PolicyDecision`], [`super::CommitmentMode`]). That attribute binds
/// every crate except the defining one, so here it would force a `_` arm at
/// exactly the two call sites — `QuorumMode::effective_threshold` in
/// `macp-modes` and `evaluate_quorum_commitment_outcome` in `macp-policy` —
/// whose compile-time exhaustiveness *is* the guarantee unifying this rule
/// buys. A fail-closed `_` arm would be strictly worse for a governance
/// kernel: a future variant would silently decline instead of failing to
/// build, which is the same class of silent mis-handling as issue #145. Adding
/// a variant later is not a silent break either — `enum_variant_added` is a
/// major `cargo-semver-checks` lint and `release-plz.toml` sets
/// `semver_check = true`, so it blocks the release PR. The residual cost is
/// release coordination, not an undetected breakage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectiveThreshold {
    /// The rule imposes no bar (`value <= 0`, including the schema default),
    /// so the caller keeps its own default.
    ///
    /// The two callers' defaults **differ**, and unifying them is out of scope
    /// for the rounding fix: the mode falls back to the ApprovalRequest's
    /// `required_approvals`, while the evaluator applies no threshold check at
    /// all. See `ASSUMPTIONS.md`, "Quorum `threshold.value = 0`".
    Inert,
    /// This many approvals are required to seal a **positive** commitment.
    /// Never zero: a bar of zero would be met before any ballot was cast.
    Approvals(u32),
    /// No number of approvals can satisfy the rule, so the session can seal no
    /// positive commitment. Returned for any unrecognised `type` — which as of
    /// RFC-MACP-0012 1.2.0-draft includes `weighted`, removed from
    /// `quorum-rules.schema.json`'s enum and reserved by spec #110 because it
    /// never had a weights vocabulary, an electorate rule, or a weighted
    /// analogue of RFC-MACP-0011 §5's count-only termination arithmetic — and
    /// for a `percentage` over an empty participant set.
    ///
    /// Registration refuses an unrecognised type
    /// (`PolicyRegistry::validate_quorum_threshold`), so reaching that needs
    /// a directly-constructed `PolicyDefinition`; the empty-participant-set
    /// case is not catchable there, since registration has no participant
    /// count, and `QuorumMode::on_session_start` blocks it instead. It
    /// fails closed rather than silently reinterpreting the value as a raw
    /// approval count, which is what the old shared `_` arm did.
    Unsatisfiable,
}

impl QuorumThreshold {
    /// Resolve this threshold against a session with `total_participants`
    /// declared participants. See [`EffectiveThreshold`] for the contract —
    /// **both** the mode and the policy evaluator must resolve through here.
    ///
    /// Rounding is **ceiling** (`0.5` of a participant is a whole participant,
    /// and half a vote cannot approve anything), and the result has a floor of
    /// one approval. That floor is what makes `T = 0` unreachable: at `T = 0`
    /// a session is "ready to commit" with no ballot cast at all, and a
    /// negative commitment then seals with zero approvals (issue #145).
    pub fn effective(&self, total_participants: usize) -> EffectiveThreshold {
        // `is_sign_negative` would mis-handle NaN and -0.0; comparing against
        // the ordered predicate keeps NaN, 0.0 and negatives on one path.
        if self.value.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return EffectiveThreshold::Inert;
        }
        let required: f64 = match self.threshold_type.as_str() {
            "percentage" => {
                if total_participants == 0 {
                    // Unreachable through the mode (`QuorumMode::on_session_start`
                    // rejects an empty participant set) but reachable through a
                    // direct evaluator call; a share of nobody is unmeetable.
                    return EffectiveThreshold::Unsatisfiable;
                }
                (self.value / 100.0) * total_participants as f64
            }
            // `count` is this runtime's documented alias for `n_of_m`
            // (`docs/policy.md`); the canonical schema enum omits it and the
            // gap is tracked as spec issue #98.
            "n_of_m" | "count" => self.value,
            _ => return EffectiveThreshold::Unsatisfiable,
        };
        // `as u32` saturates on overflow, so an absurd `value` becomes an
        // unmeetable-but-finite bar rather than wrapping to a small one.
        EffectiveThreshold::Approvals((required.ceil() as u32).max(1))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AbstentionRules {
    #[serde(default)]
    pub counts_toward_quorum: bool,
    #[serde(default = "default_interpretation")]
    pub interpretation: String,
}

impl Default for AbstentionRules {
    fn default() -> Self {
        Self {
            counts_toward_quorum: false,
            interpretation: default_interpretation(),
        }
    }
}

fn default_interpretation() -> String {
    "neutral".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_policy_rules_defaults() {
        let rules = DecisionPolicyRules::default();
        assert_eq!(rules.voting.algorithm, "none");
        assert!((rules.voting.threshold - 0.5).abs() < f64::EPSILON);
        assert_eq!(rules.voting.quorum.quorum_type, "count");
        assert!(!rules.objection_handling.critical_severity_vetoes);
        assert_eq!(rules.objection_handling.veto_threshold, 1);
        assert_eq!(
            rules.objection_handling.critical_objection_action,
            CriticalObjectionAction::Deny
        );
        assert!(!rules.commitment.allow_decline_over_approval);
        assert!(!rules.evaluation.required_before_voting);
        assert!((rules.evaluation.minimum_confidence).abs() < f64::EPSILON);
        assert_eq!(rules.commitment.authority, "initiator_only");
        assert!(rules.commitment.designated_roles.is_empty());
        assert!(!rules.commitment.require_vote_quorum);
    }

    #[test]
    fn decision_policy_rules_deserialization() {
        let json = serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.6,
                "quorum": { "type": "percentage", "value": 75.0 },
                "weights": { "agent://fraud": 2.0, "agent://growth": 1.0 }
            },
            "objection_handling": {
                "critical_severity_vetoes": true,
                "veto_threshold": 2
            },
            "evaluation": {
                "required_before_voting": true,
                "minimum_confidence": 0.8
            },
            "commitment": {
                "authority": "designated_role",
                "designated_roles": ["agent://lead"],
                "require_vote_quorum": true
            }
        });

        let rules: DecisionPolicyRules = serde_json::from_value(json).unwrap();
        assert_eq!(rules.voting.algorithm, "majority");
        assert!((rules.voting.threshold - 0.6).abs() < f64::EPSILON);
        assert_eq!(rules.voting.quorum.quorum_type, "percentage");
        assert!((rules.voting.quorum.value - 75.0).abs() < f64::EPSILON);
        assert_eq!(*rules.voting.weights.get("agent://fraud").unwrap(), 2.0);
        assert!(rules.objection_handling.critical_severity_vetoes);
        assert_eq!(rules.objection_handling.veto_threshold, 2);
        assert!(rules.evaluation.required_before_voting);
        assert!((rules.evaluation.minimum_confidence - 0.8).abs() < f64::EPSILON);
        assert_eq!(rules.commitment.authority, "designated_role");
        assert_eq!(rules.commitment.designated_roles, vec!["agent://lead"]);
        assert!(rules.commitment.require_vote_quorum);
    }

    #[test]
    fn partial_deserialization_fills_defaults() {
        let json = serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        });
        let rules: DecisionPolicyRules = serde_json::from_value(json).unwrap();
        assert_eq!(rules.voting.algorithm, "unanimous");
        assert!((rules.voting.threshold - 0.5).abs() < f64::EPSILON);
        assert!(!rules.objection_handling.critical_severity_vetoes);
        assert_eq!(rules.objection_handling.veto_threshold, 1);
    }

    #[test]
    fn proposal_policy_rules_defaults() {
        let rules = ProposalPolicyRules::default();
        assert_eq!(rules.acceptance.criterion, "all_parties");
        assert_eq!(rules.counter_proposal.max_rounds, 0);
        assert!(!rules.rejection.terminal_on_any_reject);
        assert_eq!(rules.commitment.authority, "initiator_only");
    }

    #[test]
    fn proposal_policy_rules_deserialization() {
        let json = serde_json::json!({
            "acceptance": { "criterion": "counterparty" },
            "counter_proposal": { "max_rounds": 3 },
            "rejection": { "terminal_on_any_reject": true },
            "commitment": { "authority": "any_participant" }
        });
        let rules: ProposalPolicyRules = serde_json::from_value(json).unwrap();
        assert_eq!(rules.acceptance.criterion, "counterparty");
        assert_eq!(rules.counter_proposal.max_rounds, 3);
        assert!(rules.rejection.terminal_on_any_reject);
        assert_eq!(rules.commitment.authority, "any_participant");
    }

    #[test]
    fn task_policy_rules_defaults() {
        let rules = TaskPolicyRules::default();
        assert!(!rules.assignment.allow_reassignment_on_reject);
        assert!(!rules.completion.require_output);
        assert_eq!(rules.commitment.authority, "initiator_only");
    }

    #[test]
    fn task_policy_rules_deserialization() {
        let json = serde_json::json!({
            "assignment": { "allow_reassignment_on_reject": true },
            "completion": { "require_output": true },
            "commitment": { "authority": "initiator_only" }
        });
        let rules: TaskPolicyRules = serde_json::from_value(json).unwrap();
        assert!(rules.assignment.allow_reassignment_on_reject);
        assert!(rules.completion.require_output);
    }

    #[test]
    fn handoff_policy_rules_defaults() {
        let rules = HandoffPolicyRules::default();
        assert_eq!(rules.acceptance.implicit_accept_timeout_ms, 0);
        assert_eq!(rules.commitment.authority, "initiator_only");
    }

    #[test]
    fn handoff_policy_rules_deserialization() {
        let json = serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": 5000 },
            "commitment": { "authority": "any_participant" }
        });
        let rules: HandoffPolicyRules = serde_json::from_value(json).unwrap();
        assert_eq!(rules.acceptance.implicit_accept_timeout_ms, 5000);
        assert_eq!(rules.commitment.authority, "any_participant");
    }

    #[test]
    fn quorum_policy_rules_defaults() {
        let rules = QuorumPolicyRules::default();
        assert_eq!(rules.threshold.threshold_type, "n_of_m");
        assert!((rules.threshold.value).abs() < f64::EPSILON);
        assert!(!rules.abstention.counts_toward_quorum);
        assert_eq!(rules.abstention.interpretation, "neutral");
        assert_eq!(rules.commitment.authority, "initiator_only");
    }

    #[test]
    fn effective_threshold_ceils_and_floors_at_one() {
        let t = |kind: &str, value: f64| QuorumThreshold {
            threshold_type: kind.into(),
            value,
        };
        // Ceiling, not truncation — the divergence behind issue #145.
        assert_eq!(
            t("n_of_m", 0.5).effective(3),
            EffectiveThreshold::Approvals(1)
        );
        assert_eq!(
            t("count", 2.4).effective(3),
            EffectiveThreshold::Approvals(3)
        );
        // Percentage is a share of the declared participants.
        assert_eq!(
            t("percentage", 50.0).effective(3),
            EffectiveThreshold::Approvals(2)
        );
        // The floor keeps a bar of 0 unreachable.
        assert_eq!(
            t("percentage", 0.5).effective(3),
            EffectiveThreshold::Approvals(1)
        );
        // Non-positive and NaN are inert; the caller keeps its own default.
        assert_eq!(t("n_of_m", 0.0).effective(3), EffectiveThreshold::Inert);
        assert_eq!(t("n_of_m", -1.0).effective(3), EffectiveThreshold::Inert);
        assert_eq!(
            t("n_of_m", f64::NAN).effective(3),
            EffectiveThreshold::Inert
        );
        // Unimplemented and unknown types fail closed rather than being read
        // as a raw approval count, and so does a share of nobody.
        assert_eq!(
            t("weighted", 2.0).effective(3),
            EffectiveThreshold::Unsatisfiable
        );
        assert_eq!(
            t("two_thirds", 2.0).effective(3),
            EffectiveThreshold::Unsatisfiable
        );
        assert_eq!(
            t("percentage", 50.0).effective(0),
            EffectiveThreshold::Unsatisfiable
        );
        // An absurd value saturates instead of wrapping to a small bar.
        assert_eq!(
            t("n_of_m", 1e30).effective(3),
            EffectiveThreshold::Approvals(u32::MAX)
        );
    }

    #[test]
    fn quorum_policy_rules_deserialization() {
        let json = serde_json::json!({
            "threshold": { "type": "percentage", "value": 75.0 },
            "abstention": { "counts_toward_quorum": true, "interpretation": "implicit_reject" },
            "commitment": { "authority": "initiator_only" }
        });
        let rules: QuorumPolicyRules = serde_json::from_value(json).unwrap();
        assert_eq!(rules.threshold.threshold_type, "percentage");
        assert!((rules.threshold.value - 75.0).abs() < f64::EPSILON);
        assert!(rules.abstention.counts_toward_quorum);
        assert_eq!(rules.abstention.interpretation, "implicit_reject");
    }
}
