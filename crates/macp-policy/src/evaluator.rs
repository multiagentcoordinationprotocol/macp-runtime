use macp_core::decision::{DecisionState, Vote};
use macp_core::policy::rules::{
    CriticalObjectionAction, DecisionPolicyRules, EffectiveThreshold, HandoffPolicyRules,
    ProposalPolicyRules, QuorumPolicyRules, TaskPolicyRules,
};
use macp_core::policy::{PolicyDecision, PolicyDefinition};
use std::collections::BTreeMap;

// RFC-MACP-0012 §3: "A runtime MUST accept every schema version it supports
// (`{1, 2, 3}`)", with no per-mode carve-out — so this gate is shared by all
// five standard modes.
//
// 1 → 2 is **additive**: version 2 only signals the descriptor MAY carry the
// Decision decline-gating fields (`commitment.allow_decline_over_approval`,
// `objection_handling.critical_objection_action`), so schema_version 1 policies
// stay valid.
//
// 2 → 3 is the first **semantic** bump: the same rules bytes can evaluate
// differently, because under version 3 every voting algorithm other than `none`
// is binding on an empty decisive tally (§4.1 "Empty tally"). Versions 1 and 2
// keep the fail-open legacy arm forever so stored sessions replay identically
// (§8), which is why the semantics are selected by the declared version on the
// *stored descriptor* and never by the runtime's release.
const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1, 2, 3];

fn check_schema_version(policy: &PolicyDefinition) -> Option<PolicyDecision> {
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&policy.schema_version) {
        Some(PolicyDecision::Deny {
            reasons: vec![format!(
                "unsupported policy schema version: {} (supported: {:?})",
                policy.schema_version, SUPPORTED_SCHEMA_VERSIONS
            )],
        })
    } else {
        None
    }
}

/// Parse the mode's rule schema from the bound policy. Rules were validated
/// at registration, so an eval-time parse failure means the definition was
/// corrupted or bypassed registration — deny loudly rather than silently
/// evaluating default (empty) rules as if the policy imposed nothing.
fn parse_rules<T: serde::de::DeserializeOwned>(
    policy: &PolicyDefinition,
) -> Result<T, PolicyDecision> {
    serde_json::from_value(policy.rules.clone()).map_err(|e| PolicyDecision::Deny {
        reasons: vec![format!(
            "policy '{}' rules failed to parse at evaluation time: {e}",
            policy.policy_id
        )],
    })
}

/// Evaluate whether the governance policy allows a *positive* commitment in
/// Decision Mode (the legacy, outcome-unaware entry point).
///
/// Equivalent to [`evaluate_decision_commitment_outcome`] with
/// `outcome_positive = true`; retained so existing callers and the ~30 unit
/// tests below keep compiling against a stable 3-argument signature.
pub fn evaluate_decision_commitment(
    policy: &PolicyDefinition,
    state: &DecisionState,
    participants: &[String],
) -> PolicyDecision {
    evaluate_decision_commitment_outcome(policy, state, participants, true)
}

/// Evaluate whether the governance policy allows a commitment in Decision Mode,
/// accounting for the commitment's `outcome_positive` flag.
///
/// Decision Mode permits both positive and negative committed outcomes
/// (RFC-MACP-0007 §6). The gating is **outcome-aware**: a positive commit must
/// clear the approval bar; a negative (decline) commit must be backed by a
/// *conclusive non-approval* — at least one explicit reject — not merely the
/// absence of approval (which, for `unanimous`, may just be incomplete
/// participation).
///
/// Checks:
/// 1. Evaluation confidence: do evaluations meet `minimum_confidence`? (both outcomes)
/// 2. Objection veto: critical objections, resolved per `critical_objection_action`.
/// 3. Quorum: have enough participants voted? (both outcomes)
/// 4. Voting threshold mapped to the requested outcome (see below).
///
/// Voting-result → validity mapping for a real algorithm (`algorithm != "none"`):
///
/// | `VotingResult` | approve commit | decline commit |
/// |---|---|---|
/// | `Passed`  | allowed | denied, unless `commitment.allow_decline_over_approval` **and** the decline guard passes |
/// | `Failed`  | denied  | allowed iff the decline guard passes |
/// | `NoVotes` | `schema_version >= 3`: denied; `<= 2`: denied iff quorum required | denied (no decisive reject can exist) |
///
/// The table governs **vote-authorized** commitments only. An
/// *objection-authorized* decline skips it entirely — see "Objection-authorized
/// decline" below.
///
/// **Empty decisive tally (`NoVotes`):** which of the two positive-commitment
/// readings applies is selected by the **policy's own `schema_version`**
/// (RFC-MACP-0012 §4.1). Under `schema_version >= 3` every algorithm other than
/// `none` is binding on its own, so a positive commitment is denied
/// unconditionally. Under `schema_version <= 2` the fail-open legacy arm
/// applies, where `commitment.require_vote_quorum` alone decides; §4.1 requires
/// implementations to keep that arm (so stored sessions replay identically,
/// §8) and requires they MUST NOT apply it to `schema_version >= 3` policies.
/// The discriminator is a property of the stored descriptor, never of the
/// runtime release: two sessions started by the same binary in the same
/// millisecond, one binding a v1 policy and one a v3 policy, must evaluate
/// differently. A **vote-authorized decline** is denied on an empty tally at
/// every schema version — no decisive reject can exist — so only the positive
/// column moves; an objection-authorized decline (below) never reaches this row
/// at all. `none` is untouched in both directions: it never enters the voting
/// block.
///
/// **The weighted electorate (every schema version).** Under `weighted`, the
/// `voting.weights` map *is* the electorate: a declared participant absent from
/// it weighs `0` and is **non-decisive** — outside both sides of the ratio, and
/// unable to satisfy the decline guard (RFC-MACP-0012 §4.1, and
/// `decision-rules.schema.json`'s "normative for EVERY schema version, not only
/// 3"). Unlike the empty-tally split above this is keyed off nothing, so it
/// reaches stored `schema_version <= 2` descriptors too; §8's "Bounded
/// exception — weight-`0` decisiveness" accepts that in writing, and
/// `docs/deployment.md` carries the operator note. The participation floor is
/// carved out and unchanged: a weight-`0` vote still counts toward
/// `voting.quorum` (see [`count_unique_voters`]). Under every other algorithm
/// `weights` is not consulted and nothing here applies.
///
/// **Negative weighted total:** a `weighted` round whose *cast* weights sum
/// below zero is out-of-schema (`voting.weights[*]` is `exclusiveMinimum: 0`
/// since spec #99, and was `minimum: 0` before it) and
/// short-circuits to `Failed` before any ratio is computed, so it takes the
/// `Failed` row above — an approve is denied, and a decline is allowed iff a
/// decisive explicit reject backs it. It previously reached the ratio, where the
/// negative denominator inverted `ratio >= threshold` and could report
/// `Passed`; that made this a *tightening* for an approve (`Passed` →
/// `Failed`) and, on the same round, a `DENY` → `ALLOW` move for a decline. A
/// total of exactly **zero** reports `NoVotes`, which is no longer a deferral:
/// §4.1 defines a tally whose total decisive weight is zero as *the* empty
/// decisive tally, and RFC-MACP-0007 §6.2 agrees it is `NoVotes` for every
/// algorithm at every schema version.
///
/// **Decline guard (universal reject-floor):** a decline backed by the vote
/// outcome requires at least one *decisive* explicit reject
/// (`decisive_reject_count > 0`; see [`count_decisive_rejects`]). A non-vote
/// must never authorize a finalized adverse decline, and neither may a ballot
/// from outside the weighted electorate. RFC-MACP-0007 §6.2 states the guard
/// "applies across all three voting results", so it gates the `Passed` row as
/// well: `allow_decline_over_approval` waives the approval *result*, not the
/// guard. The quorum gate (check 3) supplies the additional, opt-in
/// `require_vote_quorum` condition.
///
/// **Objection-authorized decline (`schema_version >= 2`):** when the policy
/// sets `objection_handling.critical_objection_action` to `finalize_decline`
/// and a standing critical objection blocks the positive direction, a decline
/// is authorized by the recorded `Objection` rather than by the tally.
/// RFC-MACP-0007 §6.2: such a decline "is not gated by the tri-state above and
/// is not subject to the decline guard" — the objection is itself the explicit,
/// attributable dissent the guard exists to require — so it is available at
/// every tally, the empty one included. Without it a `schema_version >= 3`
/// session with a non-`none` algorithm, an empty tally and a standing critical
/// objection could terminate only by expiry, the stuck state
/// `finalize_decline` exists to resolve. Three bounds. It is one-directional:
/// a *positive* commitment is still denied by the veto and still evaluated
/// against the voting block. It is scoped to the action: `deny` and `hold`
/// stay hard-stops in both directions. And it waives the tri-state and the
/// reject-floor only — checks 1 (`minimum_confidence`) and 3
/// (`require_vote_quorum`) are outcome-agnostic and still apply. That last
/// bound is the fail-closed reading of a §6.2 that names neither: the quorum
/// condition sits inside its decline-guard sentence, so a wider reading is
/// arguable and deliberately not taken here. No version check is needed:
/// `critical_objection_action` is a v2 field and a v1 descriptor that omits it
/// defaults to `Deny`, which never sets the flag.
///
/// **`none` exception:** with `algorithm == "none"` the decision is
/// initiator-driven and `outcome_positive` is taken at face value (a `none`
/// decision may legitimately carry no votes); the reject-floor does not apply.
/// Action/outcome consistency is still enforced upstream by
/// `validate_commitment_payload_for_session`.
///
/// Returns `PolicyDecision::Allow` or `PolicyDecision::Deny` with reasons.
pub fn evaluate_decision_commitment_outcome(
    policy: &PolicyDefinition,
    state: &DecisionState,
    participants: &[String],
    outcome_positive: bool,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: DecisionPolicyRules = match parse_rules(policy) {
        Ok(r) => r,
        Err(deny) => return deny,
    };

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();
    // Set by check 2 when a standing critical objection authorizes *this*
    // decline under `critical_objection_action: "finalize_decline"`. It is what
    // makes check 5 skippable — see RFC-MACP-0007 §6.2 there.
    let mut objection_authorized_decline = false;

    // 1. Check evaluation requirements (minimum confidence threshold).
    // RFC-MACP-0007: REVIEW evaluations are informational only and MUST NOT
    // satisfy confidence thresholds or "required before voting" checks. A
    // decline still needs a qualifying basis, so this gate is outcome-agnostic.
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

    // 2. Check critical objections (veto by count of "critical" severity objections).
    // RFC-MACP-0007 §5: only Objections with severity "critical" trigger veto
    // logic. How the veto resolves a commitment is operator-controlled via
    // `critical_objection_action` (default `deny` = historical hard-stop).
    if rules.objection_handling.critical_severity_vetoes {
        let blocking: Vec<&str> = state
            .objections
            .iter()
            .filter(|o| o.severity == "critical")
            .map(|o| o.sender.as_str())
            .collect();
        if blocking.len() >= rules.objection_handling.veto_threshold as usize {
            let detail = format!(
                "{} blocking objection(s) (veto threshold: {}), from: {}",
                blocking.len(),
                rules.objection_handling.veto_threshold,
                blocking.join(", ")
            );
            match rules.objection_handling.critical_objection_action {
                CriticalObjectionAction::Deny => {
                    deny_reasons.push(format!("blocked by {detail}"));
                }
                CriticalObjectionAction::Hold => {
                    // Deny at the evaluator layer (which leaves the session
                    // open); the reason marks it as an escalation hold rather
                    // than a permanent denial.
                    deny_reasons.push(format!(
                        "held for escalation by {detail} (critical_objection_action=hold)"
                    ));
                }
                CriticalObjectionAction::FinalizeDecline => {
                    if outcome_positive {
                        deny_reasons.push(format!(
                            "veto blocks a positive commitment: {detail} (critical_objection_action=finalize_decline)"
                        ));
                    } else {
                        // RFC-MACP-0007 §6.2 "Objection-authorized decline":
                        // the authorization is the recorded critical
                        // `Objection`, not the voting result, so check 5 is
                        // skipped entirely for this direction. Set only here —
                        // the positive branch above keeps evaluating policy.
                        objection_authorized_decline = true;
                        allow_reasons.push(format!(
                            "critical-objection veto finalized as a decline: {detail}"
                        ));
                    }
                }
            }
        }
    }

    // 3. Collect all votes across all proposals.
    //
    // `total_voters` is deliberately *not* weight-aware: RFC-MACP-0012 §4.1
    // says a weight-`0` vote "still counts as a vote cast for the
    // `voting.quorum` participation floor, which this rule does not alter".
    // The decline guard is the opposite — RFC-MACP-0007 §6.2 counts only
    // *decisive* rejects — so the two counts diverge under `weighted`.
    let total_voters = count_unique_voters(&state.votes);
    let participant_count = participants.len();
    let decisive_reject_count =
        count_decisive_rejects(&rules.voting.algorithm, &rules.voting.weights, &state.votes);

    // 4. Check vote quorum (outcome-agnostic — a decline needs the same quorum
    //    as an approve when `require_vote_quorum` is set).
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

    // 5. Map the voting algorithm result to the requested outcome.
    //
    // An objection-authorized decline skips this block whole: RFC-MACP-0007
    // §6.2 says such a decline "is not gated by the tri-state above and is not
    // subject to the decline guard", because the recorded critical `Objection`
    // is itself the explicit, attributable dissent the guard exists to require.
    // The `none` arm is tested first so the skip cannot swallow the face-value
    // allow reason — `none` never had a tri-state to be exempted from, and its
    // reason set must not change.
    if rules.voting.algorithm == "none" {
        // `none`: initiator-driven; outcome taken at face value (no reject-floor).
        allow_reasons.push("voting algorithm is 'none'; no vote threshold required".into());
    } else if !objection_authorized_decline {
        match check_voting_algorithm(
            &rules.voting.algorithm,
            rules.voting.threshold,
            &rules.voting.weights,
            &state.votes,
            participants,
        ) {
            VotingResult::Passed(reason) => {
                if outcome_positive {
                    allow_reasons.push(reason);
                } else if !rules.commitment.allow_decline_over_approval {
                    deny_reasons.push(format!(
                        "vote passed the approval threshold but a decline was requested; set commitment.allow_decline_over_approval to permit an executive override ({reason})"
                    ));
                } else if decisive_reject_count > 0 {
                    allow_reasons.push(format!(
                        "decline authorized over a passing approval vote (allow_decline_over_approval=true): {reason}"
                    ));
                } else {
                    // RFC-MACP-0007 §6.2: the decline guard "applies across
                    // all three voting results". `allow_decline_over_approval`
                    // waives the *approval result*, not the guard, so a round
                    // with no decisive dissent — an all-approve tally, or one
                    // whose only rejects were cast by participants outside
                    // `voting.weights` and are therefore non-decisive — still
                    // has nothing to finalize an adverse outcome on.
                    //
                    // The explanatory clause is algorithm-conditional on
                    // purpose. RFC-MACP-0012 §8's "Bounded exception —
                    // weight-`0` decisiveness" is only about the `weighted`
                    // half of this arm; the arm itself is reached by every
                    // algorithm, and a `majority` policy has no
                    // `voting.weights` at all, so naming that key there would
                    // point the operator at a knob their descriptor does not
                    // carry.
                    let detail = if rules.voting.algorithm == "weighted" {
                        "a REJECT from a participant outside voting.weights is non-decisive, \
                         and incomplete participation is not a rejection"
                    } else {
                        "no REJECT was cast; neither an abstention nor a missing ballot is a \
                         rejection"
                    };
                    deny_reasons.push(format!(
                        "allow_decline_over_approval permits a decline over a passing vote, but no decisive reject backs it ({detail}): {reason}"
                    ));
                }
            }
            VotingResult::Failed(reason) => {
                if outcome_positive {
                    deny_reasons.push(reason);
                } else if decisive_reject_count > 0 {
                    // Decline guard satisfied: the approval bar was not met and
                    // there is at least one decisive explicit reject backing
                    // the decline.
                    allow_reasons.push(format!("decline backed by conclusive rejection: {reason}"));
                } else {
                    // Approval failed only through incomplete participation
                    // (no decisive explicit reject) — must not finalize an
                    // adverse decline.
                    deny_reasons.push(format!(
                        "approval threshold not met but no decisive reject to justify a decline (incomplete participation is not a rejection): {reason}"
                    ));
                }
            }
            VotingResult::NoVotes => {
                if outcome_positive {
                    // RFC-MACP-0012 §4.1 "Empty tally (schema_version >= 3)":
                    // every algorithm other than `none` is binding on its own,
                    // so an empty decisive tally denies a positive commitment
                    // whatever `require_vote_quorum` says. `>= 3` rather than
                    // `== 3` so a future schema version inherits the current,
                    // fail-closed semantics instead of silently falling back to
                    // the fail-open arm below.
                    //
                    // Below 3, §4.1's "Legacy empty-tally rule" applies and is
                    // retained verbatim: implementations MUST keep it so stored
                    // sessions replay identically (§8), and MUST NOT apply it to
                    // `schema_version >= 3`. The discriminator is the *stored
                    // descriptor's* declared version (§8 item 3), not the
                    // runtime's release, so nothing here consults `semantics_rev`.
                    if policy.schema_version >= 3 {
                        deny_reasons.push(format!(
                            "no decisive votes cast: under policy schema_version {} the '{}' \
                             voting algorithm is binding on an empty tally and does not \
                             authorize a positive commitment (RFC-MACP-0012 §4.1)",
                            policy.schema_version, rules.voting.algorithm
                        ));
                    } else if rules.commitment.require_vote_quorum {
                        deny_reasons.push("no votes cast".into());
                    }
                } else {
                    // "decisive" rather than merely "explicit", to stay
                    // parallel with the `Passed` and `Failed` arms above and
                    // with `docs/policy.md`: under `weighted` an explicit
                    // `REJECT` from a participant outside `voting.weights` is
                    // not enough, which is exactly how this arm becomes
                    // reachable on a complete ballot set.
                    deny_reasons.push(
                        "no votes cast; a decline requires at least one decisive explicit reject vote"
                            .into(),
                    );
                }
            }
        }
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

/// Count the explicit `REJECT` ballots that are **decisive** under the bound
/// voting algorithm — the figure RFC-MACP-0007 §6.2's decline guard is defined
/// over ("`reject_count > 0`, where `reject_count` counts decisive rejects").
///
/// Under `weighted` the `voting.weights` map **is** the electorate
/// (RFC-MACP-0012 §4.1): a declared participant absent from it has weight `0`,
/// and a weight-`0` ballot "is accepted as a message and preserved in history,
/// but it is **non-decisive** … it contributes to neither side of the weighted
/// ratio, does not enter the decisive tally, and does not satisfy the decline
/// guard". Under every other algorithm `weights` is not consulted at all and
/// every non-abstain ballot is decisive, so the raw [`aggregate_votes`] reject
/// count is already the decisive count.
///
/// This rule is keyed off nothing — §4.1 states it is "normative for **every**
/// schema version", and `decision-rules.schema.json` spells the same rule out
/// as "normative for EVERY schema version, not only 3". It is therefore *not*
/// gated on `policy.schema_version` the way the empty-tally arm is, and
/// RFC-MACP-0012 §8's "Bounded exception — weight-`0` decisiveness" accepts the
/// resulting break in stored-session replay in writing.
///
/// A negative weight (out-of-schema, reachable only from a directly-constructed
/// `PolicyDefinition`) is non-decisive too: `> 0.0` is the electorate test, so a
/// voter the map gives a negative weight sits outside the electorate for the
/// purposes of this guard. That is deliberately *not* the same test
/// [`compute_weighted_votes`] applies — it sums every cast weight, negative ones
/// included — so the two diverge whenever a negative weight fails to drag the
/// total below zero: `{a: 1.0, b: -0.5}` sums to `+0.5`, reaches the ratio with
/// `b` contributing, and yet does not count `b`'s `REJECT` here. Only when the
/// *total* goes negative does [`check_voting_algorithm`] short-circuit to
/// `Failed` and make the divergence moot. It is left documented rather than
/// resolved because `exclusiveMinimum: 0` makes the whole family unauthorable
/// through registration.
fn count_decisive_rejects(
    algorithm: &str,
    weights: &std::collections::HashMap<String, f64>,
    votes: &BTreeMap<String, BTreeMap<String, Vote>>,
) -> usize {
    if algorithm != "weighted" {
        let (_, reject_count, _, _) = aggregate_votes(votes);
        return reject_count;
    }
    votes
        .values()
        .flat_map(|proposal_votes| proposal_votes.iter())
        .filter(|(voter, vote)| {
            vote.vote == "REJECT" && weights.get(*voter).copied().unwrap_or(0.0) > 0.0
        })
        .count()
}

/// Count the number of unique voters across all proposals.
///
/// Deliberately **not** weight-aware, and must not become so: RFC-MACP-0012
/// §4.1 carves the participation floor out of the weighted-electorate rule — a
/// weight-`0` vote "still counts as a vote cast for the `voting.quorum`
/// participation floor, which this rule does not alter". Only the decline guard
/// and the ratio narrow; see [`count_decisive_rejects`].
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
            // A *negative* total is out-of-schema: `voting.weights`'s
            // `additionalProperties` is
            // `{"type":"number","exclusiveMinimum":0}`
            // (`decision-rules.schema.json`, spec #99), so registration
            // already refuses
            // it and only a directly-constructed `PolicyDefinition` can get
            // here. It used to escape the `== 0.0` guard below and reach the
            // ratio, where a negative denominator *inverts*
            // `ratio >= threshold`: `{fraud: 1.0, growth: -2.0}` with fraud
            // REJECTing and growth APPROVing gave `-2.0 / -1.0 = 2.0`, so the
            // round read as 200% approval and passed. Fail it instead — no
            // ratio over a negative denominator is meaningful.
            if weighted_total < 0.0 {
                return VotingResult::Failed(format!(
                    "weighted vote failed: the weights of the votes cast sum to {weighted_total:.1}; \
                     voting.weights values must be > 0, so no approval ratio is meaningful"
                ));
            }
            // A total of exactly zero **is** the empty decisive tally, not a
            // deferral. Spec #99 made `voting.weights[*]`
            // `exclusiveMinimum: 0` with `minProperties: 1`, so an explicit-`0`
            // or empty weight map is no longer authorable at all; the reachable
            // cause is now a ballot set cast entirely by participants *outside*
            // the map, who weigh `0` (RFC-MACP-0012 §4.1's electorate rule).
            // §4.1 defines exactly that state: "a tally whose total decisive
            // weight is zero **is** the empty decisive tally … This state is
            // **NoVotes**", and "Implementations MUST NOT compute a weighted
            // ratio with a zero denominator". See `zero_decisive_weight_is_no_votes`.
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
/// so they do not dilute the approval ratio. They are skipped *before* the
/// weight lookup, and must stay that way — an abstention is not an
/// observer-weight question.
///
/// **The `weights` map is the electorate.** A voter absent from it weighs `0`,
/// not `1.0`: RFC-MACP-0012 §4.1 — "a declared participant absent from the map
/// has weight `0` (an observer is expressed by omission, and the schema
/// rejects explicit `0` values and an empty map)" — and
/// `decision-rules.schema.json`'s `voting.weights.description` adds that the
/// rule "is normative for EVERY schema version, not only 3". A weight-`0`
/// ballot therefore contributes to neither returned figure, which is what makes
/// an all-unlisted ballot set sum to a zero decisive total and read as the
/// empty tally. This runtime previously defaulted an unlisted voter to `1.0`,
/// which is the opposite; RFC-MACP-0012 §8's "Bounded exception — weight-`0`
/// decisiveness" accepts the resulting stored-replay break, and
/// `docs/deployment.md` carries the operator-facing note.
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
            // Omission means weight `0` — the map is the electorate, not a set
            // of overrides on an implicit default of `1.0`.
            let weight = weights.get(voter).copied().unwrap_or(0.0);
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
/// Legacy wrapper: assumes a positive outcome.
pub fn evaluate_proposal_commitment(
    policy: &PolicyDefinition,
    counter_proposal_count: usize,
) -> PolicyDecision {
    evaluate_proposal_commitment_outcome(policy, counter_proposal_count, true)
}

/// Outcome-aware Proposal Mode evaluation.
///
/// Checks:
/// - `counter_proposal.max_rounds`: if > 0, a **positive** commitment must not
///   exceed the round limit. A negative (terminal-reject) commitment is the
///   legitimate exit from an over-long negotiation and is not blocked by the
///   round limit — denying it would trap the session until TTL expiry.
pub fn evaluate_proposal_commitment_outcome(
    policy: &PolicyDefinition,
    counter_proposal_count: usize,
    outcome_positive: bool,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: ProposalPolicyRules = match parse_rules(policy) {
        Ok(r) => r,
        Err(deny) => return deny,
    };

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    if rules.counter_proposal.max_rounds > 0
        && counter_proposal_count > rules.counter_proposal.max_rounds
    {
        if outcome_positive {
            deny_reasons.push(format!(
                "counter-proposal limit exceeded: {} of {} allowed",
                counter_proposal_count, rules.counter_proposal.max_rounds
            ));
        } else {
            allow_reasons
                .push("round limit exceeded but outcome is a decline: limit not applicable".into());
        }
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
/// Legacy wrapper: assumes a positive outcome.
pub fn evaluate_task_commitment(policy: &PolicyDefinition, has_output: bool) -> PolicyDecision {
    evaluate_task_commitment_outcome(policy, has_output, true)
}

/// Outcome-aware Task Mode evaluation.
///
/// Checks:
/// - `completion.require_output`: if true, a **positive** (completed)
///   commitment must include non-empty output. A negative commitment (a
///   `TaskFail` terminal report) legitimately has no output and is not gated
///   by the output requirement.
pub fn evaluate_task_commitment_outcome(
    policy: &PolicyDefinition,
    has_output: bool,
    outcome_positive: bool,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: TaskPolicyRules = match parse_rules(policy) {
        Ok(r) => r,
        Err(deny) => return deny,
    };

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    if rules.completion.require_output && !has_output {
        if outcome_positive {
            deny_reasons.push("policy requires task output before commitment".into());
        } else {
            allow_reasons
                .push("negative outcome (task failure): output requirement not applicable".into());
        }
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
    evaluate_handoff_commitment_outcome(policy, true)
}

/// Outcome-aware Handoff Mode evaluation. Both accepted handoffs (positive)
/// and declined handoffs (negative) are policy-clean; the rules the RFC
/// defines for handoff (`acceptance.implicit_accept_timeout_ms`) act at
/// message-processing time, not commitment time.
pub fn evaluate_handoff_commitment_outcome(
    policy: &PolicyDefinition,
    _outcome_positive: bool,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let _rules: HandoffPolicyRules = match parse_rules(policy) {
        Ok(r) => r,
        Err(deny) => return deny,
    };

    PolicyDecision::Allow {
        reasons: vec!["handoff policy constraints satisfied".into()],
    }
}

// ── Quorum Mode Evaluator ───────────────────────────────────────────

/// Evaluate whether the governance policy allows a commitment in Quorum Mode.
/// Legacy wrapper: assumes a positive outcome.
pub fn evaluate_quorum_commitment(
    policy: &PolicyDefinition,
    approve_count: usize,
    reject_count: usize,
    abstain_count: usize,
    total_participants: usize,
) -> PolicyDecision {
    evaluate_quorum_commitment_outcome(
        policy,
        approve_count,
        reject_count,
        abstain_count,
        total_participants,
        true,
    )
}

/// Outcome-aware Quorum Mode evaluation.
///
/// RFC-MACP-0012 §4.2: the policy `threshold` **overrides the
/// `required_approvals` from the ApprovalRequest** — it is an approval-count
/// bar, exactly as the mode itself reads it. (An earlier revision of this
/// evaluator reinterpreted the same field as a *participation* quorum over
/// approve+reject voters — a second, conflicting meaning of one field, which
/// both denied legitimate declines and double-gated approvals. That reading
/// was non-conformant and has been removed; a distinct participation-quorum
/// rule field, if desired, needs an RFC schema addition first.)
///
/// Checks:
/// - `threshold`: for a **positive** commitment, approvals must meet the
///   effective threshold, resolved by
///   [`QuorumThreshold::effective`](macp_core::policy::rules::QuorumThreshold::effective)
///   — the one implementation `QuorumMode::effective_threshold` also calls, so
///   a policy cannot mean two different bars in the two layers. It ceils and
///   floors at 1, and reports `weighted`/unrecognised types as unsatisfiable
///   rather than as a raw approval count. A **negative** (decline) commitment
///   is the legitimate terminal when approval is not reached — the threshold
///   does not gate it (RFC-MACP-0011 §4b). Note the mode adds a gate this
///   evaluator cannot: it refuses the decline when *no ballot has been cast*,
///   which needs ballot counts this signature does not carry.
/// - `abstention.interpretation`: `implicit_reject` is reported for
///   transparency (the effective rejection count) but does not gate.
pub fn evaluate_quorum_commitment_outcome(
    policy: &PolicyDefinition,
    approve_count: usize,
    reject_count: usize,
    abstain_count: usize,
    total_participants: usize,
    outcome_positive: bool,
) -> PolicyDecision {
    if let Some(deny) = check_schema_version(policy) {
        return deny;
    }
    let rules: QuorumPolicyRules = match parse_rules(policy) {
        Ok(r) => r,
        Err(deny) => return deny,
    };

    let mut deny_reasons: Vec<String> = Vec::new();
    let mut allow_reasons: Vec<String> = Vec::new();

    // Approval threshold (RFC-0012 §4.2). Resolved by
    // `QuorumThreshold::effective`, which is the single implementation of this
    // rule — `QuorumMode::effective_threshold` calls the same function, so the
    // two layers cannot drift apart again. They did: this arm ceiled a
    // fractional `value` while the mode truncated it (issue #145), and the
    // comment that used to sit here asserted a parity that did not exist.
    match rules.threshold.effective(total_participants) {
        // No bar configured. NOTE: the mode's fallback here is the
        // ApprovalRequest's `required_approvals`, while this evaluator applies
        // no bar at all — a residual divergence outside the scope of the
        // rounding fix, recorded in `ASSUMPTIONS.md`.
        EffectiveThreshold::Inert => {}
        EffectiveThreshold::Approvals(required) => {
            let required = required as usize;
            if outcome_positive && approve_count < required {
                deny_reasons.push(format!(
                    "approval threshold not met: {} of {} required approvals ({} {})",
                    approve_count, required, rules.threshold.value, rules.threshold.threshold_type
                ));
            } else if !outcome_positive {
                allow_reasons
                    .push("negative outcome: approval threshold does not gate a decline".into());
            }
        }
        EffectiveThreshold::Unsatisfiable => {
            // Fail closed: an unimplemented or unrecognised `threshold.type`
            // (or a percentage over an empty participant set) must not be
            // silently reinterpreted as a raw approval count, which is what
            // the old `_` arm did. A decline stays allowed for the same reason
            // it is allowed under a met-able bar — RFC-MACP-0011 §4b makes an
            // unreachable threshold the legitimate trigger for a negative
            // commitment.
            if outcome_positive {
                deny_reasons.push(format!(
                    "approval threshold '{}' cannot be satisfied by this runtime \
                     ({} participants): no positive commitment is possible",
                    rules.threshold.threshold_type, total_participants
                ));
            } else {
                allow_reasons
                    .push("negative outcome: approval threshold does not gate a decline".into());
            }
        }
    }

    // Handle abstention interpretation (reported for transparency; not gating)
    let effective_reject_count = match rules.abstention.interpretation.as_str() {
        "implicit_reject" => reject_count + abstain_count,
        _ => reject_count, // "neutral" and "ignored" don't add to rejections
    };
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

    // ── Weighted degenerate totals (both now defined, neither deferred) ──
    //
    // Two arithmetic facts these tests turn on, both easy to get wrong:
    //
    // 1. `compute_weighted_votes` sums only the *cast* APPROVE/REJECT weights,
    //    so the sign of the total is set by the weight map, not by which way
    //    the ballots went. `{a: 1.0, b: -1.0}` with both agents voting sums to
    //    exactly **0.0**, which RFC-MACP-0012 §4.1 defines as the empty
    //    decisive tally and therefore `NoVotes` — a named outcome, not a
    //    deferral. (This map was once called schema-legal here; spec #99 moved
    //    `voting.weights.additionalProperties` to `exclusiveMinimum: 0`, so it
    //    is refused on `b` at registration and survives only as a
    //    directly-constructed `PolicyDefinition`.) A negative total needs the
    //    negative weight to outweigh the positive ones, e.g.
    //    `{a: 1.0, b: -2.0}`.
    // 2. A negative total was **never** reported as `NoVotes`: the guard it
    //    escaped was `weighted_total == 0.0`, which a negative value does not
    //    match. It fell straight through to `weighted_approve /
    //    weighted_total`, and a negative denominator *inverts* the
    //    `ratio >= threshold` comparison. That is the actual hole: with
    //    `{fraud: 1.0, growth: -2.0}`, fraud REJECTing and growth APPROVing
    //    gave `-2.0 / -1.0 = 2.0 >= 0.5` → `Passed`, i.e. a positive
    //    commitment **allowed** on a reject from the only non-negative voter.
    //    So the change is a tightening in the approve direction (`Passed` →
    //    `Failed`) and, for the same round, a `DENY` → `ALLOW` move in the
    //    decline direction (a decline over `Passed` was refused; over `Failed`
    //    it is allowed by the reject-floor).

    /// Drive `check_voting_algorithm` directly, so the `VotingResult` variant
    /// itself is pinned rather than only the `Allow`/`Deny` it maps onto.
    fn weighted_result(
        weights: &[(&str, f64)],
        vote_entries: Vec<(&str, &str, &str)>,
    ) -> VotingResult {
        let state = make_state_with_votes(vote_entries);
        let weights: std::collections::HashMap<String, f64> = weights
            .iter()
            .map(|(voter, weight)| ((*voter).to_string(), *weight))
            .collect();
        check_voting_algorithm("weighted", 0.5, &weights, &state.votes, &participants())
    }

    /// Drive `check_voting_algorithm` with a policy's *own* `voting` rules, so
    /// a test can pin the `VotingResult` variant and the `PolicyDecision` for
    /// the same descriptor without restating the weight map twice.
    fn weighted_result_for(
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
    ) -> VotingResult {
        let rules: DecisionPolicyRules = parse_rules(policy).expect("test rules must parse");
        check_voting_algorithm(
            &rules.voting.algorithm,
            rules.voting.threshold,
            &rules.voting.weights,
            &state.votes,
            participants,
        )
    }

    fn negative_weighted_policy() -> PolicyDefinition {
        make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                // Out-of-schema: `voting.weights[*]` is `exclusiveMinimum: 0`
                // (spec #99; it was `minimum: 0` before, which already refused
                // negatives), so registration refuses this. Only a
                // directly-constructed `PolicyDefinition` — which is how these
                // tests build one — can reach the evaluator with it.
                "weights": {
                    "agent://fraud": 1.0,
                    "agent://growth": -2.0
                }
            }
        }))
    }

    /// The round that exposes the inverted comparison: the only non-negative
    /// voter REJECTs and the negative-weight voter APPROVEs, so
    /// `weighted_approve / weighted_total` was `-2.0 / -1.0 = 2.0`.
    fn inverted_ratio_votes() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "APPROVE"),
        ]
    }

    #[test]
    fn negative_weighted_total_fails_the_round_instead_of_inverting_the_comparison() {
        // The hole: a negative denominator flipped `ratio >= threshold`, so
        // this round read as 200% weighted approval and `Passed` — a positive
        // commitment allowed over a reject from the only voter whose weight is
        // in-schema. A negative total admits no meaningful ratio at all.
        assert!(matches!(
            weighted_result(
                &[("agent://fraud", 1.0), ("agent://growth", -2.0)],
                inverted_ratio_votes(),
            ),
            VotingResult::Failed(_)
        ));
        let state = make_state_with_votes(inverted_ratio_votes());
        assert!(
            matches!(
                evaluate_decision_commitment(&negative_weighted_policy(), &state, &participants()),
                PolicyDecision::Deny { .. }
            ),
            "a negative weighted total must not seal a positive commitment"
        );

        // The mirror round — fraud APPROVEs, growth REJECTs — already failed
        // before this change (`1.0 / -1.0 = -1.0 < 0.5`), but for the wrong
        // reason. Pin it too, so the arm is `Failed` for every negative total
        // rather than only for the ones the inverted ratio happened to reject.
        assert!(matches!(
            weighted_result(
                &[("agent://fraud", 1.0), ("agent://growth", -2.0)],
                vec![
                    ("p1", "agent://fraud", "APPROVE"),
                    ("p1", "agent://growth", "REJECT"),
                ],
            ),
            VotingResult::Failed(_)
        ));
    }

    #[test]
    fn negative_weighted_total_allows_a_decline_backed_by_an_explicit_reject() {
        // The decline-direction delta this change deliberately accepts, and
        // the reason it is not purely a tightening. On the same round: as
        // `Passed` the decline was refused ("vote passed the approval
        // threshold but a decline was requested"); as `Failed` it is allowed,
        // because the universal reject-floor is satisfied by fraud's explicit
        // REJECT. The round is genuinely decided and an explicit reject backs
        // the decline, which is the right outcome — but the direction of
        // change is DENY -> ALLOW.
        let state = make_state_with_votes(inverted_ratio_votes());
        assert!(matches!(
            decline(&negative_weighted_policy(), &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn negative_weighted_total_still_denies_a_decline_with_no_explicit_reject() {
        // Same negative total, reached with every ballot an APPROVE, so
        // `reject_count == 0`. The reject-floor holds: participation that is
        // merely contradictory must never finalize an adverse decline. The
        // decline was refused before this change too (`-1.0 / -1.0 = 1.0`
        // read as `Passed`), so only the `VotingResult` moves here — which is
        // exactly why the variant is asserted directly and not just the
        // `PolicyDecision`.
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
        ]);
        assert!(matches!(
            weighted_result(
                &[("agent://fraud", 1.0), ("agent://growth", -2.0)],
                vec![
                    ("p1", "agent://fraud", "APPROVE"),
                    ("p1", "agent://growth", "APPROVE"),
                ],
            ),
            VotingResult::Failed(_)
        ));
        assert!(matches!(
            decline(&negative_weighted_policy(), &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn zero_decisive_weight_is_no_votes() {
        // Re-based, not a new test: this was `zero_weighted_total_still_returns_
        // no_votes`, whose premise — that `voting.weights[*]` is `minimum: 0`
        // *inclusive*, so an all-zero weight map is schema-legal, and what it
        // ought to mean was spec issue #98 item 3 — died with spec #99. The
        // bound is now `exclusiveMinimum: 0` with `minProperties: 1`, so an
        // explicit-`0` or empty map is unauthorable and the shape below is the
        // only remaining route to a zero decisive total: a ballot set cast
        // entirely by participants *outside* the map, who weigh `0` under
        // RFC-MACP-0012 §4.1's electorate rule. §4.1 names the state — "a tally
        // whose total decisive weight is zero **is** the empty decisive tally
        // … This state is **NoVotes**" — so the assertion survives verbatim
        // while its justification moves from a deferral to a conformance pin.
        let unlisted_ballots = vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
        ];
        assert!(matches!(
            weighted_result(&[("agent://compliance", 1.0)], unlisted_ballots.clone()),
            VotingResult::NoVotes
        ));

        // The sharp end of the same claim: `NoVotes` denies a decline
        // unconditionally, where `Failed` — which is what an unlisted voter
        // weighing `1.0` produced — would allow it on the explicit rejects
        // above. So this asserts the electorate rule is real and not just a
        // variant name.
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": { "agent://compliance": 1.0 }
            }
        }));
        let state = make_state_with_votes(unlisted_ballots);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    // ── The weighted electorate (RFC-MACP-0012 §4.1, every schema version) ──
    //
    // `voting.weights` *is* the electorate: a declared participant absent from
    // it weighs `0` and is non-decisive — outside both sides of the ratio, and
    // unable to satisfy RFC-MACP-0007 §6.2's decline guard. Only the
    // `voting.quorum` participation floor is carved out. None of this is keyed
    // on `schema_version`; `decision-rules.schema.json` says the rule is
    // "normative for EVERY schema version, not only 3", and §8's "Bounded
    // exception — weight-`0` decisiveness" accepts the stored-replay break.

    /// The canonical shape: `agent://a` is the whole electorate.
    fn single_voter_weighted_policy(commitment: Option<serde_json::Value>) -> PolicyDefinition {
        let mut rules = serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": { "agent://a": 1.0 }
            }
        });
        if let Some(commitment) = commitment {
            rules["commitment"] = commitment;
        }
        make_policy(rules)
    }

    fn abc_participants() -> Vec<String> {
        vec!["agent://a".into(), "agent://b".into(), "agent://c".into()]
    }

    #[test]
    fn an_unlisted_voter_is_non_decisive_under_weighted() {
        // Criterion 1. `agent://b` is declared but absent from `weights`, so it
        // weighs `0`; its REJECT is the only ballot. The decisive tally is
        // therefore **empty**, which §4.1 says is `NoVotes` for every
        // algorithm, "never **Failed**" — and the distinction is the whole
        // observable, so assert the variant through `check_voting_algorithm`
        // rather than the `PolicyDecision` it maps onto.
        //
        // Before the electorate rule, `b` weighed `1.0`: total `1.0`, approve
        // share `0.0 < 0.5`, result `Failed`.
        let weights: std::collections::HashMap<String, f64> =
            [("agent://a".to_string(), 1.0)].into_iter().collect();
        let state = make_state_with_votes(vec![("p1", "agent://b", "REJECT")]);
        let result =
            check_voting_algorithm("weighted", 0.5, &weights, &state.votes, &abc_participants());
        assert!(
            matches!(result, VotingResult::NoVotes),
            "an unlisted voter's ballot must leave the decisive tally empty (NoVotes), \
             not produce a decided Failed round"
        );
    }

    #[test]
    fn a_weight_zero_reject_does_not_authorize_a_decline() {
        // Criterion 2, and the unit-test expression of
        // `decision_weighted_zero_weight_v1.json`'s discriminator. §6.2: "under
        // `weighted` a `REJECT` cast by a weight-`0` participant is
        // non-decisive and does not count". Swept across all three schema
        // versions because the electorate rule is keyed on none of them —
        // §8's bounded exception reaches stored v1 and v2 descriptors too.
        //
        // Before: `b` weighed `1.0`, the round read `Failed`, `reject_count`
        // was 1 and the decline was ALLOWED at every version.
        let state = make_state_with_votes(vec![("p1", "agent://b", "REJECT")]);
        for schema_version in [1u32, 2, 3] {
            let mut policy = single_voter_weighted_policy(None);
            policy.schema_version = schema_version;
            let decision = decline(&policy, &state, &abc_participants());
            assert!(
                matches!(decision, PolicyDecision::Deny { .. }),
                "schema_version {schema_version}: a weight-0 REJECT must not authorize a \
                 decline, got: {decision:?}"
            );
        }
    }

    #[test]
    fn a_weight_zero_reject_does_not_authorize_a_decline_over_a_passed_vote() {
        // Criterion 3 — RFC-MACP-0012 §8's "Bounded exception — weight-`0`
        // decisiveness", the one configuration where the pre-#99 text *did*
        // define the outcome this rule reverses. No conformance fixture covers
        // it (the spec dropped the fixture because explicit-`0` descriptors are
        // no longer admissible), so this unit test is the only thing that
        // discharges it.
        //
        // `a` is the whole electorate and approves, so the round Passes.
        // `b` is unlisted and rejects. With `allow_decline_over_approval: true`
        // the knob waives the approval *result*, but §6.2's guard "applies
        // across all three voting results", and `b`'s ballot is non-decisive —
        // so there is no dissent to finalize. Before: the `Passed` arm consulted
        // only the knob and ALLOWED the decline.
        let policy = single_voter_weighted_policy(Some(
            serde_json::json!({ "allow_decline_over_approval": true }),
        ));
        let state = make_state_with_votes(vec![
            ("p1", "agent://a", "APPROVE"),
            ("p1", "agent://b", "REJECT"),
        ]);
        assert!(
            matches!(
                weighted_result_for(&policy, &state, &abc_participants()),
                VotingResult::Passed(_)
            ),
            "the listed voter approved, so the round must Pass — otherwise this test \
             is not exercising the Passed arm at all"
        );
        let decision = decline(&policy, &state, &abc_participants());
        assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "a weight-0 REJECT must not authorize a decline over a passing vote, got: \
             {decision:?}"
        );
    }

    #[test]
    fn a_listed_voters_reject_still_authorizes_a_decline_over_a_passed_vote() {
        // Criterion 4, and the reason criterion 3 is not vacuous: the same
        // shape with `b` inside the electorate at weight `1.0`. The round still
        // Passes (`1.0 / 2.0 = 0.5 >= 0.5`, inclusive), `b`'s REJECT is now
        // decisive, and `allow_decline_over_approval` does what it says. An
        // implementation that broke the knob outright would satisfy criterion 3
        // and fail here.
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": { "agent://a": 1.0, "agent://b": 1.0 }
            },
            "commitment": { "allow_decline_over_approval": true }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://a", "APPROVE"),
            ("p1", "agent://b", "REJECT"),
        ]);
        assert!(matches!(
            weighted_result_for(&policy, &state, &abc_participants()),
            VotingResult::Passed(_)
        ));
        let decision = decline(&policy, &state, &abc_participants());
        assert!(
            matches!(decision, PolicyDecision::Allow { .. }),
            "a decisive REJECT under allow_decline_over_approval must still authorize a \
             decline, got: {decision:?}"
        );
    }

    #[test]
    fn weighted_quorum_counts_weight_zero_ballots() {
        // Criterion 5 — §4.1's explicit carve-out: a weight-`0` vote "still
        // counts as a vote cast for the `voting.quorum` participation floor,
        // which this rule does not alter". Two ballots are cast (`a` abstains,
        // unlisted `b` rejects) against `{"type":"count","value":2}`, so the
        // quorum gate is SATISFIED; the decisive tally is nonetheless empty, so
        // the commitment is denied by the v3 empty-tally rule instead. The
        // reason assertion is what distinguishes the two denials — a
        // `PolicyDecision::Deny` alone would not.
        let mut policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "weighted",
                "threshold": 0.5,
                "weights": { "agent://a": 1.0 },
                "quorum": { "type": "count", "value": 2 }
            },
            "commitment": { "require_vote_quorum": true }
        }));
        policy.schema_version = 3;
        let state = make_state_with_votes(vec![
            ("p1", "agent://a", "ABSTAIN"),
            ("p1", "agent://b", "REJECT"),
        ]);
        let decision =
            evaluate_decision_commitment_outcome(&policy, &state, &abc_participants(), true);
        let PolicyDecision::Deny { reasons } = &decision else {
            panic!("an empty decisive tally must deny a positive commitment at v3: {decision:?}");
        };
        let joined = reasons.join(" | ");
        assert!(
            !joined.contains("vote quorum not met"),
            "the participation floor must not narrow with the electorate — two ballots \
             were cast against a quorum of 2, got: {joined}"
        );
        assert!(
            joined.contains("no decisive votes cast"),
            "the denial must come from the empty-tally rule, not from quorum, got: {joined}"
        );
    }

    #[test]
    fn an_unlisted_voter_cannot_carry_a_positive_round() {
        // Criterion 8 — THE POSITIVE DIRECTION, and the widest and most common
        // behavioural change in this work. Criteria 1-7 are all reject-side and
        // not one of them can observe this reversal.
        //
        // Worked example, reproduced verbatim in
        // `docs/deployment.md`: weights `{"agent://a": 1.0}`, participants `a`,
        // `b`, `c`; `b` and `c` cast APPROVE, `a` casts REJECT;
        // `voting.threshold: 0.5`.
        //
        // Before: every voter weighed `1.0`, so the total was `3.0`, the approve
        // share `2.0 / 3.0 = 0.667 >= 0.5`, the result `Passed`, and a positive
        // `Commitment` was ACCEPTED into history.
        //
        // After: the total decisive weight is `1.0` (only `a` is in the
        // electorate, and `a` rejected), the approve share is `0.0`, the result
        // is `Failed`, and the commitment is DENIED.
        //
        // This is the majority-approves-but-the-weighted-voter-dissents shape —
        // the most natural reason to reach for `weighted` at all — and it is
        // authorable today with an ordinary, schema-valid, registered policy.
        let policy = single_voter_weighted_policy(None);
        let state = make_state_with_votes(vec![
            ("p1", "agent://a", "REJECT"),
            ("p1", "agent://b", "APPROVE"),
            ("p1", "agent://c", "APPROVE"),
        ]);
        let result = weighted_result_for(&policy, &state, &abc_participants());
        assert!(
            matches!(result, VotingResult::Failed(_)),
            "two unlisted APPROVEs must not outvote the electorate's only REJECT; \
             expected Failed"
        );
        let decision =
            evaluate_decision_commitment_outcome(&policy, &state, &abc_participants(), true);
        assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "a positive commitment carried only by unlisted voters must be denied, got: \
             {decision:?}"
        );
    }

    #[test]
    fn empty_decisive_tally_is_no_votes_for_every_algorithm() {
        // Conformance pin for `check_voting_algorithm`'s front-of-dispatch
        // `non_abstain_total == 0 => NoVotes` return. RFC-MACP-0012 §4.1 makes
        // it a MUST: the empty decisive tally "is **NoVotes** for *every*
        // algorithm, never **Failed**", and implementations "MUST report
        // NoVotes so that two conformant runtimes agree on what they
        // observed". Two dispatched arms would disagree if reached —
        // `plurality` reads a 0-0 tie as `Failed`, and `unanimous`'s universally
        // quantified predicate is *vacuously true* over an empty declared set,
        // which §4.1 says "MUST NOT be taken as a pass". So no phase may remove
        // this short-circuit; issue #147 proposed exactly that and #99 did not
        // adopt it.
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "ABSTAIN"),
            ("p1", "agent://growth", "ABSTAIN"),
            ("p1", "agent://compliance", "ABSTAIN"),
        ]);
        let no_weights = std::collections::HashMap::new();

        // An algorithm name nothing recognises: the `_` arm returns
        // `Failed("unknown voting algorithm ...")`. Seeing `NoVotes` is proof
        // the return happened *before* the match on `algorithm`, not inside
        // some arm of it.
        assert!(
            matches!(
                check_voting_algorithm(
                    "no-such-algorithm",
                    0.5,
                    &no_weights,
                    &state.votes,
                    &participants()
                ),
                VotingResult::NoVotes
            ),
            "the zero-non-abstain short-circuit must run in front of algorithm dispatch"
        );

        // And no dispatched algorithm may be reached with an empty tally —
        // `unanimous`, for one, would otherwise report `Failed` here.
        for algorithm in [
            "majority",
            "supermajority",
            "unanimous",
            "weighted",
            "plurality",
        ] {
            assert!(
                matches!(
                    check_voting_algorithm(
                        algorithm,
                        0.5,
                        &no_weights,
                        &state.votes,
                        &participants()
                    ),
                    VotingResult::NoVotes
                ),
                "{algorithm} must not be dispatched with zero non-abstain votes"
            );
        }
    }

    #[test]
    fn zero_participant_unanimous_is_no_votes_not_a_pass() {
        // The sharp end of the test above, and newly *reachable*: Decision now
        // accepts `SessionStart` with `participants: []`, so a session with an
        // empty declared roster and an empty vote map is authorable over the
        // wire rather than only constructible in a test.
        //
        // `check_voting_algorithm`'s `unanimous` arm is
        // `participants.iter().all(…)`, which is **vacuously true** over an
        // empty slice; with `reject_count == 0` it would return `Passed` — a
        // positive pass on a session nobody voted in. RFC-MACP-0012 §4.1 names
        // the case in terms: "With **zero** declared participants the
        // universally quantified predicate is vacuously true and MUST NOT be
        // taken as a pass."
        //
        // The **only** thing standing between this runtime and that outcome is
        // the front-of-dispatch `non_abstain_total == 0` short-circuit, and
        // nothing in the fixture corpus pins it —
        // `decision_zero_participants.json` stops at a `FORBIDDEN` `Proposal`
        // and never reaches policy evaluation, which its own `_comment` says.
        // This test is therefore the sole guard against a later
        // "simplification" of that short-circuit.
        let no_participants: Vec<String> = vec![];
        let no_weights = std::collections::HashMap::new();
        let no_votes = make_state_with_votes(vec![]);

        let result = check_voting_algorithm(
            "unanimous",
            0.5,
            &no_weights,
            &no_votes.votes,
            &no_participants,
        );
        // `VotingResult` is not `Debug`, so name the two wrong answers in the
        // message instead: `Passed` is the vacuous pass §4.1 forbids, `Failed`
        // is the disagreement with a conformant peer it also forbids.
        assert!(
            matches!(result, VotingResult::NoVotes),
            "a zero-participant unanimous round with no ballots must report NoVotes, \
             never a vacuous Passed and never Failed"
        );

        // And through the full evaluator, so the variant actually maps onto a
        // refusal rather than being reported and then ignored. `schema_version:
        // 3` makes the empty tally binding on its own (§4.1), with no
        // `require_vote_quorum` in the descriptor to do the work instead.
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        policy.schema_version = 3;
        let decision = evaluate_decision_commitment_outcome(
            &policy,
            &no_votes,
            &no_participants,
            true, // outcome_positive
        );
        let PolicyDecision::Deny { reasons } = &decision else {
            panic!(
                "a positive commitment in a zero-participant session must be denied, \
                 got: {decision:?}"
            );
        };
        assert!(
            reasons.iter().any(|r| r.contains("no decisive votes cast")),
            "the denial must come from the empty-tally rule, got: {reasons:?}"
        );
    }

    #[test]
    fn empty_tally_denies_a_positive_commitment_under_schema_version_3() {
        // RFC-MACP-0012 §4.1 "Empty tally (schema_version >= 3)": every
        // algorithm other than `none` is binding on its own, so an empty
        // decisive tally denies a positive commitment — with **no**
        // `require_vote_quorum` anywhere in the descriptor, which is the whole
        // point, since the legacy arm gates this same case on that flag alone.
        //
        // The assertion reads the deny *reasons*, not just the variant: a
        // `schema_version: 3` policy is denied by `check_schema_version` until
        // this phase widens `SUPPORTED_SCHEMA_VERSIONS`, so a variant-only
        // assertion would pass against the unfixed runtime.
        let empty = make_state_with_votes(vec![]);
        for (algorithm, threshold) in [
            ("majority", 0.5),
            ("supermajority", 0.67),
            ("unanimous", 0.5),
            ("weighted", 0.5),
            ("plurality", 0.5),
        ] {
            let mut policy = make_policy(serde_json::json!({
                "voting": {
                    "algorithm": algorithm,
                    "threshold": threshold,
                    "weights": { "agent://fraud": 1.0 }
                }
            }));
            policy.schema_version = 3;
            match evaluate_decision_commitment(&policy, &empty, &participants()) {
                PolicyDecision::Deny { reasons } => {
                    let joined = reasons.join(" | ");
                    assert!(
                        joined.contains("no decisive votes cast")
                            && joined.contains("schema_version 3")
                            && joined.contains(algorithm),
                        "{algorithm}: the denial must cite the empty tally under \
                         schema_version 3, got: {joined}"
                    );
                    assert!(
                        !joined.contains("unsupported policy schema version"),
                        "{algorithm}: schema_version 3 must be accepted, not \
                         version-denied: {joined}"
                    );
                }
                other => panic!("{algorithm}: expected a deny, got {other:?}"),
            }
        }
    }

    #[test]
    fn empty_tally_under_schema_version_2_still_follows_require_vote_quorum() {
        // RFC-MACP-0012 §4.1 "Legacy empty-tally rule (schema_version <= 2)"
        // and "Retaining the legacy arm": implementations MUST keep the
        // fail-open arm so stored sessions replay identically (§8). Both rules
        // objects and the reason string below are the ones
        // `no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum`
        // has always pinned, re-declared here at `schema_version: 2`.
        //
        // The `Allow` half is the load-bearing one — it proves the legacy arm
        // survived. A runtime that fail-closed every schema version would still
        // pass the `Deny` half.
        let empty = make_state_with_votes(vec![]);

        let mut permissive = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" },
            "commitment": { "require_vote_quorum": false }
        }));
        permissive.schema_version = 2;
        assert!(
            approves(&permissive, &empty, &participants()),
            "at schema_version 2, without require_vote_quorum an unvoted positive \
             commitment is not blocked"
        );

        let mut binding = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" },
            "commitment": { "require_vote_quorum": true }
        }));
        binding.schema_version = 2;
        match evaluate_decision_commitment(&binding, &empty, &participants()) {
            PolicyDecision::Deny { reasons } => assert!(
                reasons.iter().any(|r| r == "no votes cast"),
                "the legacy denial keeps its exact reason string, got: {reasons:?}"
            ),
            other => panic!("expected a deny, got {other:?}"),
        }
    }

    #[test]
    fn none_at_schema_version_3_allows_an_empty_tally_in_both_directions() {
        // §4.1's empty-tally bullets end with "`none` — unaffected; no voting
        // constraint is enforced at any tally". The conformance corpus
        // previously pinned `none` only at v2, so a runtime that folded `none`
        // into the fail-closed arm passed every fixture in the repository —
        // spec #99 added two `decision_none_v3_*` fixtures for exactly that,
        // and this is their unit-level guard. It holds because `none` never
        // enters the voting block at all.
        let empty = make_state_with_votes(vec![]);
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" }
        }));
        policy.schema_version = 3;
        assert!(
            matches!(
                evaluate_decision_commitment(&policy, &empty, &participants()),
                PolicyDecision::Allow { .. }
            ),
            "`none` at schema_version 3 must allow a positive commitment on an empty tally"
        );
        assert!(
            matches!(
                decline(&policy, &empty, &participants()),
                PolicyDecision::Allow { .. }
            ),
            "`none` at schema_version 3 must allow a decline on an empty tally: the \
             reject-floor does not reach `none` (RFC-MACP-0007 §6.2)"
        );
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
    fn quorum_denies_positive_when_approvals_below_threshold_despite_participation() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 },
            "abstention": { "counts_toward_quorum": true, "interpretation": "neutral" }
        }));
        // RFC-0012 §4.2: threshold is an approval-count bar, not a
        // participation quorum. 2 approvals < 3 required — the fact that an
        // abstention brings "participation" to 3 must not approve it. (This
        // test previously asserted Allow under the removed participation
        // reading.)
        let result = super::evaluate_quorum_commitment(&policy, 2, 0, 1, 5);
        assert!(matches!(result, PolicyDecision::Deny { .. }));
    }

    #[test]
    fn quorum_allows_positive_when_approvals_meet_threshold() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 }
        }));
        let result = super::evaluate_quorum_commitment(&policy, 3, 1, 1, 5);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn quorum_decline_not_gated_by_threshold() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 3 }
        }));
        // A negative (decline) commitment is the legitimate terminal when the
        // approval bar is unreachable — the threshold must not deny it.
        let result = super::evaluate_quorum_commitment_outcome(&policy, 0, 2, 3, 5, false);
        assert!(matches!(result, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn quorum_percentage_threshold_is_approval_share_of_participants() {
        let policy = make_policy(serde_json::json!({
            "threshold": { "type": "percentage", "value": 60.0 }
        }));
        // 60% of 5 participants = 3 required approvals.
        let denied = super::evaluate_quorum_commitment(&policy, 2, 0, 0, 5);
        assert!(matches!(denied, PolicyDecision::Deny { .. }));
        let allowed = super::evaluate_quorum_commitment(&policy, 3, 0, 0, 5);
        assert!(matches!(allowed, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn task_fail_not_gated_by_require_output() {
        let policy = make_policy(serde_json::json!({
            "completion": { "require_output": true }
        }));
        // Positive completion without output: denied.
        let denied = super::evaluate_task_commitment_outcome(&policy, false, true);
        assert!(matches!(denied, PolicyDecision::Deny { .. }));
        // Negative outcome (TaskFail) has no output by nature: allowed.
        let allowed = super::evaluate_task_commitment_outcome(&policy, false, false);
        assert!(matches!(allowed, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn proposal_decline_not_gated_by_max_rounds() {
        let policy = make_policy(serde_json::json!({
            "counter_proposal": { "max_rounds": 2 }
        }));
        // Positive convergence over the round limit: denied.
        let denied = super::evaluate_proposal_commitment_outcome(&policy, 5, true);
        assert!(matches!(denied, PolicyDecision::Deny { .. }));
        // Terminal-reject decline after the same rounds: allowed (the decline
        // is the exit from an over-long negotiation).
        let allowed = super::evaluate_proposal_commitment_outcome(&policy, 5, false);
        assert!(matches!(allowed, PolicyDecision::Allow { .. }));
    }

    #[test]
    fn handoff_decline_allowed() {
        let policy = make_policy(serde_json::json!({}));
        let result = super::evaluate_handoff_commitment_outcome(&policy, false);
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

    // ── Schema version gate ─────────────────────────────────────────

    #[test]
    fn schema_version_2_is_accepted() {
        // RFC-MACP-0012 §3: v2 is additive; a v2 descriptor must not be denied
        // for version reasons. Use a passing majority vote so any denial could
        // only come from the version gate.
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        policy.schema_version = 2;
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
        ]);
        assert!(matches!(
            evaluate_decision_commitment(&policy, &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn schema_version_3_is_accepted() {
        // RFC-MACP-0012 §3: "A runtime MUST accept every schema version it
        // supports (`{1, 2, 3}`)". `none` is used so the only denial available
        // is the version gate itself — under §4.1, `none` is exempt from the
        // v3 empty-tally rule.
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" }
        }));
        policy.schema_version = 3;
        let state = make_state_with_votes(vec![]);
        let result = evaluate_decision_commitment(&policy, &state, &participants());
        assert!(
            matches!(result, PolicyDecision::Allow { .. }),
            "schema_version 3 must be accepted, got: {result:?}"
        );
    }

    #[test]
    fn unsupported_schema_versions_are_denied() {
        // The probe sat at 3 until spec #99 defined it; it moves to 4 rather
        // than being deleted so fail-closed behaviour for a *genuinely* unknown
        // version stays pinned.
        for v in [0u32, 4, 99] {
            let mut policy = make_policy(serde_json::json!({
                "voting": { "algorithm": "none" }
            }));
            policy.schema_version = v;
            let state = make_state_with_votes(vec![]);
            let result = evaluate_decision_commitment(&policy, &state, &participants());
            assert!(
                matches!(result, PolicyDecision::Deny { .. }),
                "schema_version {v} must be denied, got: {result:?}"
            );
        }
    }

    // ── Outcome-aware gating: negative (decline) commitments ────────
    //
    // RFC-MACP-0007 §6: Decision Mode permits both positive and negative
    // committed outcomes. `evaluate_decision_commitment_outcome` maps the vote
    // result onto the requested `outcome_positive`.

    /// Convenience: evaluate a *decline* (`outcome_positive = false`).
    fn decline(
        policy: &PolicyDefinition,
        state: &DecisionState,
        participants: &[String],
    ) -> PolicyDecision {
        evaluate_decision_commitment_outcome(policy, state, participants, false)
    }

    #[test]
    fn decline_allowed_when_majority_rejects() {
        // The motivating case: a clear reject-majority should finalize a decline.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);
        // Same vote that denies an approve (Failed) must allow a decline.
        assert!(matches!(
            evaluate_decision_commitment(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn decline_denied_when_failed_but_no_explicit_reject() {
        // `unanimous` returns Failed for a *missing voter*, not a rejection.
        // A non-vote must never authorize a finalized adverse decline.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            // compliance didn't vote → Failed, but reject_count == 0
        ]);
        let result = decline(&policy, &state, &participants());
        assert!(
            matches!(result, PolicyDecision::Deny { .. }),
            "incomplete participation must not authorize a decline, got: {result:?}"
        );
    }

    #[test]
    fn decline_allowed_when_unanimous_has_explicit_reject() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn decline_denied_over_passing_vote_by_default() {
        // Vote passed (approve majority) but a decline was requested without
        // the executive-override knob.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn decline_allowed_over_passing_vote_with_knob() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "commitment": { "allow_decline_over_approval": true }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "REJECT"),
        ]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn decline_denied_over_an_all_approve_round_with_the_knob() {
        // The non-`weighted` reach of the `Passed`-arm decline guard, which no
        // other test covers: `decline_allowed_over_passing_vote_with_knob`
        // above carries a `REJECT`, and the weighted-electorate tests all bind
        // `voting.algorithm: "weighted"`. This shape has **no** `weighted`
        // algorithm and **no** `weights` map at all — an ordinary `majority`
        // policy and three `APPROVE`s — and it is the widest reach of the
        // change: `Allow` before the guard moved into the `Passed` arm, `Deny`
        // after. `docs/deployment.md` item 6 documents it as the second
        // stored-replay predicate, and RFC-MACP-0012 §8's bounded exception
        // does **not** cover it: §8 is about weight-`0` decisiveness, and
        // there are no weights here. RFC-MACP-0007 §6.2 carried "the guard
        // applies across all three voting results" before spec #99, so this is
        // a pre-existing conformance gap being closed rather than new
        // semantics.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "commitment": { "allow_decline_over_approval": true }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "APPROVE"),
            ("p1", "agent://growth", "APPROVE"),
            ("p1", "agent://compliance", "APPROVE"),
        ]);

        // Precondition, load-bearing: the round must really be `Passed`, or
        // the test silently exercises the `Failed`/`NoVotes` arms instead of
        // the one it exists to cover.
        assert!(
            matches!(
                check_voting_algorithm(
                    "majority",
                    0.5,
                    &std::collections::HashMap::new(),
                    &state.votes,
                    &participants(),
                ),
                VotingResult::Passed(_)
            ),
            "an all-approve majority round must Pass for this test to reach the Passed arm"
        );

        let result = decline(&policy, &state, &participants());
        let PolicyDecision::Deny { reasons } = &result else {
            panic!("an all-approve round has no dissent to finalize a decline on, got: {result:?}");
        };
        let joined = reasons.join(" | ");
        // Assert the *reason*, not only the variant: the `Deny` alone would
        // also be produced by an implementation that had broken
        // `allow_decline_over_approval` outright.
        assert!(
            joined.contains("no decisive reject backs it"),
            "the denial must name the missing decisive reject, got: {joined}"
        );
        // And the wording must not send the operator to a knob this
        // descriptor does not carry.
        assert!(
            !joined.contains("voting.weights"),
            "a majority policy has no voting.weights; the reason must not cite it, got: {joined}"
        );
    }

    #[test]
    fn decline_denied_with_no_votes() {
        // NoVotes → no explicit reject → a decline is not justified.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        let state = make_state_with_votes(vec![]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn decline_allowed_with_none_algorithm() {
        // `none`: initiator-driven, outcome taken at face value (no reject-floor).
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "none" }
        }));
        let state = make_state_with_votes(vec![]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Allow { .. }
        ));
    }

    #[test]
    fn decline_denied_when_evaluation_gate_fails() {
        // The evaluation gate applies to declines too.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "evaluation": { "required_before_voting": true, "minimum_confidence": 0.8 }
        }));
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
        ]);
        // Explicit rejects exist, but no qualifying evaluation was provided.
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn decline_denied_when_quorum_required_and_unmet() {
        // The decline guard's quorum sub-condition is supplied by the existing
        // `require_vote_quorum` gate.
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 3 }
            },
            "commitment": { "require_vote_quorum": true }
        }));
        // Two explicit rejects (Failed, reject_count > 0) but quorum needs 3.
        let state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
        ]);
        assert!(matches!(
            decline(&policy, &state, &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    // ── critical_objection_action knob ──────────────────────────────

    fn veto_state() -> DecisionState {
        let mut state = make_state_with_votes(vec![
            ("p1", "agent://fraud", "REJECT"),
            ("p1", "agent://growth", "REJECT"),
        ]);
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "unacceptable risk".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        state
    }

    #[test]
    fn critical_objection_deny_blocks_decline_by_default() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        // Default action is `deny`: veto hard-stops even a decline.
        assert!(matches!(
            decline(&policy, &veto_state(), &participants()),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn critical_objection_finalize_decline_allows_negative_blocks_positive() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "objection_handling": {
                "critical_severity_vetoes": true,
                "veto_threshold": 1,
                "critical_objection_action": "finalize_decline"
            }
        }));
        // A decline finalizes under the veto...
        assert!(matches!(
            decline(&policy, &veto_state(), &participants()),
            PolicyDecision::Allow { .. }
        ));
        // ...but a positive commitment is still blocked.
        assert!(matches!(
            evaluate_decision_commitment_outcome(&policy, &veto_state(), &participants(), true),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn critical_objection_hold_denies_to_keep_session_open() {
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "objection_handling": {
                "critical_severity_vetoes": true,
                "veto_threshold": 1,
                "critical_objection_action": "hold"
            }
        }));
        let result = decline(&policy, &veto_state(), &participants());
        assert!(matches!(result, PolicyDecision::Deny { .. }));
        if let PolicyDecision::Deny { reasons } = result {
            assert!(
                reasons.iter().any(|r| r.contains("escalation")),
                "hold should surface an escalation reason, got: {reasons:?}"
            );
        }
    }

    // ── RFC-MACP-0007 §6.2 objection-authorized decline ─────────────
    //
    // A decline under `critical_objection_action: "finalize_decline"` is
    // authorized by the recorded critical `Objection`, not by the tally, so it
    // is gated by neither the voting tri-state nor the decline guard. The four
    // tests below are mutually load-bearing: each one blocks a wrong
    // implementation that would satisfy the others.

    /// `finalize_decline` over a real algorithm, with no vote ever cast.
    fn finalize_decline_policy(schema_version: u32) -> PolicyDefinition {
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "objection_handling": {
                "critical_severity_vetoes": true,
                "veto_threshold": 1,
                "critical_objection_action": "finalize_decline"
            }
        }));
        policy.schema_version = schema_version;
        policy
    }

    /// One standing critical objection, zero votes — the empty decisive tally.
    fn objection_only_state() -> DecisionState {
        let mut state = make_state_with_votes(vec![]);
        state.objections.push(Objection {
            proposal_id: "p1".into(),
            reason: "unresolved data-retention finding".into(),
            severity: "critical".into(),
            sender: "agent://compliance".into(),
        });
        state
    }

    #[test]
    fn a_critical_objection_authorizes_a_decline_on_an_empty_tally() {
        // Swept over both versions that can express `finalize_decline`: §6.2
        // says the rule "applies at every schema version that can express
        // `finalize_decline` (`schema_version >= 2`)". Before this rule the
        // decline was denied by the NoVotes negative branch — an empty tally
        // holds no decisive reject — which left a v3 session with a non-`none`
        // algorithm and a standing critical objection able to terminate only by
        // expiry, the exact stuck state `finalize_decline` exists to resolve.
        for schema_version in [2u32, 3] {
            let policy = finalize_decline_policy(schema_version);
            let result = decline(&policy, &objection_only_state(), &participants());
            let PolicyDecision::Allow { reasons } = &result else {
                panic!(
                    "a standing critical objection must authorize a decline on an empty \
                     tally (schema_version={schema_version}), got: {result:?}"
                );
            };
            let joined = reasons.join(" | ");
            assert!(
                joined.contains("critical-objection veto finalized as a decline"),
                "the allow reason must name the veto as the authorization, not the \
                 tally (schema_version={schema_version}), got: {joined}"
            );
        }
    }

    #[test]
    fn a_critical_objection_does_not_authorize_a_positive_commitment() {
        // The flag is set in the negative direction only. This is the half that
        // keeps the test above from being satisfied by an implementation that
        // stopped evaluating policy for `finalize_decline` sessions: both the
        // veto reason *and* the v3 empty-tally reason must still be reported.
        let policy = finalize_decline_policy(3);
        let result = evaluate_decision_commitment_outcome(
            &policy,
            &objection_only_state(),
            &participants(),
            true,
        );
        let PolicyDecision::Deny { reasons } = &result else {
            panic!("a veto must still block a positive commitment, got: {result:?}");
        };
        let joined = reasons.join(" | ");
        assert!(
            joined.contains("veto blocks a positive commitment"),
            "the veto denial must survive (critical_objection_action=finalize_decline), \
             got: {joined}"
        );
        assert!(
            joined.contains("no decisive votes cast"),
            "the voting block must still run for a positive commitment — the skip is \
             scoped to the decline direction, got: {joined}"
        );
    }

    #[test]
    fn a_decline_without_a_standing_objection_is_still_vote_gated() {
        // Same policy, no objection: nothing authorizes the decline, so the
        // decline guard still denies it. Without this, the first test could be
        // satisfied by unconditionally allowing declines under
        // `finalize_decline`.
        let policy = finalize_decline_policy(3);
        let result = decline(&policy, &make_state_with_votes(vec![]), &participants());
        let PolicyDecision::Deny { reasons } = &result else {
            panic!(
                "with no standing objection a decline on an empty tally stays \
                 vote-gated, got: {result:?}"
            );
        };
        assert!(
            reasons.iter().any(|r| r
                == "no votes cast; a decline requires at least one decisive explicit reject vote"),
            "the denial must come from the decline guard, got: {reasons:?}"
        );
    }

    #[test]
    fn critical_objection_action_deny_is_unaffected() {
        // §4.1 leaves `deny` (the default) and `hold` as hard-stops in both
        // directions; only `finalize_decline` authorizes a decline. Pairs with
        // the untouched `decision_critical_objection_veto.json` fixture.
        let mut policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 },
            "objection_handling": { "critical_severity_vetoes": true, "veto_threshold": 1 }
        }));
        policy.schema_version = 3;
        let result = decline(&policy, &objection_only_state(), &participants());
        let PolicyDecision::Deny { reasons } = &result else {
            panic!("the default `deny` action must still hard-stop a decline, got: {result:?}");
        };
        assert!(
            reasons.iter().any(|r| r.contains("blocked by")),
            "the default action keeps its hard-stop reason, got: {reasons:?}"
        );
    }

    // ── RFC-MACP-0012 §5.2 reserved governance profiles ─────────────
    //
    // These drive the *pre-registered canonical definitions* (not hand-rolled
    // rules) through the evaluator, so they pin the §5.2 outcome table against
    // whatever `defaults.rs` actually ships.

    use crate::defaults::{std_majority_policy, std_supermajority_policy, std_unanimous_policy};

    /// N voters, `approve` of whom approve and the rest reject, all on `p1`.
    fn split_votes(approve: usize, reject: usize) -> (DecisionState, Vec<String>) {
        let mut entries: Vec<(String, String)> = Vec::new();
        for i in 0..approve {
            entries.push((format!("agent://a{i}"), "APPROVE".to_string()));
        }
        for i in 0..reject {
            entries.push((format!("agent://r{i}"), "REJECT".to_string()));
        }
        let refs: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|(voter, vote)| ("p1", voter.as_str(), vote.as_str()))
            .collect();
        let people = entries.iter().map(|(v, _)| v.clone()).collect();
        (make_state_with_votes(refs), people)
    }

    fn approves(policy: &PolicyDefinition, state: &DecisionState, people: &[String]) -> bool {
        matches!(
            evaluate_decision_commitment_outcome(policy, state, people, true),
            PolicyDecision::Allow { .. }
        )
    }

    #[test]
    fn std_majority_approves_an_even_split() {
        // §5.2: the comparison is inclusive, so 1-of-2 approves.
        let policy = std_majority_policy();
        let (state, people) = split_votes(1, 1);
        assert!(approves(&policy, &state, &people));
    }

    #[test]
    fn std_majority_denies_a_minority() {
        let policy = std_majority_policy();
        let (state, people) = split_votes(1, 2);
        assert!(!approves(&policy, &state, &people));
    }

    #[test]
    fn std_majority_ignores_abstentions_in_the_denominator() {
        // §4.1: the denominator is the decisive votes; abstentions neither
        // help nor hinder. 1 approve / 1 reject / 3 abstain still approves.
        let policy = std_majority_policy();
        let state = make_state_with_votes(vec![
            ("p1", "agent://a0", "APPROVE"),
            ("p1", "agent://r0", "REJECT"),
            ("p1", "agent://x0", "ABSTAIN"),
            ("p1", "agent://x1", "ABSTAIN"),
            ("p1", "agent://x2", "ABSTAIN"),
        ]);
        let people: Vec<String> = ["a0", "r0", "x0", "x1", "x2"]
            .iter()
            .map(|s| format!("agent://{s}"))
            .collect();
        assert!(approves(&policy, &state, &people));
    }

    #[test]
    fn std_majority_denies_with_no_votes() {
        // require_vote_quorum = true, so an unvoted commitment is blocked.
        let policy = std_majority_policy();
        let state = make_state_with_votes(vec![]);
        assert!(!approves(&policy, &state, &participants()));
    }

    #[test]
    fn std_supermajority_matches_the_rfc_outcome_table() {
        let policy = std_supermajority_policy();
        for (approve, total) in [(2usize, 3usize), (4, 6), (20, 30), (67, 100)] {
            let (state, people) = split_votes(approve, total - approve);
            assert!(
                approves(&policy, &state, &people),
                "{approve} of {total} should approve under policy.std.supermajority"
            );
        }
        let (state, people) = split_votes(66, 34);
        assert!(
            !approves(&policy, &state, &people),
            "66 of 100 must be denied under policy.std.supermajority"
        );
    }

    #[test]
    fn std_supermajority_denies_a_single_voter() {
        // quorum is `count: 2`, enforced because require_vote_quorum is true.
        let policy = std_supermajority_policy();
        let (state, people) = split_votes(1, 0);
        assert!(!approves(&policy, &state, &people));
    }

    #[test]
    fn std_unanimous_approves_when_every_participant_approves() {
        let policy = std_unanimous_policy();
        let people = participants();
        let refs: Vec<(&str, &str, &str)> = people
            .iter()
            .map(|p| ("p1", p.as_str(), "APPROVE"))
            .collect();
        let state = make_state_with_votes(refs);
        assert!(approves(&policy, &state, &people));
    }

    #[test]
    fn std_unanimous_is_blocked_by_a_silent_declared_participant() {
        // §4.1: stricter than "all decisive votes approve" — a declared
        // participant who has not voted blocks the commitment.
        let policy = std_unanimous_policy();
        let people = participants();
        let state = make_state_with_votes(vec![
            ("p1", people[0].as_str(), "APPROVE"),
            ("p1", people[1].as_str(), "APPROVE"),
        ]);
        assert!(!approves(&policy, &state, &people));
    }

    #[test]
    fn std_unanimous_is_blocked_by_an_abstaining_participant() {
        let policy = std_unanimous_policy();
        let people = participants();
        let state = make_state_with_votes(vec![
            ("p1", people[0].as_str(), "APPROVE"),
            ("p1", people[1].as_str(), "APPROVE"),
            ("p1", people[2].as_str(), "ABSTAIN"),
        ]);
        assert!(!approves(&policy, &state, &people));
    }

    // ── §4.1 clauses the profiles depend on ─────────────────────────

    #[test]
    fn voting_quorum_is_inert_without_require_vote_quorum() {
        // §4.1: `voting.quorum` states the bar but gates nothing on its own.
        let policy = make_policy(serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 99 }
            },
            "commitment": { "require_vote_quorum": false }
        }));
        let (state, people) = split_votes(1, 0);
        assert!(
            approves(&policy, &state, &people),
            "an unmet quorum must not gate a commitment on its own"
        );
    }

    #[test]
    fn no_decisive_votes_blocks_a_positive_commitment_only_under_require_vote_quorum() {
        // §4.1 "No decisive votes".
        let permissive = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" },
            "commitment": { "require_vote_quorum": false }
        }));
        let empty = make_state_with_votes(vec![]);
        assert!(
            approves(&permissive, &empty, &participants()),
            "without require_vote_quorum an unvoted positive commitment is not blocked"
        );

        let binding = make_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" },
            "commitment": { "require_vote_quorum": true }
        }));
        assert!(!approves(&binding, &empty, &participants()));
    }

    #[test]
    fn no_decisive_votes_always_blocks_a_negative_commitment() {
        // §4.1: a decline must be backed by at least one *decisive* explicit
        // reject.
        // Version-independent: RFC-MACP-0007 §6.2's NoVotes bullet denies a
        // vote-authorized decline on an empty tally, and §4.1 says the schema
        // versions "differ only in the **positive** direction" — so the
        // schema_version axis is swept to pin that the v3 branch left the
        // negative branch alone.
        const EXPECTED: &str =
            "no votes cast; a decline requires at least one decisive explicit reject vote";
        for schema_version in [1u32, 2, 3] {
            for require_quorum in [false, true] {
                let mut policy = make_policy(serde_json::json!({
                    "voting": { "algorithm": "majority", "threshold": 0.5 },
                    "commitment": { "require_vote_quorum": require_quorum }
                }));
                policy.schema_version = schema_version;
                let empty = make_state_with_votes(vec![]);
                let result =
                    evaluate_decision_commitment_outcome(&policy, &empty, &participants(), false);
                match result {
                    PolicyDecision::Deny { reasons } => assert!(
                        reasons.iter().any(|r| r == EXPECTED),
                        "the decline denial keeps its version-independent reason \
                         (schema_version={schema_version}, require_vote_quorum={require_quorum}), \
                         got: {reasons:?}"
                    ),
                    other => panic!(
                        "a decline with no votes must be denied \
                         (schema_version={schema_version}, \
                         require_vote_quorum={require_quorum}), got: {other:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn plurality_fails_on_a_tie_and_ignores_threshold() {
        // §4.1: plurality is "more approve than reject"; a tie fails.
        let policy = make_policy(serde_json::json!({
            "voting": { "algorithm": "plurality", "threshold": 0.01 }
        }));
        let (tied, tied_people) = split_votes(1, 1);
        assert!(!approves(&policy, &tied, &tied_people));
        let (won, won_people) = split_votes(2, 1);
        assert!(approves(&policy, &won, &won_people));
    }
}
