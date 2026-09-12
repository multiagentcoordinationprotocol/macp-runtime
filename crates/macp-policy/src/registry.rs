use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

use super::defaults::{
    canonical_std_policy, default_policy, std_policies, DEFAULT_POLICY_ID, STD_POLICY_PREFIX,
};
use macp_core::policy::rules::{
    CommitmentRules, DecisionPolicyRules, HandoffPolicyRules, ProposalPolicyRules,
    QuorumPolicyRules, TaskPolicyRules,
};
use macp_core::policy::{PolicyDefinition, PolicyError};

/// Legal values of `voting.algorithm`, mirroring
/// `schemas/json/policy/decision-rules.schema.json`
/// (`properties.voting.properties.algorithm.enum`).
const DECISION_VOTING_ALGORITHMS: [&str; 6] = [
    "none",
    "majority",
    "supermajority",
    "unanimous",
    "weighted",
    "plurality",
];

/// Legal values of `voting.quorum.type`, mirroring
/// `schemas/json/policy/decision-rules.schema.json`
/// (`properties.voting.properties.quorum.properties.type.enum`).
const DECISION_VOTING_QUORUM_TYPES: [&str; 2] = ["count", "percentage"];

/// Legal values of Quorum mode's `threshold.type`.
///
/// `schemas/json/policy/quorum-rules.schema.json`
/// (`properties.threshold.properties.type.enum`) is the closed pair `n_of_m`,
/// `percentage` as of RFC-MACP-0012 1.2.0-draft (spec #110). This list departs
/// from it **once**: `count` is a documented alias for `n_of_m` in this runtime
/// (`docs/policy.md`), which both the mode and the evaluator already treat as
/// one.
///
/// `weighted` used to be the second departure — the canonical enum listed it
/// and this runtime refused it as unimplemented. Spec #110 removed it from the
/// vocabulary outright (no weights map, no electorate rule, no weighted
/// analogue of RFC-MACP-0011 §5's count-only termination arithmetic, so no
/// conformant evaluation of it ever existed) and **reserved** the identifier,
/// which MUST NOT be reused with a different meaning. Our refusal is therefore
/// no longer a departure but agreement: `weighted` is now refused by this
/// list's ordinary enum check, like any other unknown type.
const QUORUM_THRESHOLD_TYPES: [&str; 3] = ["n_of_m", "percentage", "count"];

/// Every standards-track mode a policy may target, which is exactly the set a
/// `mode: "*"` policy targets: `Runtime::handle_session_start` binds a wildcard
/// to any mode's session. Registration therefore holds a wildcard to all of
/// their schemas and all of their conditional constraints, not just Decision's.
const STANDARDS_TRACK_POLICY_MODES: [&str; 5] = [
    "macp.mode.decision.v1",
    "macp.mode.proposal.v1",
    "macp.mode.task.v1",
    "macp.mode.handoff.v1",
    "macp.mode.quorum.v1",
];

/// The outcome of validating one file during a [`PolicyRegistry::validate_dir`]
/// pass.
///
/// `#[non_exhaustive]` because this type is only ever *produced* by the
/// registry and never constructed by callers, so reserving the right to add a
/// field costs external users nothing — while adding one later to an
/// exhaustive struct would be a breaking change.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PolicyFileOutcome {
    /// The file that was validated.
    pub path: std::path::PathBuf,
    /// `Ok(policy_id)` if the file would load, `Err(reason)` if it would be
    /// rejected. The reason is the same text `register` would return.
    pub result: Result<String, String>,
}

/// In-memory policy registry for governance policy definitions.
///
/// Mirrors the `ModeRegistry` pattern: uses `RwLock` for entries and
/// `broadcast::Sender<()>` for change notifications.
pub struct PolicyRegistry {
    entries: RwLock<HashMap<String, PolicyDefinition>>,
    change_tx: broadcast::Sender<()>,
}

impl PolicyRegistry {
    /// Create a new policy registry pre-loaded with the built-in policies:
    /// the required `policy.default` (RFC-MACP-0012 §5.1) and the three
    /// reserved governance profiles of §5.2. Pre-registration bypasses
    /// [`Self::register`] deliberately — the reserved-namespace guard there
    /// exists to keep callers out of `policy.std.`, not the runtime itself.
    pub fn new() -> Self {
        let mut entries = HashMap::new();
        let default = default_policy();
        entries.insert(default.policy_id.clone(), default);
        for profile in std_policies() {
            entries.insert(profile.policy_id.clone(), profile);
        }

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
    /// - The policy_id is in the reserved `policy.std.` namespace and the
    ///   descriptor is not the canonical RFC-MACP-0012 §5.2 definition
    /// - A policy with this id already exists
    /// - The schema_version is 0
    pub fn register(&self, definition: PolicyDefinition) -> Result<(), String> {
        Self::validate_definition(&definition)?;

        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&definition.policy_id) {
            // Deliberately *not* prefixed with `INVALID_POLICY_DEFINITION`.
            // A taken id is a conflict, not a malformed definition — the
            // descriptor offered here may be perfectly valid, and it already
            // cleared `validate_definition` above. RFC-MACP-0012 spends that
            // code on the definition itself being wrong; stretching it over a
            // namespace collision would make one code mean two things, and
            // consumers that branch on the prefix (the control plane maps it
            // to HTTP 400) would report a 409-shaped failure as a 400.
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
    /// - The policy is a pre-registered `policy.std.` profile
    /// - The policy does not exist
    pub fn unregister(&self, policy_id: &str) -> Result<(), String> {
        if policy_id == DEFAULT_POLICY_ID {
            return Err("cannot unregister the built-in default policy".into());
        }

        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        // RFC-MACP-0012 §7: a pre-registered `policy.std.` policy MUST NOT be
        // unregistered. Anything present under the prefix was pre-registered —
        // `register` refuses every other route into the namespace — so mere
        // presence is the test. An absent one still reports "not found".
        if policy_id.starts_with(STD_POLICY_PREFIX) && guard.contains_key(policy_id) {
            return Err(format!(
                "cannot unregister the built-in reserved policy '{}' (RFC-MACP-0012 §2.2)",
                policy_id
            ));
        }
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

    /// Load policy definitions from a directory of JSON files (RFC-MACP-0012
    /// §9 / `MACP_POLICIES_DIR`). Each `*.json` file holds one
    /// `PolicyDefinition`-shaped document:
    /// `{ "policy_id", "mode", "description", "rules", "schema_version" }`.
    ///
    /// Every file goes through the same `register` path as the RPC (schema
    /// validation, reserved-id and duplicate checks). Returns the number of
    /// policies loaded; any invalid file is an error — a runtime configured
    /// to preload governance policies must not silently start without them.
    /// It stops at the **first** rejection, which is what makes
    /// [`Self::validate_dir`] worth having: an operator upgrading into new
    /// registration constraints needs the whole list, not just the first file
    /// that trips one.
    pub fn load_from_dir(&self, dir: &std::path::Path) -> Result<usize, String> {
        let entries = Self::policy_files_in(dir)?;

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

    /// Validate every policy file in `dir` **without** loading it into this
    /// registry, reporting one outcome per file rather than stopping at the
    /// first rejection. Backs `MACP_POLICIES_DRY_RUN=1`.
    ///
    /// Validation runs against a scratch registry, so it reproduces the
    /// startup path exactly — including duplicate-id detection between two
    /// files in the same directory and the pre-registered `policy.default` /
    /// `policy.std.*` entries — while leaving `self` untouched. Only a
    /// directory that cannot be read at all is an `Err`; every per-file
    /// problem is an `Err` inside a [`PolicyFileOutcome`].
    pub fn validate_dir(dir: &std::path::Path) -> Result<Vec<PolicyFileOutcome>, String> {
        let scratch = Self::new();
        let mut outcomes = Vec::new();
        for path in Self::policy_files_in(dir)? {
            let result = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read file: {e}"))
                .and_then(|raw| {
                    serde_json::from_str::<PolicyDefinition>(&raw)
                        .map_err(|e| format!("not a valid policy document: {e}"))
                })
                .and_then(|definition| {
                    let policy_id = definition.policy_id.clone();
                    scratch.register(definition).map(|()| policy_id)
                });
            outcomes.push(PolicyFileOutcome { path, result });
        }
        Ok(outcomes)
    }

    /// The `*.json` files in `dir`, in a deterministic order regardless of
    /// filesystem enumeration order.
    fn policy_files_in(dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| format!("cannot read policies dir {}: {e}", dir.display()))?
            .filter_map(|r| r.ok().map(|d| d.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        entries.sort();
        Ok(entries)
    }

    /// Subscribe to policy registry change notifications.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }

    /// Every rejection returned from here (and from the helpers it calls) leads
    /// with `INVALID_POLICY_DEFINITION`, because `RegisterPolicyResponse`
    /// carries no structured error code — only `ok` and a message — so the code
    /// RFC-MACP-0012 mandates has to travel in the text. Downstream consumers
    /// branch on that prefix (the control plane maps it to HTTP 400), so it is
    /// wire-visible contract, not decoration.
    fn validate_definition(definition: &PolicyDefinition) -> Result<(), String> {
        if definition.policy_id.trim().is_empty() {
            return Err("INVALID_POLICY_DEFINITION: policy_id must not be empty".into());
        }
        if definition.policy_id == DEFAULT_POLICY_ID {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: cannot register with reserved policy_id '{}'",
                DEFAULT_POLICY_ID
            ));
        }
        if definition.schema_version == 0 {
            return Err("INVALID_POLICY_DEFINITION: schema_version must be > 0".into());
        }
        if !definition.rules.is_object() {
            return Err("INVALID_POLICY_DEFINITION: rules must be a JSON object".into());
        }
        Self::validate_reserved_namespace(definition)?;
        // Validate that rules deserialize into the mode-specific schema.
        Self::validate_rules_for_mode(&definition.mode, &definition.rules)?;
        // Validate conditional constraints (RFC-MACP-0012).
        Self::validate_conditional_constraints(&definition.mode, &definition.rules)?;
        Ok(())
    }

    /// Enforce the reserved `policy.std.` namespace (RFC-MACP-0012 §2.2).
    ///
    /// A `policy_id` under the prefix may only be registered when the
    /// descriptor *is* the canonical §5.2 definition for that identifier;
    /// unassigned identifiers under the prefix are refused outright. This runs
    /// on every caller-supplied registration, so it covers both
    /// `RegisterPolicy` and the `MACP_POLICIES_DIR` loading path (which funnels
    /// through [`Self::register`]).
    ///
    /// The rejection message leads with `INVALID_POLICY_DEFINITION` because
    /// `RegisterPolicyResponse` carries no structured error code — only `ok`
    /// and a message — so the code the RFC mandates has to travel in the text.
    fn validate_reserved_namespace(definition: &PolicyDefinition) -> Result<(), String> {
        if !definition.policy_id.starts_with(STD_POLICY_PREFIX) {
            return Ok(());
        }
        let Some(canonical) = canonical_std_policy(&definition.policy_id) else {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: policy_id '{}' is in the reserved '{}' namespace \
                 but is not a governance profile assigned by RFC-MACP-0012 §5.2",
                definition.policy_id, STD_POLICY_PREFIX
            ));
        };
        if !Self::resolves_to_same_rules(definition, &canonical) {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: policy_id '{}' is reserved by RFC-MACP-0012 §2.2; \
                 a registration under it must be the canonical §5.2 definition verbatim",
                definition.policy_id
            ));
        }
        Ok(())
    }

    /// Semantic equality for a reserved-namespace descriptor: `mode` and
    /// `schema_version` must match, and every rule parameter — whether spelled
    /// out or left to its schema default — must resolve to the canonical value.
    /// Comparing the *parsed* rules (not the raw JSON) is what makes the
    /// "spelled out or defaulted" clause of §2.2 hold.
    fn resolves_to_same_rules(candidate: &PolicyDefinition, canonical: &PolicyDefinition) -> bool {
        if candidate.mode != canonical.mode || candidate.schema_version != canonical.schema_version
        {
            return false;
        }
        let resolve = |rules: &serde_json::Value| {
            serde_json::from_value::<DecisionPolicyRules>(rules.clone())
                .ok()
                .and_then(|parsed| serde_json::to_value(parsed).ok())
        };
        match (resolve(&candidate.rules), resolve(&canonical.rules)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Validate that policy rules match the expected schema for the target mode.
    ///
    /// A wildcard (`"*"`) policy is validated against **every** standards-track
    /// mode's schema, because `Runtime::handle_session_start` binds it to every
    /// mode's sessions (`policy.mode != "*" && policy.mode != mode_name` is the
    /// whole mismatch test) and each mode's evaluator then re-parses these same
    /// rules through its own struct. Validating it against Decision alone —
    /// which this did, calling Decision a "superset" it is not — let a quorum
    /// `threshold` through with no check of any kind: `DecisionPolicyRules` has
    /// no such field and no `deny_unknown_fields`, so the object was silently
    /// dropped here, `validate_conditional_constraints` skipped its quorum
    /// block on the exact-mode test, and `QuorumMode::effective_threshold` then
    /// read a value nothing had validated.
    ///
    /// Unknown modes are allowed (extension modes may have custom rules).
    fn validate_rules_for_mode(mode: &str, rules: &serde_json::Value) -> Result<(), String> {
        let result = match mode {
            "*" => {
                return STANDARDS_TRACK_POLICY_MODES
                    .iter()
                    .try_for_each(|m| Self::validate_rules_for_mode(m, rules));
            }
            "macp.mode.decision.v1" => {
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
        result.map_err(|e| {
            format!(
                "INVALID_POLICY_DEFINITION: rules do not match schema for mode '{}': {}",
                mode, e
            )
        })
    }

    /// Validate conditional constraints that depend on specific field values.
    ///
    /// Two families of check live here:
    ///
    /// 1. **Value-domain checks that mirror the canonical JSON Schemas**
    ///    (`schemas/json/policy/*.schema.json` in the spec repo) — enum
    ///    membership, numeric ranges and numeric types. This runtime carries no
    ///    JSON-Schema evaluator, so these are hand-written from the schema text
    ///    and each helper cites the file and keyword it mirrors. Without them
    ///    an out-of-domain value reaches the evaluator, where it either falls
    ///    into a fail-open `_` arm or silently changes the governance bar.
    /// 2. **Conditional (`allOf`/`if`-`then`) constraints** the schemas express
    ///    structurally.
    ///
    /// Every rejection leads with `INVALID_POLICY_DEFINITION` because
    /// `RegisterPolicyResponse` carries no structured error code — only `ok`
    /// and a message — so the code has to travel in the text. The same applies
    /// to the `MACP_POLICIES_DIR` path, which funnels through
    /// [`Self::register`].
    fn validate_conditional_constraints(
        mode: &str,
        rules: &serde_json::Value,
    ) -> Result<(), String> {
        // Decision mode (and wildcard) voting constraints
        if matches!(mode, "macp.mode.decision.v1" | "*") {
            if let Ok(decision) = serde_json::from_value::<DecisionPolicyRules>(rules.clone()) {
                Self::validate_decision_voting(&decision.voting)?;
                if decision.voting.algorithm == "weighted" && decision.voting.weights.is_empty() {
                    return Err(
                        "INVALID_POLICY_DEFINITION: voting.algorithm 'weighted' requires non-empty voting.weights".into(),
                    );
                }
                // `weights.minProperties: 1` is unconditional on the algorithm
                // in decision-rules.schema.json, so a *supplied* empty map is
                // refused whatever the algorithm. Discriminate on the raw JSON:
                // `VotingRules.weights` is a `HashMap` that defaults to empty,
                // so a parsed-struct test could not tell "supplied `{}`" from
                // "omitted entirely" and would refuse every non-weighted
                // policy. This check belongs inside the Decision mode guard —
                // hoisted out, it would police a `voting.weights` map in rules
                // registered for a mode whose schema does not govern it.
                if rules
                    .get("voting")
                    .and_then(|v| v.get("weights"))
                    .and_then(|w| w.as_object())
                    .is_some_and(|w| w.is_empty())
                {
                    return Err(
                        "INVALID_POLICY_DEFINITION: voting.weights must be non-empty when supplied"
                            .into(),
                    );
                }
                if decision.voting.algorithm == "supermajority" && decision.voting.threshold <= 0.5
                {
                    return Err(
                        "INVALID_POLICY_DEFINITION: voting.algorithm 'supermajority' requires voting.threshold > 0.5".into(),
                    );
                }
                // Inclusive at 0.5, deliberately asymmetric with the
                // supermajority arm above: `policy.std.majority` sets exactly
                // `0.5` and RFC-MACP-0012 §2.2 pins that profile byte-identical
                // on every runtime, so an exclusive bound here would refuse a
                // profile this runtime pre-registers at startup.
                if decision.voting.algorithm == "majority" && decision.voting.threshold < 0.5 {
                    return Err(
                        "INVALID_POLICY_DEFINITION: voting.algorithm 'majority' requires voting.threshold >= 0.5".into(),
                    );
                }
            }
        }

        // Quorum mode threshold constraints — and the wildcard, which binds to
        // quorum sessions too. The exact-mode test this used to carry was a
        // hole straight through every quorum check below: a `mode: "*"` policy
        // reached `QuorumMode::effective_threshold` carrying a `threshold` that
        // no layer had validated.
        if matches!(mode, "macp.mode.quorum.v1" | "*") {
            if let Ok(quorum) = serde_json::from_value::<QuorumPolicyRules>(rules.clone()) {
                Self::validate_quorum_threshold(&quorum.threshold)?;
                // `threshold.value`'s `exclusiveMinimum: 0` (spec #110, the
                // quorum-side twin of the Decision floor #99 tightened). A zero
                // approval bar is trivially satisfied, so a restrictive-looking
                // quorum policy approved everything — fail-open, the worst
                // polarity to leave authorable.
                //
                // Discriminate on the raw JSON, for the same reason
                // `voting.weights` does above: `QuorumThreshold::value`
                // defaults to `0.0`, so a parsed-struct test could not tell a
                // *supplied* `0` from an omitted `threshold` (or a
                // `threshold: {}` carrying no `value`) and would refuse every
                // quorum policy that declines to set a bar — including the
                // built-in `policy.default` wildcard. JSON Schema applies the
                // keyword only where the key is present, and the canonical
                // `threshold` object sets no `required`, so presence is exactly
                // the right discriminator. An omitted value still resolves to
                // `EffectiveThreshold::Inert`, where the ApprovalRequest's own
                // `required_approvals` stands.
                if rules
                    .get("threshold")
                    .and_then(|t| t.get("value"))
                    .and_then(|v| v.as_f64())
                    .is_some_and(|v| v.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
                {
                    return Err(format!(
                        "INVALID_POLICY_DEFINITION: threshold.value {} is out of range: \
                         must be greater than 0 (RFC-MACP-0011 §5 rule 2)",
                        quorum.threshold.value
                    ));
                }
            }
        }

        // All modes: commitment.designated_roles required when authority is designated_role
        if let Some(commitment) = rules.get("commitment") {
            if let Ok(cr) = serde_json::from_value::<CommitmentRules>(commitment.clone()) {
                if cr.authority == "designated_role" && cr.designated_roles.is_empty() {
                    return Err(
                        "INVALID_POLICY_DEFINITION: commitment.authority 'designated_role' requires non-empty commitment.designated_roles".into(),
                    );
                }
            }
        }

        Ok(())
    }

    /// Decision-mode `voting` value domains, mirroring
    /// `schemas/json/policy/decision-rules.schema.json`:
    ///
    /// - `properties.voting.properties.algorithm.enum` →
    ///   [`DECISION_VOTING_ALGORITHMS`]. An unknown algorithm reaches
    ///   `check_voting_algorithm`'s fail-closed `_` arm, so every commitment in
    ///   such a session is denied with no way to tell a typo from a policy.
    /// - `properties.voting.properties.threshold.{exclusiveMinimum,maximum}` —
    ///   `0` **exclusive** and `1` inclusive. Spec #99 settled what used to be
    ///   deferred to spec issue #98: `threshold: 0.0` made an all-`REJECT`
    ///   round return `Passed` under both `majority` and `weighted`, so it is
    ///   now refused. The floor is unconditional and reaches `unanimous` and
    ///   `plurality`, which never read `threshold` — RFC-MACP-0012 §4.1 makes
    ///   that deliberate, so a threshold an author believed was in force is
    ///   never silently ignored. A rules object that *omits* `threshold` is
    ///   unaffected; `default_threshold()` is `0.5`.
    /// - `properties.voting.properties.quorum.properties.type.enum` →
    ///   [`DECISION_VOTING_QUORUM_TYPES`]. Note the evaluator additionally
    ///   accepts `n_of_m` here (`evaluator.rs`, `check_quorum`), which the
    ///   canonical enum does not list; registration refuses it so the drift
    ///   cannot enter the registry.
    /// - `properties.voting.properties.quorum.properties.value.minimum` — `0`.
    ///   The schema types this one as `number`, not `integer`, and sets no
    ///   `maximum` even for `type: "percentage"`, so neither is enforced.
    /// - `properties.voting.properties.weights.additionalProperties.exclusiveMinimum`
    ///   — `0`. Zero and every negative value are refused. The `weights` map is
    ///   the weighted electorate, so a legitimately zero-weighted observer is
    ///   expressed by **omission** from the map, never by an explicit `0`.
    ///   `exclusiveMinimum: 0` is also what closes issue #148's
    ///   `{"a": 1.0, "b": -1.0}` at admission time. The companion
    ///   `properties.voting.properties.weights.minProperties: 1` is enforced in
    ///   [`Self::validate_conditional_constraints`], where the raw JSON is
    ///   still available to tell a supplied empty map from an absent one.
    fn validate_decision_voting(
        voting: &macp_core::policy::rules::VotingRules,
    ) -> Result<(), String> {
        if !DECISION_VOTING_ALGORITHMS.contains(&voting.algorithm.as_str()) {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: voting.algorithm '{}' is not one of: {}",
                voting.algorithm,
                DECISION_VOTING_ALGORITHMS.join(", ")
            ));
        }
        if !(voting.threshold > 0.0 && voting.threshold <= 1.0) {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: voting.threshold {} is out of range: \
                 must be greater than 0.0 and at most 1.0",
                voting.threshold
            ));
        }
        // Deterministic message: `weights` is a HashMap, so sort the offenders.
        let mut negative: Vec<&str> = voting
            .weights
            .iter()
            .filter(|(_, weight)| **weight <= 0.0 || weight.is_nan())
            .map(|(participant, _)| participant.as_str())
            .collect();
        if !negative.is_empty() {
            negative.sort_unstable();
            return Err(format!(
                "INVALID_POLICY_DEFINITION: voting.weights has non-positive values for: {} \
                 — every weight must be > 0",
                negative
                    .iter()
                    .map(|k| format!("'{k}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !DECISION_VOTING_QUORUM_TYPES.contains(&voting.quorum.quorum_type.as_str()) {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: voting.quorum.type '{}' is not one of: {}",
                voting.quorum.quorum_type,
                DECISION_VOTING_QUORUM_TYPES.join(", ")
            ));
        }
        if voting.quorum.value < 0.0 || voting.quorum.value.is_nan() {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: voting.quorum.value {} is out of range: must be >= 0",
                voting.quorum.value
            ));
        }
        Ok(())
    }

    /// Quorum-mode `threshold` value domains, mirroring
    /// `schemas/json/policy/quorum-rules.schema.json`:
    ///
    /// - `properties.threshold.properties.type.enum` — the closed pair
    ///   `n_of_m`, `percentage` (RFC-MACP-0012 1.2.0-draft). One deliberate
    ///   departure: `count` is **accepted** although the canonical enum omits
    ///   it. This runtime documents it (`docs/policy.md`) and both layers
    ///   already treat it as an alias for `n_of_m`
    ///   (`QuorumMode::effective_threshold` and
    ///   `evaluate_quorum_commitment_outcome`), so refusing it would break
    ///   documented behaviour. Spec #110 ruled the alias out of the vocabulary
    ///   permanently (issue #98 item 4), so this is now a standing departure
    ///   rather than a schema gap awaiting a decision.
    ///   `weighted` is refused by this same enum check: #110 removed it from
    ///   the vocabulary and reserved the identifier.
    /// - `properties.threshold.properties.value.type` — `integer`. The Rust
    ///   field is `f64`, so this is enforced as a zero fractional part, which
    ///   is exactly what JSON Schema's `integer` type means (`2.0` is an
    ///   integer, `0.5` is not).
    /// - `allOf[0]`: `then.properties.value.maximum` — `100` when
    ///   `type` is `percentage`.
    ///
    /// The companion `properties.threshold.properties.value.exclusiveMinimum:
    /// 0` is enforced in [`Self::validate_conditional_constraints`], where the
    /// raw JSON is still available to tell a supplied `0` from an absent
    /// `value` — `QuorumThreshold::value` defaults to `0.0`, so a
    /// parsed-struct test here would refuse every policy that sets no bar.
    fn validate_quorum_threshold(
        threshold: &macp_core::policy::rules::QuorumThreshold,
    ) -> Result<(), String> {
        let kind = threshold.threshold_type.as_str();
        if !QUORUM_THRESHOLD_TYPES.contains(&kind) {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: threshold.type '{kind}' is not one of: {}",
                QUORUM_THRESHOLD_TYPES.join(", ")
            ));
        }
        if threshold.value.fract() != 0.0 {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: threshold.value {} must be an integer",
                threshold.value
            ));
        }
        if kind == "percentage" && threshold.value > 100.0 {
            return Err(format!(
                "INVALID_POLICY_DEFINITION: threshold.value {} is out of range: \
                 must be <= 100 when threshold.type is 'percentage'",
                threshold.value
            ));
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
    use crate::defaults::{std_majority_policy, std_unanimous_policy};

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
        // A conflict, not a malformed definition. Every rejection produced by
        // `validate_definition` leads with `INVALID_POLICY_DEFINITION` (the
        // `refuse` helper below asserts that on each of them); this one must
        // not, or consumers branching on the prefix — the control plane maps
        // it to HTTP 400 — would report a 409-shaped failure as a 400.
        assert!(
            !err.starts_with("INVALID_POLICY_DEFINITION"),
            "a duplicate id is a conflict, not an invalid definition: {err}"
        );
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
        // default + the three §5.2 profiles + a + b
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn list_filtered_by_mode() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.decision")).unwrap();
        let mut task_policy = test_policy("policy.task");
        task_policy.mode = "macp.mode.task.v1".into();
        registry.register(task_policy).unwrap();

        let decision_policies = registry.list(Some("macp.mode.decision.v1"));
        // default (mode="*") + policy.decision + the three §5.2 profiles
        assert_eq!(decision_policies.len(), 5);

        let task_policies = registry.list(Some("macp.mode.task.v1"));
        // Should include: default (mode="*") + policy.task (mode matches);
        // the §5.2 profiles are Decision Mode only.
        assert_eq!(task_policies.len(), 2);
    }

    #[test]
    fn list_returns_sorted_by_id() {
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.z")).unwrap();
        registry.register(test_policy("policy.a")).unwrap();
        let all = registry.list(None);
        let ids: Vec<&str> = all.iter().map(|p| p.policy_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "policy.a",
                "policy.default",
                "policy.std.majority",
                "policy.std.supermajority",
                "policy.std.unanimous",
                "policy.z",
            ]
        );
    }

    // ── Reserved `policy.std.` namespace (RFC-MACP-0012 §2.2, §5.2, §7) ──

    #[test]
    fn new_registry_pre_registers_the_std_profiles() {
        let registry = PolicyRegistry::new();
        for expected in std_policies() {
            let found = registry
                .get(&expected.policy_id)
                .unwrap_or_else(|| panic!("{} not pre-registered", expected.policy_id));
            assert_eq!(found.mode, expected.mode);
            assert_eq!(found.schema_version, expected.schema_version);
            assert_eq!(found.rules, expected.rules);
        }
    }

    #[test]
    fn std_profiles_resolve_by_id() {
        let registry = PolicyRegistry::new();
        for expected in std_policies() {
            let resolved = registry.resolve(&expected.policy_id).unwrap();
            assert_eq!(resolved.policy_id, expected.policy_id);
        }
    }

    #[test]
    fn register_non_canonical_std_id_fails() {
        let registry = PolicyRegistry::new();
        // An assigned identifier, but with rules that are not the canonical ones.
        let mut policy = test_policy("policy.std.majority");
        policy.rules = serde_json::json!({
            "voting": { "algorithm": "plurality" },
            "commitment": { "require_vote_quorum": false }
        });
        let err = registry.register(policy).unwrap_err();
        assert!(err.starts_with("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("canonical"), "error: {err}");
        // The pre-registered definition is untouched.
        assert_eq!(
            registry.get("policy.std.majority").unwrap().rules,
            std_majority_policy().rules
        );
    }

    #[test]
    fn register_unassigned_std_id_fails_and_does_not_resolve() {
        let registry = PolicyRegistry::new();
        let err = registry
            .register(test_policy("policy.std.nonesuch"))
            .unwrap_err();
        assert!(err.starts_with("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("policy.std."), "error: {err}");
        assert!(registry.get("policy.std.nonesuch").is_none());
        assert!(matches!(
            registry.resolve("policy.std.nonesuch").unwrap_err(),
            PolicyError::UnknownPolicy(_)
        ));
    }

    #[test]
    fn register_canonical_std_id_fails_as_a_duplicate() {
        // The descriptor is canonical, so the namespace guard lets it through;
        // it is then refused as a plain duplicate of the pre-registered profile.
        let registry = PolicyRegistry::new();
        let err = registry.register(std_unanimous_policy()).unwrap_err();
        assert!(err.contains("already registered"), "error: {err}");
    }

    #[test]
    fn register_std_id_with_defaulted_rules_is_semantically_equal() {
        // §2.2: a parameter left to its schema default counts as resolving to
        // that default. `policy.std.unanimous` omits `threshold`; spelling it
        // out at the schema default of 0.5 is the same policy, so the guard
        // must fall through to the duplicate check rather than reject it.
        let registry = PolicyRegistry::new();
        let mut policy = std_unanimous_policy();
        policy.rules["voting"]["threshold"] = serde_json::json!(0.5);
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("already registered"), "error: {err}");
    }

    #[test]
    fn register_std_id_with_wrong_mode_fails() {
        let registry = PolicyRegistry::new();
        let mut policy = std_majority_policy();
        policy.mode = "macp.mode.quorum.v1".into();
        let err = registry.register(policy).unwrap_err();
        assert!(err.starts_with("INVALID_POLICY_DEFINITION"), "error: {err}");
    }

    #[test]
    fn unregister_std_profile_fails() {
        let registry = PolicyRegistry::new();
        for profile in std_policies() {
            let err = registry.unregister(&profile.policy_id).unwrap_err();
            assert!(err.contains("reserved"), "error: {err}");
            assert!(
                registry.get(&profile.policy_id).is_some(),
                "{} was removed",
                profile.policy_id
            );
        }
    }

    #[test]
    fn unregister_unassigned_std_id_reports_not_found() {
        let registry = PolicyRegistry::new();
        let err = registry.unregister("policy.std.nonesuch").unwrap_err();
        assert!(err.contains("not found"), "error: {err}");
    }

    #[test]
    fn unnamespaced_short_ids_remain_available() {
        // §2.2: `policy.majority` and friends are explicitly NOT reserved.
        let registry = PolicyRegistry::new();
        registry.register(test_policy("policy.majority")).unwrap();
        registry.register(test_policy("policy.stdlib")).unwrap();
        assert!(registry.get("policy.majority").is_some());
        assert!(registry.get("policy.stdlib").is_some());
    }

    #[test]
    fn policies_dir_cannot_smuggle_in_a_std_id() {
        // `MACP_POLICIES_DIR` is an "implementation-defined loading path" under
        // §2.2; it funnels through `register`, so the guard covers it.
        let dir = std::env::temp_dir().join(format!(
            "macp-policy-std-guard-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("evil.json");
        std::fs::write(
            &path,
            serde_json::to_string(&test_policy("policy.std.evil")).unwrap(),
        )
        .unwrap();

        let registry = PolicyRegistry::new();
        let err = registry.load_from_dir(&dir).unwrap_err();
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(registry.get("policy.std.evil").is_none());

        std::fs::remove_dir_all(&dir).ok();
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
                // `threshold.value` is typed `integer` by
                // quorum-rules.schema.json; this test used to spell it `75.0`.
                "threshold": { "type": "percentage", "value": 75 },
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

    /// Spec #112 gave the canonical `supermajority` `allOf` arm
    /// `required: ["threshold"]`, so
    /// `{"voting": {"algorithm": "supermajority"}}` — the exact object in the
    /// spec's `invalid-policy-rules/supermajority-missing-threshold.json` — is
    /// no longer a legal policy. It was already refused here, and by design
    /// rather than by accident: `default_threshold()` is `0.5`, and the
    /// `supermajority` check is `threshold <= 0.5`, so the default the schema
    /// would have supplied is exactly the value the arm declares illegal. That
    /// coincidence is what closes the gap without new code, and it is fragile
    /// enough in both directions (a looser `< 0.5` check, or a different
    /// default) to be worth pinning.
    #[test]
    fn register_supermajority_without_threshold_fails() {
        let registry = PolicyRegistry::new();
        let policy = PolicyDefinition {
            policy_id: "test-super-no-threshold".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "supermajority with no threshold at all".into(),
            rules: serde_json::json!({
                "voting": { "algorithm": "supermajority" }
            }),
            schema_version: 1,
        };
        let err = registry.register(policy).unwrap_err();
        assert!(err.contains("supermajority"), "error: {err}");
        assert!(err.contains("voting.threshold"), "error: {err}");
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

    // ── Schema value domains (schemas/json/policy/*.schema.json) ────────

    fn decision_policy(rules: serde_json::Value) -> PolicyDefinition {
        PolicyDefinition {
            policy_id: "policy.test.decision".into(),
            mode: "macp.mode.decision.v1".into(),
            description: "decision under test".into(),
            rules,
            schema_version: 1,
        }
    }

    fn quorum_policy(rules: serde_json::Value) -> PolicyDefinition {
        PolicyDefinition {
            policy_id: "policy.test.quorum".into(),
            mode: "macp.mode.quorum.v1".into(),
            description: "quorum under test".into(),
            rules,
            schema_version: 1,
        }
    }

    fn refuse(definition: PolicyDefinition) -> String {
        let err = PolicyRegistry::new().register(definition).unwrap_err();
        assert!(
            err.starts_with("INVALID_POLICY_DEFINITION"),
            "rejection must carry the error code, got: {err}"
        );
        err
    }

    fn accept(definition: PolicyDefinition) {
        PolicyRegistry::new().register(definition).unwrap();
    }

    #[test]
    fn register_unknown_voting_algorithm_fails() {
        let err = refuse(decision_policy(
            serde_json::json!({ "voting": { "algorithm": "majorty" } }),
        ));
        assert!(err.contains("voting.algorithm 'majorty'"), "error: {err}");
        // The legal set is named, so a typo is self-diagnosing.
        for algorithm in DECISION_VOTING_ALGORITHMS {
            assert!(err.contains(algorithm), "{algorithm} missing from: {err}");
        }
    }

    #[test]
    fn register_unknown_voting_algorithm_fails_for_wildcard_mode_too() {
        // `"*"` policies are validated against the Decision schema.
        let mut policy = decision_policy(serde_json::json!({ "voting": { "algorithm": "nope" } }));
        policy.mode = "*".into();
        let err = refuse(policy);
        assert!(err.contains("voting.algorithm 'nope'"), "error: {err}");
    }

    #[test]
    fn register_voting_threshold_above_one_fails() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 1.5 }
        })));
        assert!(err.contains("voting.threshold"), "error: {err}");
    }

    #[test]
    fn register_negative_voting_threshold_fails() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": -0.1 }
        })));
        assert!(err.contains("voting.threshold"), "error: {err}");
    }

    #[test]
    fn register_zero_voting_threshold_fails() {
        // Inverted by spec #99: `threshold` moved from `minimum: 0` to
        // `exclusiveMinimum: 0` in decision-rules.schema.json, because
        // `threshold: 0.0` made an all-`REJECT` round return `Passed` under
        // both `majority` and `weighted` (spec issue #98 item 2, now settled).
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.0 }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("voting.threshold"), "error: {err}");
    }

    #[test]
    fn register_zero_voting_threshold_fails_for_unanimous_too() {
        // The `exclusiveMinimum: 0` floor is **unconditional** and reaches
        // `unanimous` and `plurality`, which never read `threshold` —
        // RFC-MACP-0012 §4.1 makes that deliberate so a threshold an author
        // believed was in force is never silently ignored. This case is what
        // pins the floor itself: under `majority` the sibling test above is
        // also satisfied by the `threshold >= 0.5` arm, so it cannot tell the
        // two mirrors apart.
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous", "threshold": 0.0 }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(
            err.contains("must be greater than 0.0"),
            "the range check must be what refuses this, got: {err}"
        );
    }

    #[test]
    fn register_majority_threshold_below_half_fails() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.4 }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("voting.threshold"), "error: {err}");
        assert!(err.contains("majority"), "error: {err}");
    }

    #[test]
    fn register_majority_threshold_at_half_succeeds() {
        // The majority floor is **inclusive**, deliberately asymmetric with
        // supermajority's exclusive one: `policy.std.majority` sets exactly
        // `0.5` and RFC-MACP-0012 §2.2 pins it byte-identical on every runtime.
        accept(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        })));
    }

    #[test]
    fn register_unit_voting_threshold_succeeds() {
        accept(decision_policy(serde_json::json!({
            "voting": { "algorithm": "weighted", "threshold": 1.0, "weights": { "a": 1.0 } }
        })));
    }

    #[test]
    fn register_negative_voting_weight_fails() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "weights": { "a": -1.0 } }
        })));
        assert!(err.contains("voting.weights"), "error: {err}");
        assert!(err.contains("'a'"), "offending key not named: {err}");
    }

    #[test]
    fn register_negative_voting_weights_name_every_offender_in_order() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "weighted", "weights": { "z": -1.0, "a": -2.0, "m": 3.0 } }
        })));
        // `weights` is a HashMap; the message must not depend on iteration order.
        assert!(err.contains("'a', 'z'"), "error: {err}");
        assert!(!err.contains("'m'"), "non-offender named: {err}");
    }

    #[test]
    fn register_zero_voting_weights_fail() {
        // Inverted by spec #99: `weights.additionalProperties` moved from
        // `minimum: 0` to `exclusiveMinimum: 0`. An all-zero map yielded "no
        // votes" on a *complete* ballot set (spec issue #98 item 3); a
        // legitimately zero-weighted observer is expressed by omission from
        // the map, not by an explicit `0`, so nothing is lost.
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "weighted", "weights": { "a": 0.0, "b": 0.0 } }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        // `weights` is a HashMap; the message must not depend on iteration order.
        assert!(err.contains("'a', 'b'"), "error: {err}");
    }

    #[test]
    fn register_mixed_sign_weights_fails() {
        // The exact descriptor from issue #148. `exclusiveMinimum: 0` excludes
        // every negative value a fortiori, so the case is unauthorable: it can
        // neither be registered nor preloaded from `MACP_POLICIES_DIR`.
        //
        // Honest note on what this pins: the negative half was **already**
        // refused before spec #99, by the old `weight < 0.0` filter — this test
        // passes against the pre-#99 mirror unchanged. It is a regression pin
        // naming the reported descriptor, not the test that closes the issue.
        // What #99 newly forbids is the **zero** half, which
        // `register_zero_voting_weights_fail` and
        // `register_empty_weights_map_fails` are the discriminating tests for.
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "weighted", "weights": { "a": 1.0, "b": -1.0 } }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("'b'"), "offending key not named: {err}");
        assert!(!err.contains("'a'"), "non-offender named: {err}");
    }

    #[test]
    fn register_empty_weights_map_fails() {
        // `weights.minProperties: 1` is unconditional on the algorithm, so a
        // supplied empty map is refused even under `majority`.
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "weights": {} }
        })));
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(err.contains("voting.weights"), "error: {err}");
    }

    #[test]
    fn register_absent_weights_map_succeeds() {
        // The other half of the pair: `VotingRules.weights` is a `HashMap` that
        // defaults to empty, so a non-empty check written against the parsed
        // struct would refuse *every* non-weighted policy. This test is what
        // keeps that regression out.
        accept(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority" }
        })));
    }

    #[test]
    fn register_empty_weights_map_for_another_mode_succeeds() {
        // The `weights` non-emptiness mirror lives inside the Decision mode
        // guard. Hoisted out, it would refuse rules registered for a mode whose
        // schema does not govern `voting.weights` at all.
        let mut policy = decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "weights": {} }
        }));
        policy.mode = "macp.mode.task.v1".to_string();
        accept(policy);
    }

    #[test]
    fn register_voting_quorum_n_of_m_fails() {
        // `n_of_m` is what `evaluator::check_quorum` accepts here; the
        // canonical decision schema does not list it. Registration closes the
        // drift so it cannot enter the registry.
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "quorum": { "type": "n_of_m", "value": 1 } }
        })));
        assert!(err.contains("voting.quorum.type 'n_of_m'"), "error: {err}");
        assert!(err.contains("count, percentage"), "error: {err}");
    }

    #[test]
    fn register_negative_voting_quorum_value_fails() {
        let err = refuse(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "quorum": { "type": "count", "value": -1 } }
        })));
        assert!(err.contains("voting.quorum.value"), "error: {err}");
    }

    #[test]
    fn register_zero_voting_quorum_value_succeeds() {
        accept(decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "quorum": { "type": "percentage", "value": 0 } }
        })));
    }

    #[test]
    fn register_fractional_quorum_threshold_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 0.5 }
        })));
        assert!(err.contains("threshold.value"), "error: {err}");
        assert!(err.contains("integer"), "error: {err}");
    }

    #[test]
    fn register_negative_quorum_threshold_value_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": -1 }
        })));
        assert!(err.contains("threshold.value"), "error: {err}");
    }

    #[test]
    fn register_quorum_percentage_over_one_hundred_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "percentage", "value": 101 }
        })));
        assert!(err.contains("100"), "error: {err}");
    }

    #[test]
    fn register_quorum_percentage_of_one_hundred_succeeds() {
        accept(quorum_policy(serde_json::json!({
            "threshold": { "type": "percentage", "value": 100 }
        })));
    }

    /// Spec #110 moved quorum `threshold.value` from `minimum: 0` to
    /// `exclusiveMinimum: 0`, the quorum-side twin of the Decision floor #99
    /// tightened. A zero approval bar is trivially satisfied, so a
    /// restrictive-looking quorum policy approved everything. This test used to
    /// assert the opposite (`register_zero_quorum_threshold_value_succeeds`).
    #[test]
    fn register_zero_quorum_threshold_value_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 0 }
        })));
        assert!(err.contains("threshold.value"), "error: {err}");
        assert!(err.contains("greater than 0"), "error: {err}");

        // `percentage` too: the `maximum: 100` arm is conditional, the floor is
        // not.
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "percentage", "value": 0 }
        })));
        assert!(err.contains("greater than 0"), "error: {err}");

        // Negatives, a fortiori — this is the sole owner of the lower bound now,
        // so it has to reach them.
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": -2 }
        })));
        assert!(err.contains("greater than 0"), "error: {err}");
    }

    /// The floor is keyed on the `value` key being **supplied**, not on the
    /// parsed struct, because `QuorumThreshold::value` defaults to `0.0`. A
    /// policy that sets no bar at all must still register: it resolves to
    /// `EffectiveThreshold::Inert`, where the ApprovalRequest's own
    /// `required_approvals` stands. Getting this wrong would refuse the
    /// built-in `policy.default` wildcard, which carries no `threshold`.
    #[test]
    fn register_quorum_rules_without_a_threshold_value_succeeds() {
        accept(quorum_policy(serde_json::json!({})));
        accept(quorum_policy(serde_json::json!({ "threshold": {} })));
        accept(quorum_policy(
            serde_json::json!({ "threshold": { "type": "percentage" } }),
        ));
        accept(quorum_policy(serde_json::json!({
            "abstention": { "counts_toward_quorum": true }
        })));
    }

    #[test]
    fn register_quorum_count_threshold_type_succeeds() {
        // `count` is absent from the canonical enum but documented here and
        // treated as an `n_of_m` alias by both the mode and the evaluator.
        // Refusing it would break documented behaviour — see spec issue #98.
        accept(quorum_policy(serde_json::json!({
            "threshold": { "type": "count", "value": 2 }
        })));
    }

    /// `weighted` is refused, and since spec #110 that refusal is **agreement**
    /// with the canonical vocabulary rather than a departure from it: the
    /// identifier was removed from `threshold.type`'s enum and reserved, so it
    /// now fails the ordinary enum check. It keeps a test of its own because
    /// the reservation is normative — a future runtime must not quietly revive
    /// it with invented semantics.
    #[test]
    fn register_quorum_weighted_threshold_type_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "weighted", "value": 2 }
        })));
        assert!(err.contains("threshold.type 'weighted'"), "error: {err}");
        assert!(err.contains("is not one of"), "error: {err}");
    }

    #[test]
    fn register_unknown_quorum_threshold_type_fails() {
        let err = refuse(quorum_policy(serde_json::json!({
            "threshold": { "type": "two_thirds", "value": 2 }
        })));
        assert!(err.contains("threshold.type 'two_thirds'"), "error: {err}");
        assert!(err.contains("n_of_m"), "error: {err}");
    }

    #[test]
    fn quorum_threshold_constraints_apply_to_wildcard_policies() {
        // Was `quorum_threshold_constraints_do_not_apply_to_wildcard_policies`,
        // pinning a deferred fail-open. Phase 3 closed it: a `mode: "*"` policy
        // binds to a quorum session (`Runtime::handle_session_start` only tests
        // `policy.mode != "*" && policy.mode != mode_name`) and
        // `QuorumMode::effective_threshold` re-parses these rules as
        // `QuorumPolicyRules`, so the wildcard must clear the quorum domain as
        // well as Decision's. Both halves of the old hole are asserted below:
        // `validate_rules_for_mode` now checks a wildcard against every
        // standards-track schema, and `validate_conditional_constraints` runs
        // its quorum block for `"*"`.
        let wildcard = |rules| PolicyDefinition {
            policy_id: "policy.test.wildcard".into(),
            mode: "*".into(),
            description: "wildcard carrying a quorum threshold".into(),
            rules,
            schema_version: 1,
        };

        // The exact payload the old test asserted was accepted.
        let err = refuse(wildcard(serde_json::json!({
            "voting": { "algorithm": "majority" },
            "threshold": { "type": "weighted", "value": 0.5 }
        })));
        assert!(err.contains("threshold.type 'weighted'"), "error: {err}");

        // ... and the fractional value from issue #145, which is what made the
        // hole severe: it reached `effective_threshold` as `0.5 as u32 == 0`.
        let err = refuse(wildcard(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 0.5 }
        })));
        assert!(err.contains("threshold.value"), "error: {err}");
        assert!(err.contains("integer"), "error: {err}");

        // A shape error (not just a value error) is caught too: `threshold` is
        // silently dropped by `DecisionPolicyRules`, so only the quorum schema
        // sees this one.
        let err = refuse(wildcard(serde_json::json!({ "threshold": "majority" })));
        assert!(err.contains("macp.mode.quorum.v1"), "error: {err}");

        // Decision-shaped wildcards, including the built-in `policy.default`
        // rules, still register: every other mode's struct ignores the fields
        // it does not know.
        accept(wildcard(serde_json::json!({
            "voting": { "algorithm": "none", "quorum": { "type": "count", "value": 0 } },
            "objection_handling": { "critical_severity_vetoes": false, "veto_threshold": 1 },
            "evaluation": { "required_before_voting": false, "minimum_confidence": 0.0 },
            "commitment": { "authority": "initiator_only", "designated_roles": [], "require_vote_quorum": false }
        })));

        // And a wildcard carrying a *valid* quorum threshold is accepted.
        accept(wildcard(serde_json::json!({
            "threshold": { "type": "percentage", "value": 60 }
        })));
    }

    #[test]
    fn quorum_threshold_value_accepts_an_integral_float() {
        // The `integer` keyword in JSON Schema 2020-12 matches any number with
        // a zero fractional part, so `75.0` is a legal `threshold.value` — the
        // JSON token does not have to be spelled without a decimal point.
        // Pinned because the rest of the integrality suite only proves that
        // *fractional* values are refused; a tightening to "the token must be
        // integral" would otherwise pass every test in this repo.
        accept(quorum_policy(serde_json::json!({
            "threshold": { "type": "percentage", "value": 75.0 }
        })));
    }

    #[test]
    fn new_registry_contains_every_built_in_policy() {
        // `PolicyRegistry::new` inserts into the HashMap directly, bypassing
        // `register`, so no other test proves the built-ins are actually
        // *present* — only that they would survive validation if they went
        // through it. A `std_policies()` entry silently dropped on the floor
        // would leave `RegisterPolicy` free to claim a reserved id.
        let registry = PolicyRegistry::new();
        for id in [
            DEFAULT_POLICY_ID,
            crate::defaults::STD_MAJORITY_POLICY_ID,
            crate::defaults::STD_SUPERMAJORITY_POLICY_ID,
            crate::defaults::STD_UNANIMOUS_POLICY_ID,
        ] {
            let policy = registry
                .get(id)
                .unwrap_or_else(|| panic!("{id} is missing from a fresh registry"));
            assert_eq!(policy.policy_id, id);
            // And it resolves the way `SessionStart` will look it up.
            assert!(registry.resolve(id).is_ok(), "{id} does not resolve");
        }
        assert_eq!(
            registry.list(None).len(),
            4,
            "a fresh registry holds exactly policy.default plus the three \
             RFC-MACP-0012 §5.2 profiles"
        );
    }

    #[test]
    fn built_in_policies_survive_every_registration_constraint() {
        // `PolicyRegistry::new` inserts `policy.default` and the three
        // `policy.std.*` profiles *directly*, bypassing `register`. A validator
        // that refused one of them would therefore take the process down at
        // startup with no wire-level symptom and no other test to catch it.
        for policy in std::iter::once(default_policy()).chain(std_policies()) {
            // The id-level guards deliberately refuse these ids from callers;
            // what must hold is that their *rules* pass every schema check.
            PolicyRegistry::validate_rules_for_mode(&policy.mode, &policy.rules)
                .unwrap_or_else(|e| panic!("{} rules refused: {e}", policy.policy_id));
            PolicyRegistry::validate_conditional_constraints(&policy.mode, &policy.rules)
                .unwrap_or_else(|e| panic!("{} rules refused: {e}", policy.policy_id));
        }
        // The `policy.std.*` profiles must additionally survive the full
        // registration path, which a client may legitimately re-send.
        for policy in std_policies() {
            PolicyRegistry::validate_definition(&policy)
                .unwrap_or_else(|e| panic!("{} would be refused: {e}", policy.policy_id));
        }
    }

    // ── MACP_POLICIES_DIR: fail-closed loading and the dry-run pass ─────

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macp-policy-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_policy(dir: &std::path::Path, file: &str, definition: &PolicyDefinition) {
        std::fs::write(
            dir.join(file),
            serde_json::to_string_pretty(definition).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn policies_dir_rejects_an_out_of_schema_file() {
        // `load_from_dir` funnels through `register`, so the new constraints
        // apply to the startup path — fatally, by design.
        let dir = temp_dir("dir-refusal");
        let mut bad = decision_policy(serde_json::json!({ "voting": { "algorithm": "majorty" } }));
        bad.policy_id = "policy.ops.typo".into();
        write_policy(&dir, "typo.json", &bad);

        let registry = PolicyRegistry::new();
        let err = registry.load_from_dir(&dir).unwrap_err();
        assert!(err.contains("typo.json"), "error: {err}");
        assert!(err.contains("INVALID_POLICY_DEFINITION"), "error: {err}");
        assert!(registry.get("policy.ops.typo").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_dir_reports_every_file_by_name() {
        let dir = temp_dir("dry-run");
        let mut good = decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority", "threshold": 0.5 }
        }));
        good.policy_id = "policy.ops.good".into();
        write_policy(&dir, "a-good.json", &good);

        let mut bad = quorum_policy(serde_json::json!({
            "threshold": { "type": "n_of_m", "value": 0.5 }
        }));
        bad.policy_id = "policy.ops.bad".into();
        write_policy(&dir, "b-bad.json", &bad);

        let mut also_good = decision_policy(serde_json::json!({
            "voting": { "algorithm": "unanimous" }
        }));
        also_good.policy_id = "policy.ops.also-good".into();
        write_policy(&dir, "c-also-good.json", &also_good);

        // Not a policy file at all — must not abort the pass either.
        std::fs::write(dir.join("d-garbage.json"), "{ not json").unwrap();
        // Non-JSON files are ignored entirely.
        std::fs::write(dir.join("README.txt"), "ignored").unwrap();

        let outcomes = PolicyRegistry::validate_dir(&dir).unwrap();
        let names: Vec<String> = outcomes
            .iter()
            .map(|o| o.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // Deterministic order, and the pass continues past the first rejection.
        assert_eq!(
            names,
            vec![
                "a-good.json",
                "b-bad.json",
                "c-also-good.json",
                "d-garbage.json"
            ]
        );
        assert_eq!(outcomes[0].result.as_deref(), Ok("policy.ops.good"));
        let rejection = outcomes[1].result.as_ref().unwrap_err();
        assert!(rejection.contains("threshold.value"), "{rejection}");
        assert_eq!(outcomes[2].result.as_deref(), Ok("policy.ops.also-good"));
        assert!(outcomes[3].result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_dir_detects_duplicates_without_touching_the_registry() {
        let dir = temp_dir("dry-run-dup");
        let mut policy = decision_policy(serde_json::json!({
            "voting": { "algorithm": "majority" }
        }));
        policy.policy_id = "policy.ops.dup".into();
        write_policy(&dir, "a.json", &policy);
        write_policy(&dir, "b.json", &policy);

        let outcomes = PolicyRegistry::validate_dir(&dir).unwrap();
        assert!(outcomes[0].result.is_ok());
        assert!(
            outcomes[1]
                .result
                .as_ref()
                .unwrap_err()
                .contains("already registered"),
            "{:?}",
            outcomes[1].result
        );

        // Validation runs against a scratch registry: nothing was loaded.
        let registry = PolicyRegistry::new();
        assert!(registry.get("policy.ops.dup").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_dir_errors_only_when_the_directory_is_unreadable() {
        let err = PolicyRegistry::validate_dir(std::path::Path::new(
            "/macp-policy-does-not-exist-anywhere",
        ))
        .unwrap_err();
        assert!(err.contains("cannot read policies dir"), "error: {err}");
    }

    // ── Parity with the canonical schemas ───────────────────────────────

    /// The canonical policy schemas, if this checkout can see them.
    ///
    /// Resolution order: `MACP_POLICY_SCHEMAS_DIR` (set by the
    /// `conformance-oracle` CI job, which already checks the spec repo out),
    /// then the sibling spec checkout used in local development.
    ///
    /// Setting `MACP_POLICY_SCHEMAS_DIR` is an explicit assertion that the
    /// canonical schemas are present, so a directory that does not exist there
    /// **panics** rather than skipping: otherwise a spec-repo reorganisation
    /// that moves `schemas/json/policy` would turn the parity gate into a
    /// silent pass in CI, and libtest swallows `println!` for passing tests.
    /// Only the local-dev sibling-checkout fallback may skip — contributors
    /// without a spec checkout must still get a green suite, which is also why
    /// the test is deliberately **not** `#[ignore]`d (an ignored test runs
    /// nowhere, CI included).
    fn canonical_schema_dir() -> Option<std::path::PathBuf> {
        if let Ok(dir) = std::env::var("MACP_POLICY_SCHEMAS_DIR") {
            let path = std::path::PathBuf::from(dir);
            assert!(
                path.is_dir(),
                "MACP_POLICY_SCHEMAS_DIR is set to '{}', which is not a directory. \
                 Setting it asserts the canonical policy schemas are available; \
                 refusing to skip the parity check silently. Point it at the spec \
                 repo's schemas/json/policy, or unset it to fall back to a sibling \
                 checkout.",
                path.display()
            );
            return Some(path);
        }
        let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../multiagentcoordinationprotocol/schemas/json/policy");
        sibling.is_dir().then_some(sibling)
    }

    fn schema(dir: &std::path::Path, file: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("cannot read canonical schema {file}: {e}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{file} is not valid JSON: {e}"))
    }

    fn enum_values(node: &serde_json::Value) -> Vec<String> {
        let mut values: Vec<String> = node
            .get("enum")
            .and_then(|e| e.as_array())
            .unwrap_or_else(|| panic!("no enum at {node}"))
            .iter()
            .map(|v| v.as_str().expect("enum entry is a string").to_string())
            .collect();
        values.sort();
        values
    }

    fn sorted(values: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = values.iter().map(|s| s.to_string()).collect();
        out.sort();
        out
    }

    #[test]
    fn enum_lists_match_the_canonical_schemas() {
        let Some(dir) = canonical_schema_dir() else {
            println!(
                "SKIP enum_lists_match_the_canonical_schemas: no canonical schema directory \
                 (set MACP_POLICY_SCHEMAS_DIR or check the spec repo out beside this one)"
            );
            return;
        };

        let decision = schema(&dir, "decision-rules.schema.json");
        let voting = &decision["properties"]["voting"]["properties"];
        assert_eq!(
            enum_values(&voting["algorithm"]),
            sorted(&DECISION_VOTING_ALGORITHMS),
            "voting.algorithm has drifted from decision-rules.schema.json"
        );
        assert_eq!(
            enum_values(&voting["quorum"]["properties"]["type"]),
            sorted(&DECISION_VOTING_QUORUM_TYPES),
            "voting.quorum.type has drifted from decision-rules.schema.json"
        );
        assert_eq!(
            voting["threshold"]["exclusiveMinimum"],
            serde_json::json!(0),
            "voting.threshold's lower bound has drifted from decision-rules.schema.json"
        );
        assert_eq!(voting["threshold"]["maximum"], serde_json::json!(1));
        assert_eq!(
            voting["quorum"]["properties"]["value"]["minimum"],
            serde_json::json!(0)
        );
        assert_eq!(
            voting["weights"]["additionalProperties"]["exclusiveMinimum"],
            serde_json::json!(0),
            "voting.weights' lower bound has drifted from decision-rules.schema.json"
        );
        // `minProperties` and the `majority` `allOf` arm are mirrored too, so a
        // future loosening upstream is caught here rather than silently
        // absorbed. The parity test can only pin keywords somebody thought to
        // mirror; #99 added both of these and the previous assertion set would
        // have noticed neither.
        assert_eq!(
            voting["weights"]["minProperties"],
            serde_json::json!(1),
            "voting.weights.minProperties has drifted from decision-rules.schema.json"
        );
        let majority_arm = decision["allOf"]
            .as_array()
            .expect("decision-rules.schema.json has an allOf array")
            .iter()
            .find(|arm| {
                arm["if"]["properties"]["voting"]["properties"]["algorithm"]["const"]
                    == serde_json::json!("majority")
            })
            .expect("decision-rules.schema.json has an allOf arm keyed on algorithm 'majority'");
        assert_eq!(
            majority_arm["then"]["properties"]["voting"]["properties"]["threshold"]["minimum"],
            serde_json::json!(0.5),
            "the majority threshold floor has drifted from decision-rules.schema.json; \
             note it is deliberately inclusive, unlike supermajority's exclusive 0.5"
        );

        // The `supermajority` arm requires the key as well as constraining the
        // value (spec #112 / issue #101). Without `required` the arm was
        // self-contradicting — `{"algorithm": "supermajority"}` validated and
        // the subschema default of 0.5 then supplied a value the same arm
        // declares illegal. This runtime reaches the same refusal through
        // `default_threshold()` landing on 0.5 and failing the `> 0.5` check,
        // so the assertion here is what keeps the two facts tied together: if
        // upstream ever drops `required`, an omitted threshold becomes legal
        // and our refusal becomes the departure.
        let supermajority_arm = decision["allOf"]
            .as_array()
            .expect("decision-rules.schema.json has an allOf array")
            .iter()
            .find(|arm| {
                arm["if"]["properties"]["voting"]["properties"]["algorithm"]["const"]
                    == serde_json::json!("supermajority")
            })
            .expect(
                "decision-rules.schema.json has an allOf arm keyed on algorithm 'supermajority'",
            );
        assert_eq!(
            supermajority_arm["then"]["properties"]["voting"]["required"],
            serde_json::json!(["threshold"]),
            "the supermajority arm no longer requires voting.threshold in \
             decision-rules.schema.json"
        );
        assert_eq!(
            supermajority_arm["then"]["properties"]["voting"]["properties"]["threshold"]
                ["exclusiveMinimum"],
            serde_json::json!(0.5),
            "the supermajority threshold floor has drifted from decision-rules.schema.json"
        );

        let quorum = schema(&dir, "quorum-rules.schema.json");
        let threshold = &quorum["properties"]["threshold"];
        // One deliberate departure, documented on the constant: `count` is
        // accepted as an alias this runtime documents. `weighted` was the
        // second until spec #110 removed it from the canonical enum; our
        // refusal is now agreement, so it is no longer carved out here.
        // Everything else must match.
        let mut ours: Vec<String> = QUORUM_THRESHOLD_TYPES
            .iter()
            .filter(|t| **t != "count")
            .map(|t| t.to_string())
            .collect();
        ours.sort();
        assert_eq!(
            enum_values(&threshold["properties"]["type"]),
            ours,
            "threshold.type has drifted from quorum-rules.schema.json"
        );
        assert_eq!(
            threshold["properties"]["value"]["type"],
            serde_json::json!("integer")
        );
        // Exclusive as of spec #110 — the quorum-side twin of the Decision
        // floor tightening #99 landed. A zero approval bar is trivially
        // satisfied, so a restrictive-looking quorum policy approved
        // everything.
        assert_eq!(
            threshold["properties"]["value"]["exclusiveMinimum"],
            serde_json::json!(0),
            "threshold.value's lower bound has drifted from quorum-rules.schema.json"
        );
        assert_eq!(
            threshold["allOf"][0]["then"]["properties"]["value"]["maximum"],
            serde_json::json!(100)
        );
    }
}
