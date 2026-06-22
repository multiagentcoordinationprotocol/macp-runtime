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
}
