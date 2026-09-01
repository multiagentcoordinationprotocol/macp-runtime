use macp_core::policy::PolicyDefinition;

/// The default policy that ships with every runtime.
///
/// Mode built-in rules apply with no additional governance constraints.
/// This policy uses `"*"` as the mode target, meaning it applies to all modes.
pub fn default_policy() -> PolicyDefinition {
    PolicyDefinition {
        policy_id: "policy.default".to_string(),
        mode: "*".to_string(),
        description: "Default policy \u{2014} mode built-in rules apply with no additional governance constraints".to_string(),
        rules: serde_json::json!({
            "voting": { "algorithm": "none", "quorum": { "type": "count", "value": 0 } },
            "objection_handling": { "critical_severity_vetoes": false, "veto_threshold": 1 },
            "evaluation": { "required_before_voting": false, "minimum_confidence": 0.0 },
            "commitment": { "authority": "initiator_only", "designated_roles": [], "require_vote_quorum": false }
        }),
        schema_version: 1,
    }
}

/// The policy ID reserved for the built-in default policy.
pub const DEFAULT_POLICY_ID: &str = "policy.default";

/// The reserved namespace prefix for the built-in governance profiles
/// published in RFC-MACP-0012 §5.2.
///
/// Every identifier under this prefix is reserved: it MUST NOT be registered
/// (via `RegisterPolicy` or `MACP_POLICIES_DIR`) unless the descriptor is the
/// canonical definition for that identifier, and an identifier the RFC has not
/// assigned MUST NOT resolve. See RFC-MACP-0012 §2.2.
pub const STD_POLICY_PREFIX: &str = "policy.std.";

/// RFC-MACP-0012 §5.2: simple majority.
pub const STD_MAJORITY_POLICY_ID: &str = "policy.std.majority";
/// RFC-MACP-0012 §5.2: two-thirds supermajority, minimum two voters.
pub const STD_SUPERMAJORITY_POLICY_ID: &str = "policy.std.supermajority";
/// RFC-MACP-0012 §5.2: every declared participant approves.
pub const STD_UNANIMOUS_POLICY_ID: &str = "policy.std.unanimous";

/// `policy.std.majority` — at least half of the decisive votes approve.
///
/// The comparison in the evaluator is inclusive (`ratio >= threshold`), so an
/// even split approves. A profile in which a tie fails is `plurality`, which
/// RFC-MACP-0012 does not reserve.
pub fn std_majority_policy() -> PolicyDefinition {
    PolicyDefinition {
        policy_id: STD_MAJORITY_POLICY_ID.to_string(),
        mode: "macp.mode.decision.v1".to_string(),
        description: "Simple majority \u{2014} at least half of the decisive votes approve"
            .to_string(),
        rules: serde_json::json!({
            "voting": {
                "algorithm": "majority",
                "threshold": 0.5,
                "quorum": { "type": "count", "value": 1 }
            },
            "commitment": { "require_vote_quorum": true }
        }),
        schema_version: 1,
    }
}

/// `policy.std.supermajority` — at least two-thirds of the decisive votes
/// approve, with at least two voters.
///
/// `0.6666666666666666` is the IEEE-754 binary64 value nearest two-thirds and
/// is exactly what `2.0 / 3.0` produces in binary64. RFC-MACP-0012 §5.2
/// requires the comparison be evaluated in binary64 so that 2-of-3, 4-of-6,
/// 20-of-30 and 67-of-100 pass while 66-of-100 does not; Rust `f64` is
/// binary64, so the evaluator satisfies this without special handling.
pub fn std_supermajority_policy() -> PolicyDefinition {
    PolicyDefinition {
        policy_id: STD_SUPERMAJORITY_POLICY_ID.to_string(),
        mode: "macp.mode.decision.v1".to_string(),
        description: "Two-thirds supermajority with a minimum of two voters".to_string(),
        rules: serde_json::json!({
            "voting": {
                "algorithm": "supermajority",
                "threshold": 0.666_666_666_666_666_6,
                "quorum": { "type": "count", "value": 2 }
            },
            "commitment": { "require_vote_quorum": true }
        }),
        schema_version: 1,
    }
}

/// `policy.std.unanimous` — every declared participant has cast an approve
/// vote and no reject exists.
///
/// Stricter than "all decisive votes approve": a declared participant who has
/// not voted blocks the commitment, and `threshold` is not consulted. The
/// `quorum` entry only keeps the algorithm binding.
pub fn std_unanimous_policy() -> PolicyDefinition {
    PolicyDefinition {
        policy_id: STD_UNANIMOUS_POLICY_ID.to_string(),
        mode: "macp.mode.decision.v1".to_string(),
        description: "Unanimous \u{2014} every declared participant approves and no reject is cast"
            .to_string(),
        rules: serde_json::json!({
            "voting": {
                "algorithm": "unanimous",
                "quorum": { "type": "count", "value": 1 }
            },
            "commitment": { "require_vote_quorum": true }
        }),
        schema_version: 1,
    }
}

/// The canonical `policy.std.` profiles this runtime pre-registers.
///
/// RFC-MACP-0012 §5.2 makes provisioning optional — a runtime may pre-register
/// any subset, including none. This is the reference runtime, so it provides
/// all three.
pub fn std_policies() -> Vec<PolicyDefinition> {
    vec![
        std_majority_policy(),
        std_supermajority_policy(),
        std_unanimous_policy(),
    ]
}

/// The canonical definition for a reserved `policy.std.` identifier, or `None`
/// if the identifier is under the reserved prefix but unassigned by the RFC.
pub fn canonical_std_policy(policy_id: &str) -> Option<PolicyDefinition> {
    match policy_id {
        STD_MAJORITY_POLICY_ID => Some(std_majority_policy()),
        STD_SUPERMAJORITY_POLICY_ID => Some(std_supermajority_policy()),
        STD_UNANIMOUS_POLICY_ID => Some(std_unanimous_policy()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_has_correct_id() {
        let policy = default_policy();
        assert_eq!(policy.policy_id, DEFAULT_POLICY_ID);
    }

    #[test]
    fn default_policy_applies_to_all_modes() {
        let policy = default_policy();
        assert_eq!(policy.mode, "*");
    }

    #[test]
    fn default_policy_rules_are_valid_json() {
        let policy = default_policy();
        assert!(policy.rules.is_object());
        assert!(policy.rules.get("voting").is_some());
        assert!(policy.rules.get("objection_handling").is_some());
        assert!(policy.rules.get("evaluation").is_some());
        assert!(policy.rules.get("commitment").is_some());
    }

    #[test]
    fn default_policy_schema_version_is_one() {
        let policy = default_policy();
        assert_eq!(policy.schema_version, 1);
    }

    #[test]
    fn default_policy_voting_algorithm_is_none() {
        let policy = default_policy();
        let voting = policy.rules.get("voting").unwrap();
        assert_eq!(voting.get("algorithm").unwrap().as_str().unwrap(), "none");
    }

    // ── RFC-MACP-0012 §5.2 reserved governance profiles ─────────────

    #[test]
    fn std_policies_all_live_under_the_reserved_prefix() {
        for policy in std_policies() {
            assert!(
                policy.policy_id.starts_with(STD_POLICY_PREFIX),
                "{} is not under {STD_POLICY_PREFIX}",
                policy.policy_id
            );
        }
    }

    #[test]
    fn std_policies_target_decision_mode_at_schema_version_one() {
        for policy in std_policies() {
            assert_eq!(policy.mode, "macp.mode.decision.v1", "{}", policy.policy_id);
            assert_eq!(policy.schema_version, 1, "{}", policy.policy_id);
        }
    }

    #[test]
    fn std_policies_all_require_vote_quorum() {
        // §5.2: without this the voting algorithm is not binding on a positive
        // commitment when no vote has been cast, making each profile vacuous.
        for policy in std_policies() {
            let require = policy
                .rules
                .get("commitment")
                .and_then(|c| c.get("require_vote_quorum"))
                .and_then(|v| v.as_bool());
            assert_eq!(require, Some(true), "{}", policy.policy_id);
        }
    }

    #[test]
    fn std_majority_matches_the_canonical_json() {
        let policy = std_majority_policy();
        assert_eq!(policy.policy_id, "policy.std.majority");
        let voting = policy.rules.get("voting").unwrap();
        assert_eq!(voting["algorithm"], "majority");
        assert_eq!(voting["threshold"].as_f64(), Some(0.5));
        assert_eq!(voting["quorum"]["type"], "count");
        assert_eq!(voting["quorum"]["value"].as_f64(), Some(1.0));
    }

    #[test]
    fn std_supermajority_matches_the_canonical_json() {
        let policy = std_supermajority_policy();
        assert_eq!(policy.policy_id, "policy.std.supermajority");
        let voting = policy.rules.get("voting").unwrap();
        assert_eq!(voting["algorithm"], "supermajority");
        assert_eq!(voting["quorum"]["type"], "count");
        assert_eq!(voting["quorum"]["value"].as_f64(), Some(2.0));
        // §5.2 determinism note: the literal is the binary64 value nearest
        // two-thirds, which is what binary64 `2 / 3` produces.
        let threshold = voting["threshold"].as_f64().unwrap();
        assert_eq!(threshold, 2.0f64 / 3.0f64);
        assert_eq!(format!("{threshold:?}"), "0.6666666666666666");
    }

    #[test]
    fn std_unanimous_matches_the_canonical_json() {
        let policy = std_unanimous_policy();
        assert_eq!(policy.policy_id, "policy.std.unanimous");
        let voting = policy.rules.get("voting").unwrap();
        assert_eq!(voting["algorithm"], "unanimous");
        assert_eq!(voting["quorum"]["type"], "count");
        assert_eq!(voting["quorum"]["value"].as_f64(), Some(1.0));
        // §5.2: `threshold` is not consulted by the unanimous algorithm, and
        // the canonical descriptor does not spell it out.
        assert!(voting.get("threshold").is_none());
    }

    #[test]
    fn supermajority_threshold_admits_the_rfc_outcome_table_in_binary64() {
        // §5.2 determinism note: 2/3, 4/6, 20/30 and 67/100 pass; 66/100 fails.
        let threshold = std_supermajority_policy().rules["voting"]["threshold"]
            .as_f64()
            .unwrap();
        for (approve, decisive) in [(2, 3), (4, 6), (20, 30), (67, 100)] {
            let ratio = approve as f64 / decisive as f64;
            assert!(
                ratio >= threshold,
                "{approve} of {decisive} should meet the two-thirds bar"
            );
        }
        let ratio = 66.0f64 / 100.0f64;
        assert!(ratio < threshold, "66 of 100 must not meet the bar");
    }

    #[test]
    fn canonical_std_policy_resolves_assigned_ids_only() {
        assert!(canonical_std_policy(STD_MAJORITY_POLICY_ID).is_some());
        assert!(canonical_std_policy(STD_SUPERMAJORITY_POLICY_ID).is_some());
        assert!(canonical_std_policy(STD_UNANIMOUS_POLICY_ID).is_some());
        assert!(canonical_std_policy("policy.std.nonesuch").is_none());
        assert!(canonical_std_policy("policy.majority").is_none());
    }
}
