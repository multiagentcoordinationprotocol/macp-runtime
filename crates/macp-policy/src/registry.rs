use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

use super::defaults::{default_policy, DEFAULT_POLICY_ID};
use macp_core::policy::rules::{
    CommitmentRules, DecisionPolicyRules, HandoffPolicyRules, ProposalPolicyRules,
    QuorumPolicyRules, TaskPolicyRules,
};
use macp_core::policy::{PolicyDefinition, PolicyError};

/// In-memory policy registry for governance policy definitions.
///
/// Mirrors the `ModeRegistry` pattern: uses `RwLock` for entries and
/// `broadcast::Sender<()>` for change notifications.
pub struct PolicyRegistry {
    entries: RwLock<HashMap<String, PolicyDefinition>>,
    change_tx: broadcast::Sender<()>,
}

impl PolicyRegistry {
    /// Create a new policy registry pre-loaded with the default policy.
    pub fn new() -> Self {
        let mut entries = HashMap::new();
        let default = default_policy();
        entries.insert(default.policy_id.clone(), default);

        let (change_tx, _) = broadcast::channel(16);
        Self {
            entries: RwLock::new(entries),
            change_tx,
        }
    }

    /// Register a new policy definition.
    ///
    /// Returns an error if:
    /// - The policy_id is empty
    /// - The policy_id is the reserved default policy
    /// - A policy with this id already exists
    /// - The schema_version is 0
    pub fn register(&self, definition: PolicyDefinition) -> Result<(), String> {
        Self::validate_definition(&definition)?;

        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&definition.policy_id) {
            return Err(format!(
                "policy '{}' is already registered",
                definition.policy_id
            ));
        }
        guard.insert(definition.policy_id.clone(), definition);
        drop(guard);
        let _ = self.change_tx.send(());
        Ok(())
    }

    /// Unregister a policy by ID.
    ///
    /// Returns an error if:
    /// - The policy is the reserved default policy
    /// - The policy does not exist
    pub fn unregister(&self, policy_id: &str) -> Result<(), String> {
        if policy_id == DEFAULT_POLICY_ID {
            return Err("cannot unregister the built-in default policy".into());
        }

        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if guard.remove(policy_id).is_none() {
            return Err(format!("policy '{}' not found", policy_id));
        }
        drop(guard);
        let _ = self.change_tx.send(());
        Ok(())
    }

    /// Resolve a policy by version string.
    ///
    /// If the version string is empty, returns the default policy.
    /// Otherwise, looks up the policy by ID.
    pub fn resolve(&self, policy_version: &str) -> Result<PolicyDefinition, PolicyError> {
        if policy_version.is_empty() {
            return self
                .get(DEFAULT_POLICY_ID)
                .ok_or_else(|| PolicyError::UnknownPolicy(DEFAULT_POLICY_ID.into()));
        }
        self.get(policy_version)
            .ok_or_else(|| PolicyError::UnknownPolicy(policy_version.into()))
    }

    /// Direct lookup by policy ID.
    pub fn get(&self, policy_id: &str) -> Option<PolicyDefinition> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard.get(policy_id).cloned()
    }

    /// List all policies, optionally filtered by target mode.
    ///
    /// If `mode_filter` is `Some(mode)`, returns only policies targeting that
    /// specific mode or the wildcard `"*"`. If `None`, returns all policies.
    pub fn list(&self, mode_filter: Option<&str>) -> Vec<PolicyDefinition> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut policies: Vec<PolicyDefinition> = guard
            .values()
            .filter(|p| match mode_filter {
                Some(mode) => p.mode == mode || p.mode == "*",
                None => true,
            })
            .cloned()
            .collect();
        policies.sort_by(|a, b| a.policy_id.cmp(&b.policy_id));
        policies
    }

    /// Subscribe to policy registry change notifications.
    /// Load policy definitions from a directory of JSON files (RFC-MACP-0012
    /// §9 / `MACP_POLICIES_DIR`). Each `*.json` file holds one
    /// `PolicyDefinition`-shaped document:
    /// `{ "policy_id", "mode", "description", "rules", "schema_version" }`.
    ///
    /// Every file goes through the same `register` path as the RPC (schema
    /// validation, reserved-id and duplicate checks). Returns the number of
    /// policies loaded; any invalid file is an error — a runtime configured
    /// to preload governance policies must not silently start without them.
    pub fn load_from_dir(&self, dir: &std::path::Path) -> Result<usize, String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read policies dir {}: {e}", dir.display()))?
            .filter_map(|r| r.ok().map(|d| d.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        // Deterministic load order regardless of filesystem enumeration.
        entries.sort();

        let mut loaded = 0;
        for path in entries {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let definition: PolicyDefinition = serde_json::from_str(&raw)
                .map_err(|e| format!("invalid policy file {}: {e}", path.display()))?;
            self.register(definition)
                .map_err(|e| format!("policy file {} rejected: {e}", path.display()))?;
            loaded += 1;
        }
        Ok(loaded)
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }

    fn validate_definition(definition: &PolicyDefinition) -> Result<(), String> {
        if definition.policy_id.trim().is_empty() {
            return Err("policy_id must not be empty".into());
        }
        if definition.policy_id == DEFAULT_POLICY_ID {
            return Err(format!(
                "cannot register with reserved policy_id '{}'",
                DEFAULT_POLICY_ID
            ));
        }
        if definition.schema_version == 0 {
            return Err("schema_version must be > 0".into());
        }
        if !definition.rules.is_object() {
            return Err("rules must be a JSON object".into());
        }
        // Validate that rules deserialize into the mode-specific schema.
        Self::validate_rules_for_mode(&definition.mode, &definition.rules)?;
        // Validate conditional constraints (RFC-MACP-0012).
        Self::validate_conditional_constraints(&definition.mode, &definition.rules)?;
        Ok(())
    }

    /// Validate that policy rules match the expected schema for the target mode.
    ///
    /// Wildcard (`"*"`) policies are validated against the Decision schema (superset).
    /// Unknown modes are allowed (extension modes may have custom rules).
    fn validate_rules_for_mode(mode: &str, rules: &serde_json::Value) -> Result<(), String> {
        let result = match mode {
            "macp.mode.decision.v1" | "*" => {
                serde_json::from_value::<DecisionPolicyRules>(rules.clone()).map(|_| ())
            }
            "macp.mode.proposal.v1" => {
                serde_json::from_value::<ProposalPolicyRules>(rules.clone()).map(|_| ())
            }
            "macp.mode.task.v1" => {
                serde_json::from_value::<TaskPolicyRules>(rules.clone()).map(|_| ())
            }
            "macp.mode.handoff.v1" => {
                serde_json::from_value::<HandoffPolicyRules>(rules.clone()).map(|_| ())
            }
            "macp.mode.quorum.v1" => {
                serde_json::from_value::<QuorumPolicyRules>(rules.clone()).map(|_| ())
            }
            _ => return Ok(()), // Extension modes: accept any valid JSON object
        };
        result.map_err(|e| format!("rules do not match schema for mode '{}': {}", mode, e))
    }

    /// Validate conditional constraints that depend on specific field values.
    fn validate_conditional_constraints(
        mode: &str,
        rules: &serde_json::Value,
    ) -> Result<(), String> {
        // Decision mode (and wildcard) voting constraints
        if matches!(mode, "macp.mode.decision.v1" | "*") {
            if let Ok(decision) = serde_json::from_value::<DecisionPolicyRules>(rules.clone()) {
                if decision.voting.algorithm == "weighted" && decision.voting.weights.is_empty() {
                    return Err(
                        "voting.algorithm 'weighted' requires non-empty voting.weights".into(),
                    );
                }
                if decision.voting.algorithm == "supermajority" && decision.voting.threshold <= 0.5
                {
                    return Err(
                        "voting.algorithm 'supermajority' requires voting.threshold > 0.5".into(),
                    );
                }
            }
        }

        // All modes: commitment.designated_roles required when authority is designated_role
        if let Some(commitment) = rules.get("commitment") {
            if let Ok(cr) = serde_json::from_value::<CommitmentRules>(commitment.clone()) {
                if cr.authority == "designated_role" && cr.designated_roles.is_empty() {
                    return Err(
                        "commitment.authority 'designated_role' requires non-empty commitment.designated_roles".into(),
                    );
                }
            }
        }

        Ok(())
    }
}

impl Default for PolicyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_policy(id: &str) -> PolicyDefinition {
        PolicyDefinition {
            policy_id: id.into(),
            mode: "macp.mode.decision.v1".into(),
            description: "test policy".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "majority", "threshold": 0.5 }
            }),
            schema_version: 1,
        }
    }

    #[test]
    fn new_registry_contains_default_policy() {
        let registry = PolicyRegistry::new();
        let default = registry.get(DEFAULT_POLICY_ID);
        assert!(default.is_some());
        assert_eq!(default.unwrap().policy_id, DEFAULT_POLICY_ID);
    }

    #[test]
    fn register_new_policy() {
        let registry = PolicyRegistry::new();
        registry
            .register(test_policy("policy.fraud.strict"))
            .unwrap();
        assert!(registry.get("policy.fraud.strict").is_some());
    }

    #[test]
    fn register_duplicate_fails() {
        let registry = PolicyRegistry::new();
        registry
            .register(test_policy("policy.fraud.strict"))
            .unwrap();
        let err = registry
            .register(test_policy("policy.fraud.strict"))
            .unwrap_err();
        assert!(err.contains("already registered"));
    }

    #[test]
    fn register_default_policy_id_fails() {
        let registry = PolicyRegistry::new();
        let err = registry
            .register(test_policy(DEFAULT_POLICY_ID))
            .unwrap_err();
        assert!(err.contains("reserved"));
    }

    #[test]
    fn register_empty_policy_id_fails() {
        let registry = PolicyRegistry::new();
        let mut policy = test_policy("valid");
        policy.policy_id = "".into();
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("must not be empty"));
    }

    #[test]
    fn register_zero_schema_version_fails() {
        let registry = PolicyRegistry::new();
        let mut policy = test_policy("policy.bad.schema");
        policy.schema_version = 0;
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("schema_version"));
    }

    #[test]
    fn register_non_object_rules_fails() {
        let registry = PolicyRegistry::new();
        let mut policy = test_policy("policy.bad.rules");
        policy.rules = serde_json::json!("not an object");
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("JSON object"));
    }

    #[test]
    fn unregister_policy() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.temp")).unwrap();
        assert!(registry.get("policy.temp").is_some());
        registry.unregister("policy.temp").unwrap();
        assert!(registry.get("policy.temp").is_none());
    }

    #[test]
    fn unregister_default_fails() {
        let registry = PolicyRegistry::new();
        let err = registry.unregister(DEFAULT_POLICY_ID).unwrap_err();
        assert!(err.contains("default policy"));
    }

    #[test]
    fn unregister_nonexistent_fails() {
        let registry = PolicyRegistry::new();
        let err = registry.unregister("nonexistent").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn resolve_empty_returns_default() {
        let registry = PolicyRegistry::new();
        let policy = registry.resolve("").unwrap();
        assert_eq!(policy.policy_id, DEFAULT_POLICY_ID);
    }

    #[test]
    fn resolve_specific_policy() {
        let registry = PolicyRegistry::new();
        registry
            .register(test_policy("policy.fraud.strict"))
            .unwrap();
        let policy = registry.resolve("policy.fraud.strict").unwrap();
        assert_eq!(policy.policy_id, "policy.fraud.strict");
    }

    #[test]
    fn resolve_unknown_returns_error() {
        let registry = PolicyRegistry::new();
        let err = registry.resolve("nonexistent").unwrap_err();
        assert!(matches!(err, PolicyError::UnknownPolicy(_)));
    }

    #[test]
    fn list_all_policies() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.a")).unwrap();
        registry.register(test_policy("policy.b")).unwrap();
        let all = registry.list(None);
        assert_eq!(all.len(), 3); // default + a + b
    }

    #[test]
    fn list_filtered_by_mode() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.decision")).unwrap();
        let mut task_policy = test_policy("policy.task");
        task_policy.mode = "macp.mode.task.v1".into();
        registry.register(task_policy).unwrap();

        let decision_policies = registry.list(Some("macp.mode.decision.v1"));
        // Should include: default (mode="*") + policy.decision (mode matches)
        assert_eq!(decision_policies.len(), 2);

        let task_policies = registry.list(Some("macp.mode.task.v1"));
        // Should include: default (mode="*") + policy.task (mode matches)
        assert_eq!(task_policies.len(), 2);
    }

    #[test]
    fn list_returns_sorted_by_id() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.z")).unwrap();
        registry.register(test_policy("policy.a")).unwrap();
        let all = registry.list(None);
        let ids: Vec<&str> = all.iter().map(|p| p.policy_id.as_str()).collect();
        assert_eq!(ids, vec!["policy.a", "policy.default", "policy.z"]);
    }

    #[test]
    fn subscribe_notifies_on_register() {
        let registry = PolicyRegistry::new();
        let mut rx = registry.subscribe_changes();
        registry.register(test_policy("policy.test")).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn subscribe_notifies_on_unregister() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.test")).unwrap();
        let mut rx = registry.subscribe_changes();
        registry.unregister("policy.test").unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn register_valid_decision_rules_succeeds() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.decision.strict".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "strict decision".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "unanimous" },
                "commitment": { "require_vote_quorum": true }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_valid_proposal_rules_succeeds() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.proposal.limited".into(),
            mode: "macp.mode.proposal.v1".into(),
            description: "limited proposals".into(),
            rules: serde_json::json!({
                "acceptance": { "criterion": "all_parties" },
                "counter_proposal": { "max_rounds": 3 },
                "rejection": { "terminal_on_any_reject": false }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_valid_task_rules_succeeds() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.task.strict".into(),
            mode: "macp.mode.task.v1".into(),
            description: "strict task".into(),
            rules: serde_json::json!({
                "assignment": { "allow_reassignment_on_reject": false },
                "completion": { "require_output": true }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_valid_handoff_rules_succeeds() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.handoff.strict".into(),
            mode: "macp.mode.handoff.v1".into(),
            description: "strict handoff".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": 5000 },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_valid_quorum_rules_succeeds() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.quorum.strict".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "strict quorum".into(),
            rules: serde_json::json!({
                "threshold": { "type": "percentage", "value": 75.0 },
                "abstention": { "counts_toward_quorum": false, "interpretation": "neutral" }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_extension_mode_accepts_any_rules() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "policy.custom.ext".into(),
            mode: "ext.custom.v1".into(),
            description: "custom extension".into(),
            rules: serde_json::json!({
                "arbitrary_field": "any_value",
                "nested": { "deep": true }
            }),
            schema_version: 1,
        };
        registry.register(policy).unwrap();
    }

    #[test]
    fn register_weighted_without_weights_fails() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "test-weighted".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "weighted without weights".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "weighted" }
            }),
            schema_version: 1,
        };
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("weighted"), "error: {err}");
    }

    #[test]
    fn register_supermajority_low_threshold_fails() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "test-super".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "supermajority with low threshold".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "supermajority", "threshold": 0.4 }
            }),
            schema_version: 1,
        };
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("supermajority"), "error: {err}");
    }

    #[test]
    fn register_designated_role_without_roles_fails() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "test-designated".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "designated_role without roles".into(),
            rules: serde_json::json!({
                "commitment": { "authority": "designated_role" }
            }),
            schema_version: 1,
        };
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("designated_role"), "error: {err}");
    }
}
