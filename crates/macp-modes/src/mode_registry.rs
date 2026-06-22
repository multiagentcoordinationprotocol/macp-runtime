use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

use crate::mode::decision::DecisionMode;
use crate::mode::handoff::HandoffMode;
use crate::mode::multi_round::MultiRoundMode;
use crate::mode::passthrough::PassthroughMode;
use crate::mode::proposal::ProposalMode;
use crate::mode::quorum::QuorumMode;
use crate::mode::task::TaskMode;
use crate::mode::{
    extension_mode_descriptors, standard_mode_descriptors, Mode, STANDARD_MODE_NAMES,
};
use macp_pb::pb::ModeDescriptor;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModeConformanceCatalog {
    pub fixture_set_name: String,
    pub golden_transcript_paths: Vec<String>,
    pub conformance_fixture_paths: Vec<String>,
}

pub trait ModeFactory: Send + Sync {
    fn create(&self) -> Box<dyn Mode>;
}

pub trait ModeDescriptorProvider: Send + Sync {
    fn descriptor(&self) -> Option<ModeDescriptor>;
}

pub trait ModeSchemaProvider: Send + Sync {
    fn schema_uris(&self) -> HashMap<String, String>;
}

pub trait ModeConformanceProvider: Send + Sync {
    fn fixture_set_name(&self) -> &'static str;
    fn golden_transcript_paths(&self) -> Vec<&'static str>;
    fn conformance_fixture_paths(&self) -> Vec<&'static str>;
}

pub struct StaticModeFactory {
    constructor: fn() -> Box<dyn Mode>,
}

impl StaticModeFactory {
    pub fn new(constructor: fn() -> Box<dyn Mode>) -> Self {
        Self { constructor }
    }
}

impl ModeFactory for StaticModeFactory {
    fn create(&self) -> Box<dyn Mode> {
        (self.constructor)()
    }
}

pub struct ClosureModeFactory {
    constructor: Arc<dyn Fn() -> Box<dyn Mode> + Send + Sync>,
}

impl ClosureModeFactory {
    pub fn new(constructor: Arc<dyn Fn() -> Box<dyn Mode> + Send + Sync>) -> Self {
        Self { constructor }
    }
}

impl ModeFactory for ClosureModeFactory {
    fn create(&self) -> Box<dyn Mode> {
        (self.constructor)()
    }
}

#[derive(Clone)]
pub struct StaticModeDescriptorProvider {
    descriptor: Option<ModeDescriptor>,
}

impl StaticModeDescriptorProvider {
    pub fn new(descriptor: Option<ModeDescriptor>) -> Self {
        Self { descriptor }
    }
}

impl ModeDescriptorProvider for StaticModeDescriptorProvider {
    fn descriptor(&self) -> Option<ModeDescriptor> {
        self.descriptor.clone()
    }
}

#[derive(Clone, Default)]
pub struct StaticModeSchemaProvider {
    schema_uris: HashMap<String, String>,
}

impl StaticModeSchemaProvider {
    pub fn new(schema_uris: HashMap<String, String>) -> Self {
        Self { schema_uris }
    }
}

impl ModeSchemaProvider for StaticModeSchemaProvider {
    fn schema_uris(&self) -> HashMap<String, String> {
        self.schema_uris.clone()
    }
}

#[derive(Clone, Default)]
pub struct StaticModeConformanceProvider {
    fixture_set_name: &'static str,
    golden_transcript_paths: Vec<&'static str>,
    conformance_fixture_paths: Vec<&'static str>,
}

impl StaticModeConformanceProvider {
    pub fn new(
        fixture_set_name: &'static str,
        golden_transcript_paths: Vec<&'static str>,
        conformance_fixture_paths: Vec<&'static str>,
    ) -> Self {
        Self {
            fixture_set_name,
            golden_transcript_paths,
            conformance_fixture_paths,
        }
    }
}

impl ModeConformanceProvider for StaticModeConformanceProvider {
    fn fixture_set_name(&self) -> &'static str {
        self.fixture_set_name
    }

    fn golden_transcript_paths(&self) -> Vec<&'static str> {
        self.golden_transcript_paths.clone()
    }

    fn conformance_fixture_paths(&self) -> Vec<&'static str> {
        self.conformance_fixture_paths.clone()
    }
}

pub struct ModeRegistration {
    pub mode_name: String,
    pub factory: Arc<dyn ModeFactory>,
    pub descriptor_provider: Arc<dyn ModeDescriptorProvider>,
    pub schema_provider: Arc<dyn ModeSchemaProvider>,
    pub conformance_provider: Arc<dyn ModeConformanceProvider>,
    pub standards_track: bool,
    pub builtin: bool,
    pub strict_session_start: bool,
}

impl ModeRegistration {
    pub fn descriptor(&self) -> Option<ModeDescriptor> {
        self.descriptor_provider.descriptor().map(|mut descriptor| {
            descriptor.mode = self.mode_name.clone();
            descriptor.schema_uris = self.schema_provider.schema_uris();
            descriptor
        })
    }

    pub fn conformance_catalog(&self) -> ModeConformanceCatalog {
        ModeConformanceCatalog {
            fixture_set_name: self.conformance_provider.fixture_set_name().to_string(),
            golden_transcript_paths: self
                .conformance_provider
                .golden_transcript_paths()
                .into_iter()
                .map(str::to_string)
                .collect(),
            conformance_fixture_paths: self
                .conformance_provider
                .conformance_fixture_paths()
                .into_iter()
                .map(str::to_string)
                .collect(),
        }
    }
}

pub struct ModeRegistry {
    entries: RwLock<HashMap<String, ModeRegistration>>,
    change_tx: broadcast::Sender<()>,
}

impl ModeRegistry {
    /// Build the default registry with 5 standards-track modes and 1 built-in extension.
    pub fn build_default(evaluator: Arc<dyn macp_core::policy::PolicyEvaluator>) -> Self {
        let std_descriptors = standard_mode_descriptors();
        let ext_descriptors = extension_mode_descriptors();
        let mut descriptor_map: HashMap<String, ModeDescriptor> = std_descriptors
            .into_iter()
            .chain(ext_descriptors)
            .map(|d| (d.mode.clone(), d))
            .collect();

        let mut entries = HashMap::new();

        Self::insert_builtin(
            &mut entries,
            "macp.mode.decision.v1",
            Arc::new(ClosureModeFactory::new(Arc::new({
                let e = evaluator.clone();
                move || Box::new(DecisionMode::new(e.clone())) as Box<dyn Mode>
            }))),
            descriptor_map.remove("macp.mode.decision.v1"),
            true,
            true,
            "decision",
            vec!["tests/conformance/decision_happy_path.json"],
            vec![
                "tests/conformance/decision_happy_path.json",
                "tests/conformance/decision_reject_paths.json",
            ],
        );
        Self::insert_builtin(
            &mut entries,
            "macp.mode.proposal.v1",
            Arc::new(ClosureModeFactory::new(Arc::new({
                let e = evaluator.clone();
                move || Box::new(ProposalMode::new(e.clone())) as Box<dyn Mode>
            }))),
            descriptor_map.remove("macp.mode.proposal.v1"),
            true,
            true,
            "proposal",
            vec!["tests/conformance/proposal_happy_path.json"],
            vec![
                "tests/conformance/proposal_happy_path.json",
                "tests/conformance/proposal_reject_paths.json",
            ],
        );
        Self::insert_builtin(
            &mut entries,
            "macp.mode.task.v1",
            Arc::new(ClosureModeFactory::new(Arc::new({
                let e = evaluator.clone();
                move || Box::new(TaskMode::new(e.clone())) as Box<dyn Mode>
            }))),
            descriptor_map.remove("macp.mode.task.v1"),
            true,
            true,
            "task",
            vec!["tests/conformance/task_happy_path.json"],
            vec![
                "tests/conformance/task_happy_path.json",
                "tests/conformance/task_reject_paths.json",
            ],
        );
        Self::insert_builtin(
            &mut entries,
            "macp.mode.handoff.v1",
            Arc::new(ClosureModeFactory::new(Arc::new({
                let e = evaluator.clone();
                move || Box::new(HandoffMode::new(e.clone())) as Box<dyn Mode>
            }))),
            descriptor_map.remove("macp.mode.handoff.v1"),
            true,
            true,
            "handoff",
            vec!["tests/conformance/handoff_happy_path.json"],
            vec![
                "tests/conformance/handoff_happy_path.json",
                "tests/conformance/handoff_reject_paths.json",
            ],
        );
        Self::insert_builtin(
            &mut entries,
            "macp.mode.quorum.v1",
            Arc::new(ClosureModeFactory::new(Arc::new({
                let e = evaluator.clone();
                move || Box::new(QuorumMode::new(e.clone())) as Box<dyn Mode>
            }))),
            descriptor_map.remove("macp.mode.quorum.v1"),
            true,
            true,
            "quorum",
            vec!["tests/conformance/quorum_happy_path.json"],
            vec![
                "tests/conformance/quorum_happy_path.json",
                "tests/conformance/quorum_reject_paths.json",
            ],
        );
        Self::insert_builtin(
            &mut entries,
            "ext.multi_round.v1",
            Arc::new(StaticModeFactory::new(|| {
                Box::new(MultiRoundMode) as Box<dyn Mode>
            })),
            descriptor_map.remove("ext.multi_round.v1"),
            false,
            true,
            "multi_round",
            vec!["tests/conformance/multi_round_happy_path.json"],
            vec![
                "tests/conformance/multi_round_happy_path.json",
                "tests/conformance/multi_round_reject_paths.json",
            ],
        );

        let (change_tx, _) = broadcast::channel(16);
        Self {
            entries: RwLock::new(entries),
            change_tx,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_builtin(
        entries: &mut HashMap<String, ModeRegistration>,
        name: &str,
        factory: Arc<dyn ModeFactory>,
        descriptor: Option<ModeDescriptor>,
        standards_track: bool,
        strict_session_start: bool,
        fixture_set_name: &'static str,
        golden_transcripts: Vec<&'static str>,
        conformance_fixtures: Vec<&'static str>,
    ) {
        let schema_uris = descriptor
            .as_ref()
            .map(|d| d.schema_uris.clone())
            .unwrap_or_default();
        let descriptor_provider = Arc::new(StaticModeDescriptorProvider::new(descriptor));
        let schema_provider = Arc::new(StaticModeSchemaProvider::new(schema_uris));
        let conformance_provider = Arc::new(StaticModeConformanceProvider::new(
            fixture_set_name,
            golden_transcripts,
            conformance_fixtures,
        ));
        entries.insert(
            name.to_string(),
            ModeRegistration {
                mode_name: name.to_string(),
                factory,
                descriptor_provider,
                schema_provider,
                conformance_provider,
                standards_track,
                builtin: true,
                strict_session_start,
            },
        );
    }

    fn ordered_standard_names(entries: &HashMap<String, ModeRegistration>) -> Vec<String> {
        let mut names: Vec<String> = STANDARD_MODE_NAMES
            .iter()
            .filter(|name| {
                entries
                    .get(**name)
                    .map(|entry| entry.standards_track)
                    .unwrap_or(false)
            })
            .map(|name| (*name).to_string())
            .collect();

        let mut promoted: Vec<String> = entries
            .iter()
            .filter(|(name, entry)| {
                entry.standards_track && !STANDARD_MODE_NAMES.contains(&name.as_str())
            })
            .map(|(name, _)| name.clone())
            .collect();
        promoted.sort();
        names.extend(promoted);
        names
    }

    pub fn get_mode(&self, name: &str) -> Option<ModeRef<'_>> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(name) {
            Some(ModeRef {
                registry: self,
                name: name.to_string(),
            })
        } else {
            None
        }
    }

    pub fn standard_mode_names(&self) -> Vec<String> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        Self::ordered_standard_names(&guard)
    }

    pub fn standard_mode_descriptors(&self) -> Vec<ModeDescriptor> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        Self::ordered_standard_names(&guard)
            .into_iter()
            .filter_map(|name| guard.get(&name).and_then(ModeRegistration::descriptor))
            .collect()
    }

    pub fn extension_mode_names(&self) -> Vec<String> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<String> = guard
            .iter()
            .filter(|(_, e)| !e.standards_track)
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    pub fn extension_mode_descriptors(&self) -> Vec<ModeDescriptor> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut descriptors: Vec<ModeDescriptor> = guard
            .iter()
            .filter(|(_, e)| !e.standards_track)
            .filter_map(|(_, e)| e.descriptor())
            .collect();
        descriptors.sort_by(|a, b| a.mode.cmp(&b.mode));
        descriptors
    }

    pub fn all_mode_names(&self) -> Vec<String> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn all_mode_descriptors(&self) -> Vec<ModeDescriptor> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut descriptors: Vec<ModeDescriptor> = guard
            .values()
            .filter_map(ModeRegistration::descriptor)
            .collect();
        descriptors.sort_by(|a, b| a.mode.cmp(&b.mode));
        descriptors
    }

    pub fn all_mode_conformance(&self) -> Vec<(String, ModeConformanceCatalog)> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut conformance: Vec<(String, ModeConformanceCatalog)> = guard
            .iter()
            .map(|(name, entry)| (name.clone(), entry.conformance_catalog()))
            .collect();
        conformance.sort_by(|a, b| a.0.cmp(&b.0));
        conformance
    }

    pub fn is_standard_mode(&self, name: &str) -> bool {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard.get(name).map(|e| e.standards_track).unwrap_or(false)
    }

    pub fn requires_strict_session_start(&self, name: &str) -> bool {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(name)
            .map(|entry| entry.strict_session_start)
            .unwrap_or(false)
    }

    /// Returns the mode_version from the mode's descriptor, if available.
    pub fn get_mode_version(&self, name: &str) -> Option<String> {
        let guard = self.entries.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(name)
            .and_then(|entry| entry.descriptor())
            .map(|d| d.mode_version)
            .filter(|v| !v.is_empty())
    }

    fn validate_extension_descriptor(descriptor: &ModeDescriptor) -> Result<(), String> {
        if descriptor.mode.trim().is_empty() {
            return Err("mode name must not be empty".into());
        }
        if descriptor.mode.starts_with("macp.mode.") {
            return Err("cannot register extension with reserved macp.mode.* namespace".into());
        }
        if descriptor.message_types.is_empty() {
            return Err("extension descriptor must declare at least one message type".into());
        }
        if descriptor.mode_version.trim().is_empty() {
            return Err("extension descriptor must bind mode_version".into());
        }
        for terminal in &descriptor.terminal_message_types {
            if !descriptor
                .message_types
                .iter()
                .any(|message_type| message_type == terminal)
            {
                return Err(format!(
                    "terminal message type '{}' must also appear in message_types",
                    terminal
                ));
            }
        }
        Ok(())
    }

    /// Register a new descriptor-driven extension mode dynamically.
    ///
    /// Dynamically registered extensions currently use `PassthroughMode`, which
    /// validates only the descriptor-declared message types and commitment
    /// authority. This keeps runtime behavior explicit until a richer external
    /// plugin mechanism is introduced.
    pub fn register_extension(&self, descriptor: ModeDescriptor) -> Result<(), String> {
        Self::validate_extension_descriptor(&descriptor)?;
        let name = descriptor.mode.clone();
        let schema_uris = descriptor.schema_uris.clone();
        let allowed_types: Vec<String> = descriptor
            .message_types
            .iter()
            .filter(|t| *t != "SessionStart")
            .cloned()
            .collect();
        let allowed_types = Arc::new(allowed_types);
        let factory: Arc<dyn ModeFactory> = Arc::new(ClosureModeFactory::new(Arc::new({
            let allowed_types = Arc::clone(&allowed_types);
            move || {
                Box::new(PassthroughMode {
                    allowed_message_types: (*allowed_types).clone(),
                }) as Box<dyn Mode>
            }
        })));
        let descriptor_provider = Arc::new(StaticModeDescriptorProvider::new(Some(descriptor)));
        let schema_provider = Arc::new(StaticModeSchemaProvider::new(schema_uris));
        let conformance_provider = Arc::new(StaticModeConformanceProvider::default());

        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if guard.contains_key(&name) {
            return Err(format!("mode '{}' is already registered", name));
        }
        guard.insert(
            name.clone(),
            ModeRegistration {
                mode_name: name,
                factory,
                descriptor_provider,
                schema_provider,
                conformance_provider,
                standards_track: false,
                builtin: false,
                strict_session_start: false,
            },
        );
        drop(guard);
        let _ = self.change_tx.send(());
        Ok(())
    }

    /// Unregister a dynamically registered extension mode.
    pub fn unregister_extension(&self, mode: &str) -> Result<(), String> {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        match guard.get(mode) {
            None => return Err(format!("mode '{}' not found", mode)),
            Some(entry) if entry.builtin => {
                return Err(format!("cannot unregister built-in mode '{}'", mode))
            }
            Some(entry) if entry.standards_track => {
                return Err(format!("cannot unregister standards-track mode '{}'", mode))
            }
            _ => {}
        }
        guard.remove(mode);
        drop(guard);
        let _ = self.change_tx.send(());
        Ok(())
    }

    /// Promote an extension mode to standards-track.
    /// Optionally re-keys the entry with a new identifier.
    pub fn promote_mode(&self, mode: &str, new_name: Option<&str>) -> Result<String, String> {
        let mut guard = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let entry = guard
            .get(mode)
            .ok_or_else(|| format!("mode '{}' not found", mode))?;
        if entry.standards_track {
            return Err(format!("mode '{}' is already standards-track", mode));
        }

        let final_name = new_name.unwrap_or(mode).to_string();
        if final_name != mode && guard.contains_key(&final_name) {
            return Err(format!(
                "cannot promote: target name '{}' already exists",
                final_name
            ));
        }

        let mut registration = guard
            .remove(mode)
            .ok_or_else(|| format!("mode '{}' not found", mode))?;
        registration.standards_track = true;
        registration.strict_session_start = true;
        registration.mode_name = final_name.clone();
        guard.insert(final_name.clone(), registration);
        drop(guard);
        let _ = self.change_tx.send(());
        Ok(final_name)
    }

    /// Subscribe to mode registry change notifications.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<()> {
        self.change_tx.subscribe()
    }
}

/// A handle that allows calling mode methods without keeping the registry read
/// lock held across callback execution.
pub struct ModeRef<'a> {
    registry: &'a ModeRegistry,
    name: String,
}

impl<'a> ModeRef<'a> {
    fn factory(&self) -> Result<Arc<dyn ModeFactory>, macp_core::error::MacpError> {
        let guard = self
            .registry
            .entries
            .read()
            .unwrap_or_else(|e| e.into_inner());
        guard
            .get(&self.name)
            .map(|entry| Arc::clone(&entry.factory))
            .ok_or(macp_core::error::MacpError::UnknownMode)
    }

    pub fn on_session_start(
        &self,
        session: &macp_core::session::Session,
        env: &macp_pb::pb::Envelope,
    ) -> Result<crate::mode::ModeResponse, macp_core::error::MacpError> {
        let mode = self.factory()?.create();
        mode.on_session_start(session, env)
    }

    pub fn on_message(
        &self,
        session: &macp_core::session::Session,
        env: &macp_pb::pb::Envelope,
    ) -> Result<crate::mode::ModeResponse, macp_core::error::MacpError> {
        let mode = self.factory()?.create();
        mode.on_message(session, env)
    }

    pub fn authorize_sender(
        &self,
        session: &macp_core::session::Session,
        env: &macp_pb::pb::Envelope,
    ) -> Result<(), macp_core::error::MacpError> {
        let mode = self.factory()?.create();
        mode.authorize_sender(session, env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::EXTENSION_MODE_NAMES;

    #[test]
    fn build_default_contains_all_standard_modes() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        for name in STANDARD_MODE_NAMES {
            assert!(registry.get_mode(name).is_some(), "missing mode: {name}");
            assert!(registry.is_standard_mode(name));
            assert!(registry.requires_strict_session_start(name));
        }
    }

    #[test]
    fn build_default_contains_multi_round_as_extension() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert!(registry.get_mode("ext.multi_round.v1").is_some());
        assert!(!registry.is_standard_mode("ext.multi_round.v1"));
        assert!(registry.requires_strict_session_start("ext.multi_round.v1"));
    }

    #[test]
    fn standard_mode_names_returns_five() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert_eq!(registry.standard_mode_names().len(), 5);
    }

    #[test]
    fn standard_mode_descriptors_returns_five() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert_eq!(registry.standard_mode_descriptors().len(), 5);
    }

    #[test]
    fn all_mode_names_returns_six() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert_eq!(registry.all_mode_names().len(), 6);
    }

    #[test]
    fn extension_mode_names_returns_one() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let ext = registry.extension_mode_names();
        assert_eq!(ext.len(), 1);
        assert!(ext.contains(&"ext.multi_round.v1".to_string()));
    }

    #[test]
    fn extension_mode_descriptors_returns_one() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descs = registry.extension_mode_descriptors();
        assert_eq!(descs.len(), 1);
        assert_eq!(descs[0].mode, "ext.multi_round.v1");
    }

    #[test]
    fn unknown_mode_returns_none() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert!(registry.get_mode("nonexistent").is_none());
        assert!(!registry.is_standard_mode("nonexistent"));
        assert!(!registry.requires_strict_session_start("nonexistent"));
    }

    #[test]
    fn register_extension_mode() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.custom.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Custom Mode".into(),
            description: "Test custom mode".into(),
            message_types: vec![
                "SessionStart".into(),
                "CustomMsg".into(),
                "Commitment".into(),
            ],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        };
        registry.register_extension(descriptor).unwrap();
        assert!(registry.get_mode("ext.custom.v1").is_some());
        assert!(!registry.is_standard_mode("ext.custom.v1"));
        assert_eq!(registry.all_mode_names().len(), 7);
        assert_eq!(registry.extension_mode_names().len(), 2);
    }

    #[test]
    fn register_rejects_macp_namespace() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "macp.mode.evil.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            ..Default::default()
        };
        assert!(registry.register_extension(descriptor).is_err());
    }

    #[test]
    fn register_rejects_empty_message_types() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.invalid.v1".into(),
            mode_version: "1.0.0".into(),
            ..Default::default()
        };
        assert!(registry.register_extension(descriptor).is_err());
    }

    #[test]
    fn register_rejects_terminal_not_in_message_types() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.invalid.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Custom".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        };
        assert!(registry.register_extension(descriptor).is_err());
    }

    #[test]
    fn register_rejects_duplicate() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.multi_round.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            ..Default::default()
        };
        assert!(registry.register_extension(descriptor).is_err());
    }

    #[test]
    fn unregister_extension_mode() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.temp.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            ..Default::default()
        };
        registry.register_extension(descriptor).unwrap();
        assert_eq!(registry.all_mode_names().len(), 7);
        registry.unregister_extension("ext.temp.v1").unwrap();
        assert_eq!(registry.all_mode_names().len(), 6);
        assert!(registry.get_mode("ext.temp.v1").is_none());
    }

    #[test]
    fn cannot_unregister_builtin() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert!(registry.unregister_extension("ext.multi_round.v1").is_err());
        assert!(registry
            .unregister_extension("macp.mode.decision.v1")
            .is_err());
    }

    #[test]
    fn promote_extension_to_standard() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.new.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        };
        registry.register_extension(descriptor).unwrap();
        assert!(!registry.is_standard_mode("ext.new.v1"));
        assert!(!registry.requires_strict_session_start("ext.new.v1"));

        let final_name = registry
            .promote_mode("ext.new.v1", Some("macp.mode.new.v1"))
            .unwrap();
        assert_eq!(final_name, "macp.mode.new.v1");
        assert!(registry.is_standard_mode("macp.mode.new.v1"));
        assert!(registry.requires_strict_session_start("macp.mode.new.v1"));
        assert!(registry.get_mode("ext.new.v1").is_none());
        assert!(registry.get_mode("macp.mode.new.v1").is_some());
    }

    #[test]
    fn promote_without_rename() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.keep.v1".into(),
            mode_version: "1.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        };
        registry.register_extension(descriptor).unwrap();
        let final_name = registry.promote_mode("ext.keep.v1", None).unwrap();
        assert_eq!(final_name, "ext.keep.v1");
        assert!(registry.is_standard_mode("ext.keep.v1"));
        assert!(registry.requires_strict_session_start("ext.keep.v1"));
    }

    #[test]
    fn promoted_mode_appears_in_standard_mode_names_and_descriptors() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let descriptor = ModeDescriptor {
            mode: "ext.promoted.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Promoted".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        };
        registry.register_extension(descriptor).unwrap();
        registry
            .promote_mode("ext.promoted.v1", Some("macp.mode.promoted.v1"))
            .unwrap();

        let standard_names = registry.standard_mode_names();
        assert!(standard_names.contains(&"macp.mode.promoted.v1".to_string()));

        let standard_modes: Vec<String> = registry
            .standard_mode_descriptors()
            .into_iter()
            .map(|d| d.mode)
            .collect();
        assert!(standard_modes.contains(&"macp.mode.promoted.v1".to_string()));
    }

    #[test]
    fn promote_already_standard_fails() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        assert!(registry
            .promote_mode("macp.mode.decision.v1", None)
            .is_err());
    }

    #[test]
    fn extension_names_constant_matches() {
        for name in EXTENSION_MODE_NAMES {
            let registry =
                ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
            assert!(
                registry.get_mode(name).is_some(),
                "EXTENSION_MODE_NAMES entry missing from registry: {name}"
            );
        }
    }

    #[test]
    fn conformance_catalog_exposes_builtin_fixture_sets() {
        let registry = ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator));
        let catalog = registry.all_mode_conformance();
        let decision = catalog
            .into_iter()
            .find(|(name, _)| name == "macp.mode.decision.v1")
            .expect("decision catalog should exist");
        assert_eq!(decision.1.fixture_set_name, "decision");
        assert!(decision
            .1
            .conformance_fixture_paths
            .iter()
            .any(|path| path.ends_with("decision_happy_path.json")));
    }
}
