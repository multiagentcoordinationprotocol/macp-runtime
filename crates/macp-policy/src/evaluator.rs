use macp_core::decision::{DecisionState, Vote};
use macp_core::policy::rules::{
    DecisionPolicyRules, HandoffPolicyRules, ProposalPolicyRules, QuorumPolicyRules,
    TaskPolicyRules,
};
use macp_core::policy::{PolicyDecision, PolicyDefinition};
use std::collections::BTreeMap;

const SUPPORTED_SCHEMA_VERSION: u32 = 1;

fn check_schema_version(policy: &PolicyDefinition) -> Option<PolicyDecision> {
    if policy.schema_version != SUPPORTED_SCHEMA_VERSION {
        Some(PolicyDecision::Deny {
            reasons: vec![format!(
                "unsupported policy schema version: {} (supported: {})",
                policy.schema_version, SUPPORTED_SCHEMA_VERSION
            )],
        })
    } else {
        None
    }
}

/// Evaluate whether the governance policy allows a commitment in Decision Mode.
///
/// Checks:
/// 1. Evaluation confidence: do evaluations meet minimum_confidence?
/// 2. Objection veto: are there enough blocking objections to trigger veto?
/// 3. Quorum: have enough participants voted?
/// 4. Voting threshold: does the vote distribution satisfy the algorithm?
///
/// Returns `PolicyDecision::Allow` or `PolicyDecision::Deny` with reasons.
pub fn evaluate_decision_commitment(
    policy: &PolicyDefinition,
    state: &DecisionState,
    participants: &[String],
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: DecisionPolicyRules =
        serde_json::from_value(policy.rules.clone()).unwrap_or_default();

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    // 1. Check evaluation requirements (minimum confidence threshold)
    // RFC-MACP-0007: REVIEW evaluations are informational only and MUST NOT
    // satisfy confidence thresholds or "required before voting" checks.
    let qualifying_evaluations: Vec<_> = state
        .evaluations
        .iter()
        .filter(|e| {
            let rec = e.recommendation.to_uppercase();
            rec != "REVIEW"
        })
        .collect();

    if rules.evaluation.required_before_voting && rules.evaluation.minimum_confidence > 0.0 {
        let meets_confidence = qualifying_evaluations
            .iter()
            .any(|e| e.confidence >= rules.evaluation.minimum_confidence);
        if qualifying_evaluations.is_empty() || !meets_confidence {
            deny_reasons.push(format!(
                "no qualifying evaluation meets minimum confidence threshold: {:.2}",
                rules.evaluation.minimum_confidence
            ));
        }
    } else if rules.evaluation.required_before_voting && qualifying_evaluations.is_empty() {
        deny_reasons.push("evaluations required before voting but none provided (REVIEW evaluations are informational only)".into());
    }

    // 2. Check critical objections (veto by count of "critical" severity objections)
    // RFC-MACP-0007 §5: only Objections with severity "critical" trigger veto logic
    if rules.objection_handling.critical_severity_vetoes {
        let blocking: Vec<&str> = state
            .objections
            .iter()
            .filter(|o| o.severity == "critical")
            .map(|o| o.sender.as_str())
            .collect();
        if blocking.len() >= rules.objection_handling.veto_threshold as usize {
            deny_reasons.push(format!(
                "blocked by {} blocking objection(s) (veto threshold: {}), from: {}",
                blocking.len(),
                rules.objection_handling.veto_threshold,
                blocking.join(", ")
            ));
        }
    }

    // 3. Collect all votes across all proposals
    let total_voters = count_unique_voters(&state.votes);
    let participant_count = participants.len();

    // 4. Check vote quorum
    let quorum_met = check_quorum(
        &rules.voting.quorum.quorum_type,
        rules.voting.quorum.value,
        total_voters,
        participant_count,
    );
    if rules.commitment.require_vote_quorum && !quorum_met {
        deny_reasons.push(format!(
            "vote quorum not met: {} voters of {} participants (quorum: {} {})",
            total_voters,
            participant_count,
            rules.voting.quorum.value,
            rules.voting.quorum.quorum_type
        ));
    }

    // 5. Check voting threshold per the algorithm
    if rules.voting.algorithm != "none" {
        match check_voting_algorithm(
            &rules.voting.algorithm,
            rules.voting.threshold,
            &rules.voting.weights,
            &state.votes,
            participants,
        ) {
            VotingResult::Passed(reason) => allow_reasons.push(reason),
            VotingResult::Failed(reason) => deny_reasons.push(reason),
            VotingResult::NoVotes => {
                if rules.commitment.require_vote_quorum {
                    deny_reasons.push("no votes cast".into());
                }
            }
        }
    } else {
        allow_reasons.push("voting algorithm is 'none'; no vote threshold required".into());
    }

    if deny_reasons.is_empty() {
        if allow_reasons.is_empty() {
            allow_reasons.push("policy constraints satisfied".into());
        }
        PolicyDecision::Allow {
            reasons: allow_reasons,
        }
    } else {
        PolicyDecision::Deny {
            reasons: deny_reasons,
        }
    }
}

/// Count the number of unique voters across all proposals.
fn count_unique_voters(votes: &BTreeMap<String, BTreeMap<String, Vote>>) -> usize {
    let mut voters = std::collections::HashSet::new();
    for proposal_votes in votes.values() {
        for voter in proposal_votes.keys() {
            voters.insert(voter.clone());
        }
    }
    voters.len()
}

/// Check whether the quorum requirement is met.
/// Accepts "count", "n_of_m", and "percentage" as quorum types.
fn check_quorum(
    quorum_type: &str,
    value: f64,
    actual_voters: usize,
    total_participants: usize,
) -> bool {
    match quorum_type {
        "count" | "n_of_m" => actual_voters as f64 >= value,
        "percentage" => {
            if total_participants == 0 {
                value <= 0.0
            } else {
                let pct = (actual_voters as f64 / total_participants as f64) * 100.0;
                pct >= value
            }
        }
        _ => actual_voters as f64 >= value,
    }
}

enum VotingResult {
    Passed(String),
    Failed(String),
    NoVotes,
}

/// Check the voting algorithm against the collected votes.
///
/// Supports: majority, supermajority, unanimous, weighted, plurality.
fn check_voting_algorithm(
    algorithm: &str,
    threshold: f64,
    weights: &std::collections::HashMap<String, f64>,
    votes: &BTreeMap<String, BTreeMap<String, Vote>>,
    participants: &[String],
) -> VotingResult {
    // Aggregate approve/reject counts across all proposals.
    // RFC-MACP-0004: abstain votes are excluded from ratio denominators.
    let (approve_count, reject_count, _abstain_count, non_abstain_total) = aggregate_votes(votes);

    if non_abstain_total == 0 {
        return VotingResult::NoVotes;
    }

    match algorithm {
        "majority" => {
            let ratio = approve_count as f64 / non_abstain_total as f64;
            if ratio >= threshold {
                VotingResult::Passed(format!(
                    "majority vote passed: {:.1}% approve (threshold: {:.1}%)",
                    ratio * 100.0,
                    threshold * 100.0
                ))
            } else {
                VotingResult::Failed(format!(
                    "majority vote failed: {:.1}% approve, need >= {:.1}%",
                    ratio * 100.0,
                    threshold * 100.0
                ))
            }
        }
        "supermajority" => {
            let effective_threshold = if threshold > 0.5 {
                threshold
            } else {
                2.0 / 3.0
            };
            let ratio = approve_count as f64 / non_abstain_total as f64;
            if ratio >= effective_threshold {
                VotingResult::Passed(format!(
                    "supermajority vote passed: {:.1}% approve (threshold: {:.1}%)",
                    ratio * 100.0,
                    effective_threshold * 100.0
                ))
            } else {
                VotingResult::Failed(format!(
                    "supermajority vote failed: {:.1}% approve, need >= {:.1}%",
                    ratio * 100.0,
                    effective_threshold * 100.0
                ))
            }
        }
        "unanimous" => {
            // All declared participants must have voted "APPROVE"
            let all_voted = participants.iter().all(|p| {
                votes
                    .values()
                    .any(|pv| pv.get(p).map(|v| v.vote == "APPROVE").unwrap_or(false))
            });
            if all_voted && reject_count == 0 {
                VotingResult::Passed("unanimous vote passed: all participants approved".into())
            } else {
                VotingResult::Failed(format!(
                    "unanimous vote failed: {} approve, {} reject out of {} participants",
                    approve_count,
                    reject_count,
                    participants.len()
                ))
            }
        }
        "weighted" => {
            let (weighted_approve, weighted_total) = compute_weighted_votes(votes, weights);
            if weighted_total == 0.0 {
                return VotingResult::NoVotes;
            }
            let ratio = weighted_approve / weighted_total;
            if ratio >= threshold {
                VotingResult::Passed(format!(
                    "weighted vote passed: {:.1}% weighted approve (threshold: {:.1}%)",
                    ratio * 100.0,
                    threshold * 100.0
                ))
            } else {
                VotingResult::Failed(format!(
                    "weighted vote failed: {:.1}% weighted approve, need >= {:.1}%",
                    ratio * 100.0,
                    threshold * 100.0
                ))
            }
        }
        "plurality" => {
            if approve_count > reject_count {
                VotingResult::Passed(format!(
                    "plurality vote passed: {} approve vs {} reject",
                    approve_count, reject_count
                ))
            } else if approve_count == reject_count {
                VotingResult::Failed(format!(
                    "plurality vote tied: {} approve vs {} reject",
                    approve_count, reject_count
                ))
            } else {
                VotingResult::Failed(format!(
                    "plurality vote failed: {} approve vs {} reject",
                    approve_count, reject_count
                ))
            }
        }
        _ => {
            // Unknown or misspelled algorithm must not silently pass.
            VotingResult::Failed(format!(
                "unknown voting algorithm '{}'; supported: majority, supermajority, unanimous, weighted, plurality, none",
                algorithm
            ))
        }
    }
}

/// Aggregate votes into approve/reject/abstain counts.
///
/// RFC-MACP-0004: Abstain votes do NOT count toward approval or rejection
/// thresholds by default. The fourth element (`non_abstain_total`) is the
/// denominator for ratio-based algorithms (majority, supermajority, etc.).
fn aggregate_votes(
    votes: &BTreeMap<String, BTreeMap<String, Vote>>,
) -> (usize, usize, usize, usize) {
    let mut approve = 0usize;
    let mut reject = 0usize;
    let mut abstain = 0usize;

    for proposal_votes in votes.values() {
        for vote in proposal_votes.values() {
            match vote.vote.as_str() {
                "APPROVE" => approve += 1,
                "REJECT" => reject += 1,
                _ => abstain += 1, // ABSTAIN or other values don't count for/against
            }
        }
    }

    let non_abstain_total = approve + reject;
    (approve, reject, abstain, non_abstain_total)
}

/// Compute weighted votes using the configured weight map.
///
/// RFC-MACP-0004: Abstain votes are excluded from the weighted total
/// so they do not dilute the approval ratio.
fn compute_weighted_votes(
    votes: &BTreeMap<String, BTreeMap<String, Vote>>,
    weights: &std::collections::HashMap<String, f64>,
) -> (f64, f64) {
    let mut weighted_approve = 0.0f64;
    let mut weighted_total = 0.0f64;

    for proposal_votes in votes.values() {
        for (voter, vote) in proposal_votes {
            // Skip abstain votes — they don't count toward the threshold
            if vote.vote != "APPROVE" && vote.vote != "REJECT" {
                continue;
            }
            let weight = weights.get(voter).copied().unwrap_or(1.0);
            weighted_total += weight;
            if vote.vote == "APPROVE" {
                weighted_approve += weight;
            }
        }
    }

    (weighted_approve, weighted_total)
}

// ── Proposal Mode Evaluator ─────────────────────────────────────────

/// Evaluate whether the governance policy allows a commitment in Proposal Mode.
///
/// Checks:
/// - `counter_proposal.max_rounds`: if > 0, ensures counter-proposal count doesn't exceed limit
pub fn evaluate_proposal_commitment(
    policy: &PolicyDefinition,
    counter_proposal_count: usize,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: ProposalPolicyRules =
        serde_json::from_value(policy.rules.clone()).unwrap_or_default();

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    if rules.counter_proposal.max_rounds > 0
        && counter_proposal_count > rules.counter_proposal.max_rounds
    {
        deny_reasons.push(format!(
            "counter-proposal limit exceeded: {} of {} allowed",
            counter_proposal_count, rules.counter_proposal.max_rounds
        ));
    }

    if deny_reasons.is_empty() {
        if allow_reasons.is_empty() {
            allow_reasons.push("proposal policy constraints satisfied".into());
        }
        PolicyDecision::Allow {
            reasons: allow_reasons,
        }
    } else {
        PolicyDecision::Deny {
            reasons: deny_reasons,
        }
    }
}

// ── Task Mode Evaluator ─────────────────────────────────────────────

/// Evaluate whether the governance policy allows a commitment in Task Mode.
///
/// Checks:
/// - `completion.require_output`: if true, task completion must include non-empty output
pub fn evaluate_task_commitment(policy: &PolicyDefinition, has_output: bool) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: TaskPolicyRules = serde_json::from_value(policy.rules.clone()).unwrap_or_default();

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    if rules.completion.require_output && !has_output {
        deny_reasons.push("policy requires task output before commitment".into());
    }

    if deny_reasons.is_empty() {
        if allow_reasons.is_empty() {
            allow_reasons.push("task policy constraints satisfied".into());
        }
        PolicyDecision::Allow {
            reasons: allow_reasons,
        }
    } else {
        PolicyDecision::Deny {
            reasons: deny_reasons,
        }
    }
}

// ── Handoff Mode Evaluator ──────────────────────────────────────────

/// Evaluate whether the governance policy allows a commitment in Handoff Mode.
///
/// The RFC handoff rules (`acceptance.implicit_accept_timeout_ms`) are handled
/// at message-processing time via lazy evaluation, not at commitment evaluation.
/// This evaluator always allows.
pub fn evaluate_handoff_commitment(policy: &PolicyDefinition) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let _rules: HandoffPolicyRules =
        serde_json::from_value(policy.rules.clone()).unwrap_or_default();

    PolicyDecision::Allow {
        reasons: vec!["handoff policy constraints satisfied".into()],
    }
}

// ── Quorum Mode Evaluator ───────────────────────────────────────────

/// Evaluate whether the governance policy allows a commitment in Quorum Mode.
///
/// Checks:
/// - `abstention`: if `counts_toward_quorum` is false and `interpretation` is not "ignored",
///   abstentions may affect quorum calculation
/// - `threshold`: if set, checks that voter participation meets the threshold requirement
pub fn evaluate_quorum_commitment(
    policy: &PolicyDefinition,
    approve_count: usize,
    reject_count: usize,
    abstain_count: usize,
    total_participants: usize,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: QuorumPolicyRules = serde_json::from_value(policy.rules.clone()).unwrap_or_default();

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    // Determine effective voter count based on abstention rules
    let effective_voters = if rules.abstention.counts_toward_quorum {
        approve_count + reject_count + abstain_count
    } else {
        approve_count + reject_count
    };

    // Handle abstention interpretation
    let effective_reject_count = match rules.abstention.interpretation.as_str() {
        "implicit_reject" => reject_count + abstain_count,
        _ => reject_count, // "neutral" and "ignored" don't add to rejections
    };

    // If interpretation is "ignored", abstentions don't trigger any denial
    // If interpretation is "neutral" or "implicit_reject" and abstentions exist
    // but counts_toward_quorum is false, we just exclude them from voter count (already done above)

    // Check quorum threshold
    let quorum_met = check_quorum(
        &rules.threshold.threshold_type,
        rules.threshold.value,
        effective_voters,
        total_participants,
    );
    if rules.threshold.value > 0.0 && !quorum_met {
        deny_reasons.push(format!(
            "quorum not met: {} effective voters of {} participants (threshold: {} {})",
            effective_voters,
            total_participants,
            rules.threshold.value,
            rules.threshold.threshold_type
        ));
    }

    // Report effective rejection count for transparency
    if effective_reject_count > reject_count {
        allow_reasons.push(format!(
            "abstentions interpreted as implicit rejections: {} effective rejections",
            effective_reject_count
        ));
    }

    if deny_reasons.is_empty() {
        if allow_reasons.is_empty() {
            allow_reasons.push("quorum policy constraints satisfied".into());
        }
        PolicyDecision::Allow {
            reasons: allow_reasons,
        }
    } else {
        PolicyDecision::Deny {
            reasons: deny_reasons,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::decision::{DecisionPhase, Evaluation, Objection, Proposal};

    fn make_policy(rules: serde_json::Value) -> PolicyDefinition {
        PolicyDefinition {
            policy_id: "test-policy".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "test".into(),
            rules,
            schema_version: 1,
        }
    }

    fn make_state_with_votes(vote_entries: Vec<(&str, &str, &str)>) -> DecisionState {
        let mut proposals = BTreeMap::new();
        let mut votes: BTreeMap<String, BTreeMap<String, Vote>> = BTreeMap::new();

        for (proposal_id, voter, vote_value) in &vote_entries {
            proposals
                .entry(proposal_id.to_string())
                .or_insert_with(|| Proposal {
                    proposal_id: proposal_id.to_string(),
                    option: "option-1".into(),
                    rationale: "reason".into(),
                    sender: "initiator".into(),
                });
            votes.entry(proposal_id.to_string()).or_default().insert(
                voter.to_string(),
                Vote {
                    proposal_id: proposal_id.to_string(),
                    vote: vote_value.to_string(),
                    reason: String::new(),
                    sender: voter.to_string(),
                },
            );
        }

        DecisionState {
            proposals,
            evaluations: Vec::new(),
            objections: Vec::new(),
            votes,
            phase: DecisionPhase::Voting,
        }
    }

    fn participants() -> Vec<String> {
        vec![
            "agent://fraud".into(),
            "agent://growth".into(),
            "agent://compliance".into(),
        ]
    }

    // ── Voting algorithm: none ──────────────────────────────────────

    #[test]
    fn none_algorithm_always_allows() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" }
        }));
        let state = make_state_with_votes(vec![]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Voting algorithm: majority ──────────────────────────────────

    #[test]
    fn majority_passes_with_sufficient_approvals() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn majority_fails_with_insufficient_approvals() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn majority_passes_on_exact_threshold() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // 50% >= 50% passes (threshold comparison uses >=)
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Voting algorithm: supermajority ─────────────────────────────

    #[test]
    fn supermajority_passes_with_two_thirds() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "supermajority" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // 2/3 = 66.7% >= 66.7%
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn supermajority_fails_below_threshold() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "supermajority", "threshold": 0.75 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // 2/3 = 66.7% < 75%
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Voting algorithm: unanimous ─────────────────────────────────

    #[test]
    fn unanimous_passes_when_all_approve() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn unanimous_fails_with_any_reject() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn unanimous_fails_when_participant_missing() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            // compliance didn't vote
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Voting algorithm: weighted ──────────────────────────────────

    #[test]
    fn weighted_passes_with_heavy_approve() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": {
                    "agent://fraud": 3.0,
                    "agent://growth": 1.0,
                    "agent://compliance": 1.0
                }
            }
        }));
        // fraud (weight 3) approves, others reject => 3/5 = 60% > 50%
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "REJECT"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn weighted_fails_with_heavy_reject() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": {
                    "agent://fraud": 3.0,
                    "agent://growth": 1.0,
                    "agent://compliance": 1.0
                }
            }
        }));
        // fraud (weight 3) rejects, others approve => 2/5 = 40% < 50%
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Voting algorithm: plurality ─────────────────────────────────

    #[test]
    fn plurality_passes_when_approves_exceed_rejects() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "plurality" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn plurality_fails_on_tie() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "plurality" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "REJECT"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Objection veto logic (count-based) ──────────────────────────

    #[test]
    fn veto_blocks_commitment_when_blocking_objections_reach_threshold() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "too risky".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn veto_allows_when_objections_below_threshold() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 3 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // Only 1 blocking objection, threshold is 3
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "minor concern".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn veto_ignores_non_blocking_severity_objections() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // "high" severity is NOT "critical", so it should NOT trigger veto
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "serious concern".into(),
            severity: "high".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn veto_disabled_ignores_objections() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": false }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "critical issue".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Quorum checking ─────────────────────────────────────────────

    #[test]
    fn quorum_count_requirement() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 3 }
            },
            "commitment": { "require_vote_quorum": true }
        }));
        // Only 2 voters, need 3
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
        if let PolicyDecision::Deny { reasons } = &result {
            assert!(reasons.iter().any(|r| r.contains("quorum")));
        }
    }

    #[test]
    fn quorum_percentage_requirement() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "percentage", "value": 100.0 }
            },
            "commitment": { "require_vote_quorum": true }
        }));
        // Only 2 of 3 voted: 66.7% < 100%
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn quorum_not_required_by_default() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 100 }
            },
            "commitment": { "require_vote_quorum": false }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // Quorum not met, but not required => allow (majority is met)
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Evaluation requirements (confidence-based) ───────────────────

    #[test]
    fn evaluation_denies_when_below_confidence() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.8 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // Evaluation with confidence below threshold
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "proceed".into(),
            confidence: 0.5,
            reason: "uncertain".into(),
            sender: "agent://fraud".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn evaluation_allows_when_meets_confidence() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.8 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "proceed".into(),
            confidence: 0.9,
            reason: "good".into(),
            sender: "agent://fraud".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn evaluation_denies_when_none_provided_but_required() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.0 }
        }));
        let state = make_state_with_votes(vec![]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Regression: lending.conservative-shaped policy ──────────────
    // Reproduces the production bug where a supermajority decision with 100%
    // approval was denied (and the run cancelled) because the example agents
    // emitted Votes/Signals but no Evaluation messages. The policy requires
    // evaluations before voting (minimum_confidence 0.6); with no qualifying
    // evaluation the commitment is denied even though the vote tally passes.
    // Emitting a qualifying Evaluation (what the fixed agents now do) allows it.
    #[test]
    fn supermajority_requires_evaluation_then_allows_when_provided() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "supermajority",
                "threshold": 0.67,
                "quorum": { "type": "count", "value": 3 }
            },
            "evaluation": { "minimum_confidence": 0.6, "required_before_voting": true },
            "commitment": { "authority": "initiator_only", "require_vote_quorum": true }
        }));
        // 3 unique voters, all APPROVE → quorum (3) met, supermajority (100% ≥ 67%) met.
        let mut state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);

        // Without any Evaluation: denied on the required-before-voting check,
        // despite the vote tally passing. This is the bug.
        let denied = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(
            matches!(denied, PolicyDecision::Deny { .. }),
            "got: {:?}",
            denied
        );
        if let PolicyDecision::Deny { reasons } = &denied {
            assert!(
                reasons.iter().any(|r| r.contains("minimum confidence")),
                "expected confidence denial, got: {:?}",
                reasons
            );
        }

        // With a qualifying (non-REVIEW, confidence ≥ 0.6) Evaluation — what the
        // fixed specialist agents now emit before voting — the commitment passes.
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: 0.8,
            reason: "clear approval".into(),
            sender: "agent://fraud".into(),
        });
        let allowed = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(
            matches!(allowed, PolicyDecision::Allow { .. }),
            "got: {:?}",
            allowed
        );
    }

    // ── Default policy always allows ────────────────────────────────

    #[test]
    fn default_policy_always_allows() {
        let policy = crate::defaults::default_policy();
        let state = make_state_with_votes(vec![]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── No votes with required quorum ───────────────────────────────

    #[test]
    fn no_votes_with_required_quorum_denies() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 1 }
            },
            "commitment": { "require_vote_quorum": true }
        }));
        let state = make_state_with_votes(vec![]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Multiple deny reasons ───────────────────────────────────────

    #[test]
    fn multiple_deny_reasons_accumulated() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 10 }
            },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 },
            "commitment": { "require_vote_quorum": true }
        }));
        let mut state = make_state_with_votes(vec![("p1", "agent://fraud", "REJECT")]);
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "bad".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        if let PolicyDecision::Deny { reasons } = result {
            // Should have: veto, quorum, and voting threshold failures
            assert!(reasons.len() >= 2);
        } else {
            panic!("expected Deny");
        }
    }

    // ── Proposal evaluator ─────────────────────────────────────────

    #[test]
    fn proposal_allows_within_counter_limit() {
        let policy = make_policy(serde_json::json!({
            "counter_proposal": { "max_rounds": 5 }
        }));
        let result = super::evaluate_proposal_commitment(&policy, 3);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn proposal_denies_exceeding_counter_limit() {
        let policy = make_policy(serde_json::json!({
            "counter_proposal": { "max_rounds": 2 }
        }));
        let result = super::evaluate_proposal_commitment(&policy, 5);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn proposal_zero_limit_allows_any() {
        let policy = make_policy(serde_json::json!({
            "counter_proposal": { "max_rounds": 0 }
        }));
        let result = super::evaluate_proposal_commitment(&policy, 100);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Task evaluator ─────────────────────────────────────────────

    #[test]
    fn task_allows_when_output_not_required() {
        let policy = make_policy(serde_json::json!({
            "completion": { "require_output": false }
        }));
        let result = super::evaluate_task_commitment(&policy, false);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn task_allows_when_output_present_and_required() {
        let policy = make_policy(serde_json::json!({
            "completion": { "require_output": true }
        }));
        let result = super::evaluate_task_commitment(&policy, true);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn task_denies_when_no_output_and_required() {
        let policy = make_policy(serde_json::json!({
            "completion": { "require_output": true }
        }));
        let result = super::evaluate_task_commitment(&policy, false);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    // ── Handoff evaluator ──────────────────────────────────────────

    #[test]
    fn handoff_always_allows() {
        let policy = make_policy(serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": 5000 }
        }));
        let result = super::evaluate_handoff_commitment(&policy);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn handoff_allows_with_default_rules() {
        let policy = make_policy(serde_json::json!({}));
        let result = super::evaluate_handoff_commitment(&policy);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Quorum evaluator ───────────────────────────────────────────

    #[test]
    fn quorum_allows_with_abstention_counting_toward_quorum() {
        let policy = make_policy(serde_json::json!({
            "abstention": { "counts_toward_quorum": true, "interpretation": "neutral" }
        }));
        let result = super::evaluate_quorum_commitment(&policy, 2, 0, 1, 3);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn quorum_denies_when_threshold_not_met_excluding_abstentions() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 },
            "abstention": { "counts_toward_quorum": false, "interpretation": "neutral" }
        }));
        // 2 approve + 0 reject = 2 effective voters < 3 threshold
        let result = super::evaluate_quorum_commitment(&policy, 2, 0, 1, 5);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn quorum_allows_when_threshold_met_including_abstentions() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 },
            "abstention": { "counts_toward_quorum": true, "interpretation": "neutral" }
        }));
        // 2 approve + 0 reject + 1 abstain = 3 effective voters >= 3 threshold
        let result = super::evaluate_quorum_commitment(&policy, 2, 0, 1, 5);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn quorum_implicit_reject_interpretation() {
        let policy = make_policy(serde_json::json!({
            "abstention": { "counts_toward_quorum": true, "interpretation": "implicit_reject" }
        }));
        // Abstentions treated as rejections in the result
        let result = super::evaluate_quorum_commitment(&policy, 2, 0, 1, 3);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn quorum_denies_when_quorum_not_met() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 },
            "abstention": { "counts_toward_quorum": true }
        }));
        let result = super::evaluate_quorum_commitment(&policy, 1, 0, 0, 5);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn quorum_allows_when_quorum_met() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 2 },
            "abstention": { "counts_toward_quorum": true }
        }));
        let result = super::evaluate_quorum_commitment(&policy, 2, 1, 0, 5);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── REVIEW evaluation filtering ────────────────────────────────

    #[test]
    fn review_evaluation_does_not_satisfy_required_before_voting() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.5 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // Only REVIEW evaluations — these are informational and MUST NOT
        // satisfy the required_before_voting check.
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "REVIEW".into(),
            confidence: 0.9,
            reason: "needs more analysis".into(),
            sender: "agent://fraud".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn review_evaluation_does_not_satisfy_minimum_confidence() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.5 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // REVIEW evaluation with high confidence (filtered out)
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "REVIEW".into(),
            confidence: 0.9,
            reason: "informational only".into(),
            sender: "agent://fraud".into(),
        });
        // APPROVE evaluation with confidence below threshold
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: 0.3,
            reason: "low confidence approval".into(),
            sender: "agent://growth".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // REVIEW is filtered out; APPROVE at 0.3 < 0.5 threshold => deny
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn approve_evaluation_alongside_review_allows() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.5 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // REVIEW evaluation (filtered out from qualifying evaluations)
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "REVIEW".into(),
            confidence: 0.9,
            reason: "informational only".into(),
            sender: "agent://fraud".into(),
        });
        // APPROVE evaluation that meets the confidence threshold
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "APPROVE".into(),
            confidence: 0.8,
            reason: "high confidence approval".into(),
            sender: "agent://growth".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // APPROVE at 0.8 >= 0.5 threshold => allow
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Critical objection veto vs BLOCK evaluation ────────────────

    #[test]
    fn critical_severity_objection_triggers_veto() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // A single critical-severity objection should trigger veto
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "unacceptable risk".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn block_evaluation_does_not_trigger_objection_veto_logic() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        let mut state = make_state_with_votes(vec![]);
        state.proposals.insert(
            "p1".into(),
            Proposal {
                proposal_id: "p1".into(),
                option: "option-1".into(),
                rationale: "reason".into(),
                sender: "initiator".into(),
            },
        );
        // A BLOCK evaluation is NOT an objection — veto logic only looks
        // at the objections list, not evaluations.
        state.evaluations.push(Evaluation {
            proposal_id: "p1".into(),
            recommendation: "BLOCK".into(),
            confidence: 0.95,
            reason: "strongly disagree".into(),
            sender: "agent://compliance".into(),
        });
        // No objections in the objections list
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        // BLOCK evaluation != critical objection, so veto logic is not triggered
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Abstention excluded from voting ratio (RFC-MACP-0004) ──────

    #[test]
    fn abstain_excluded_from_majority_ratio() {
        // 3 approve, 0 reject, 2 abstain → ratio = 3/3 = 100%, not 3/5 = 60%
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.9
            }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
            ("p1", "agent://ops", "APPROVE"),
            ("p1", "agent://abstainer1", "ABSTAIN"),
            ("p1", "agent://abstainer2", "ABSTAIN"),
        ]);
        let participants = vec![
            "agent://fraud".into(),
            "agent://compliance".into(),
            "agent://ops".into(),
            "agent://abstainer1".into(),
            "agent://abstainer2".into(),
        ];
        let result = evaluate_decision_commitment(&policy, &state, &participants);
        // 3/3 = 100% >= 90% threshold → pass (abstentions excluded from denominator)
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn abstain_excluded_from_supermajority_ratio() {
        // 1 approve, 1 reject, 3 abstain → ratio = 1/2 = 50%, which fails 2/3 supermajority
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "supermajority",
                "threshold": 0.67
            }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
            ("p1", "agent://abstainer0", "ABSTAIN"),
            ("p1", "agent://abstainer1", "ABSTAIN"),
            ("p1", "agent://abstainer2", "ABSTAIN"),
        ]);
        let participants = vec![
            "agent://fraud".into(),
            "agent://compliance".into(),
            "agent://abstainer0".into(),
            "agent://abstainer1".into(),
            "agent://abstainer2".into(),
        ];
        let result = evaluate_decision_commitment(&policy, &state, &participants);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn all_abstain_returns_no_votes() {
        // All abstain → non_abstain_total = 0 → NoVotes → algorithm skipped
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5
            }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://abstainer0", "ABSTAIN"),
            ("p1", "agent://abstainer1", "ABSTAIN"),
            ("p1", "agent://abstainer2", "ABSTAIN"),
        ]);
        let participants = vec![
            "agent://abstainer0".into(),
            "agent://abstainer1".into(),
            "agent://abstainer2".into(),
        ];
        // No non-abstain votes → algorithm returns NoVotes → no deny
        let result = evaluate_decision_commitment(&policy, &state, &participants);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn weighted_votes_exclude_abstain() {
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.6,
                "weights": {
                    "agent://heavy": 10.0,
                    "agent://light": 1.0,
                    "agent://abstainer": 100.0
                }
            }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://heavy", "APPROVE"),
            ("p1", "agent://light", "REJECT"),
            ("p1", "agent://abstainer", "ABSTAIN"),
        ]);
        let participants = vec![
            "agent://heavy".into(),
            "agent://light".into(),
            "agent://abstainer".into(),
        ];
        // weighted_approve = 10.0, weighted_total = 10.0 + 1.0 = 11.0
        // ratio = 10/11 ≈ 0.909 >= 0.6 → pass
        // Without fix, would be 10/(10+1+100) = 0.09 → fail
        let result = evaluate_decision_commitment(&policy, &state, &participants);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    // ── Unknown voting algorithm ───────────────────────────────────

    #[test]
    fn unknown_voting_algorithm_denies_commitment() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majrity" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(
            matches!(result, PolicyDecision::Deny { .. }),
            "unknown voting algorithm must deny, got: {:?}",
            result
        );
    }
}
