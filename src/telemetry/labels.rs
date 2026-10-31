//! Bounded labels for the Envelope telemetry (spec #239, `_OTHER`).
//!
//! An Envelope's `mode` and `message_type` are chosen by the client. Span
//! names and metric attributes use them only when the runtime supports them,
//! and the out-of-set token [`OTHER`] otherwise, so neither span-name nor
//! metric cardinality can be grown by a client:
//!
//! - the mode is the mode itself when registered, [`OTHER`] when non-empty and
//!   unregistered, and absent when empty;
//! - the message type is itself when the registered mode's descriptor lists
//!   it, or when it is `Signal` or `Progress` (supported in every mode, and
//!   the only types an empty mode carries); else [`OTHER`]. Every type under
//!   an [`OTHER`] mode is [`OTHER`].
//!
//! The span attribute `macp.mode.id` still records the received value; only
//! names and metric attributes are normalized.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::broadcast::{self, error::TryRecvError};

use crate::mode_registry::ModeRegistry;

pub use super::process::OTHER;

/// The message types supported in every mode, including the empty one.
const AMBIENT_TYPES: [&str; 2] = ["Signal", "Progress"];

/// The mode label of one Envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeLabel {
    /// `mode` is empty (an ambient Signal or Progress).
    Empty,
    /// `mode` names a registered mode.
    Registered(String),
    /// `mode` is non-empty and not registered.
    Other,
}

impl ModeLabel {
    /// The metric attribute value: `None` when the mode is empty.
    pub fn metric_value(&self) -> Option<&str> {
        match self {
            ModeLabel::Empty => None,
            ModeLabel::Registered(mode) => Some(mode),
            ModeLabel::Other => Some(OTHER),
        }
    }

    /// The `process` span name for this mode: `process {mode}`,
    /// `process _OTHER`, or the bare `process` for an empty mode.
    pub fn process_span_name(&self) -> String {
        match self.metric_value() {
            Some(mode) => format!("process {mode}"),
            None => "process".to_string(),
        }
    }
}

/// The normalized labels of one Envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvelopeLabels {
    pub mode: ModeLabel,
    /// A supported message type, or [`OTHER`].
    pub message_type: String,
}

impl EnvelopeLabels {
    /// Labels for an Envelope whose mode is `mode` with the declared message
    /// types `declared` (`None`: the mode is not registered).
    pub fn new(mode: &str, message_type: &str, declared: Option<&HashSet<String>>) -> Self {
        let ambient = AMBIENT_TYPES.contains(&message_type);
        if mode.is_empty() {
            return Self {
                mode: ModeLabel::Empty,
                message_type: if ambient { message_type } else { OTHER }.to_string(),
            };
        }
        match declared {
            Some(types) => Self {
                mode: ModeLabel::Registered(mode.to_string()),
                message_type: if ambient || types.contains(message_type) {
                    message_type
                } else {
                    OTHER
                }
                .to_string(),
            },
            None => Self {
                mode: ModeLabel::Other,
                message_type: OTHER.to_string(),
            },
        }
    }
}

type Modes = HashMap<String, HashSet<String>>;

/// A cache of the registered modes and their declared message types.
///
/// `ModeRegistration::descriptor()` clones, so the type sets are cached and
/// rebuilt only after a registry change event (the events that drive
/// `WatchModeRegistry`). It holds registered modes only, so a client naming
/// arbitrary modes cannot grow it. Built on first use.
///
/// Read-mostly: a lookup clones the current snapshot's `Arc` under a read
/// lock. The change channel is drained by whichever caller wins a
/// `try_lock` on it; the others never wait and read the current snapshot.
/// The winner rebuilds outside every lock and swaps the result in under a
/// brief write lock. A lookup racing a registry change may therefore see the
/// snapshot from just before it, which a message racing that change could
/// not tell apart anyway.
#[derive(Default)]
pub struct ModeLabelCache {
    modes: RwLock<Option<Arc<Modes>>>,
    /// The registry change subscription; `None` until first use.
    changes: Mutex<Option<broadcast::Receiver<()>>>,
}

impl ModeLabelCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The normalized labels for an Envelope, against `registry`.
    pub fn labels(
        &self,
        registry: &ModeRegistry,
        mode: &str,
        message_type: &str,
    ) -> EnvelopeLabels {
        if mode.is_empty() {
            return EnvelopeLabels::new(mode, message_type, None);
        }
        self.refresh(registry);
        let modes = self.current(registry);
        EnvelopeLabels::new(mode, message_type, modes.get(mode))
    }

    /// The current snapshot, building the first one if none exists yet.
    fn current(&self, registry: &ModeRegistry) -> Arc<Modes> {
        if let Some(modes) = read(&self.modes).as_ref() {
            return Arc::clone(modes);
        }
        // First use: subscribe before reading, so a change after the read is
        // seen. Serialized by the subscription lock, once per cache.
        let mut changes = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(modes) = read(&self.modes).as_ref() {
            return Arc::clone(modes);
        }
        *changes = Some(registry.subscribe_changes());
        let modes = Arc::new(snapshot(registry));
        *write(&self.modes) = Some(Arc::clone(&modes));
        modes
    }

    /// Drain pending registry changes, and rebuild the snapshot if any
    /// landed. Never blocks: if another caller holds the subscription, it is
    /// already doing this.
    fn refresh(&self, registry: &ModeRegistry) {
        let Ok(mut guard) = self.changes.try_lock() else {
            return;
        };
        let Some(changes) = guard.as_mut() else {
            return; // not built yet; `current` builds it
        };
        // Any event, or a lag (events were missed), means a rebuild.
        let mut changed = false;
        while let Ok(()) | Err(TryRecvError::Lagged(_)) = changes.try_recv() {
            changed = true;
        }
        if changed {
            // Built outside the snapshot lock: readers keep the old one.
            let modes = Arc::new(snapshot(registry));
            *write(&self.modes) = Some(modes);
        }
    }
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

fn snapshot(registry: &ModeRegistry) -> Modes {
    let mut modes: Modes = registry
        .all_mode_names()
        .into_iter()
        .map(|name| (name, HashSet::new()))
        .collect();
    for descriptor in registry.all_mode_descriptors() {
        modes
            .entry(descriptor.mode)
            .or_default()
            .extend(descriptor.message_types);
    }
    modes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::ModeDescriptor;

    const DECISION: &str = "macp.mode.decision.v1";

    fn registry() -> ModeRegistry {
        ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator))
    }

    #[test]
    fn registered_mode_and_declared_type_keep_their_values() {
        let r = registry();
        let c = ModeLabelCache::new();
        let l = c.labels(&r, DECISION, "Vote");
        assert_eq!(l.mode, ModeLabel::Registered(DECISION.into()));
        assert_eq!(l.message_type, "Vote");
        assert_eq!(l.mode.metric_value(), Some(DECISION));
        assert_eq!(l.mode.process_span_name(), format!("process {DECISION}"));
    }

    #[test]
    fn undeclared_type_under_a_registered_mode_is_other() {
        let r = registry();
        let l = ModeLabelCache::new().labels(&r, DECISION, "TaskAccept");
        assert_eq!(l.mode, ModeLabel::Registered(DECISION.into()));
        assert_eq!(l.message_type, OTHER);
    }

    /// No built-in mode lists `Progress`, yet a session-scoped Progress is
    /// valid: it keeps its type.
    #[test]
    fn session_scoped_progress_under_a_registered_mode_keeps_its_type() {
        let r = registry();
        let c = ModeLabelCache::new();
        for t in ["Progress", "Signal"] {
            let l = c.labels(&r, DECISION, t);
            assert_eq!(l.mode, ModeLabel::Registered(DECISION.into()));
            assert_eq!(l.message_type, t);
        }
    }

    #[test]
    fn unregistered_mode_is_other_with_every_type_other() {
        let r = registry();
        let c = ModeLabelCache::new();
        for t in ["SessionStart", "Vote", "Progress", "Signal"] {
            let l = c.labels(&r, "ext.bogus.v1", t);
            assert_eq!(l.mode, ModeLabel::Other, "{t}");
            assert_eq!(l.message_type, OTHER, "{t}");
        }
        assert_eq!(ModeLabel::Other.metric_value(), Some(OTHER));
        assert_eq!(ModeLabel::Other.process_span_name(), "process _OTHER");
    }

    #[test]
    fn empty_mode_has_no_mode_label_and_only_ambient_types() {
        let r = registry();
        let c = ModeLabelCache::new();
        let l = c.labels(&r, "", "Signal");
        assert_eq!(l.mode, ModeLabel::Empty);
        assert_eq!(l.message_type, "Signal");
        assert_eq!(l.mode.metric_value(), None);
        assert_eq!(l.mode.process_span_name(), "process");
        assert_eq!(c.labels(&r, "", "Progress").message_type, "Progress");
        assert_eq!(c.labels(&r, "", "Vote").message_type, OTHER);
    }

    /// The cache follows registry changes: a mode registered (or removed)
    /// after the first lookup is seen on the next one.
    #[test]
    fn cache_is_invalidated_by_registry_changes() {
        let r = registry();
        let c = ModeLabelCache::new();
        assert_eq!(c.labels(&r, "ext.custom.v1", "Ping").mode, ModeLabel::Other);
        r.register_extension(ModeDescriptor {
            mode: "ext.custom.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Custom".into(),
            message_types: vec!["SessionStart".into(), "Ping".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        })
        .unwrap();
        let l = c.labels(&r, "ext.custom.v1", "Ping");
        assert_eq!(l.mode, ModeLabel::Registered("ext.custom.v1".into()));
        assert_eq!(l.message_type, "Ping");
        r.unregister_extension("ext.custom.v1").unwrap();
        assert_eq!(c.labels(&r, "ext.custom.v1", "Ping").mode, ModeLabel::Other);
    }

    /// Many threads resolve labels while an extension registers: every
    /// result is one of the two valid answers (before or after the change),
    /// nobody blocks forever, and once the change has landed the next lookup
    /// sees it.
    #[test]
    fn concurrent_lookups_across_a_registry_change_are_all_valid() {
        let r = Arc::new(registry());
        let c = Arc::new(ModeLabelCache::new());
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let (r, c, barrier) = (Arc::clone(&r), Arc::clone(&c), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..2_000 {
                        let d = c.labels(&r, DECISION, "Vote");
                        assert_eq!(d.mode, ModeLabel::Registered(DECISION.into()));
                        assert_eq!(d.message_type, "Vote");
                        let x = c.labels(&r, "ext.race.v1", "Ping");
                        let before = x.mode == ModeLabel::Other && x.message_type == OTHER;
                        let after = x.mode == ModeLabel::Registered("ext.race.v1".into())
                            && x.message_type == "Ping";
                        assert!(before || after, "{x:?}");
                    }
                })
            })
            .collect();
        barrier.wait();
        r.register_extension(ModeDescriptor {
            mode: "ext.race.v1".into(),
            mode_version: "1.0.0".into(),
            title: "Race".into(),
            message_types: vec!["SessionStart".into(), "Ping".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        })
        .unwrap();
        for t in readers {
            t.join().unwrap();
        }
        let l = c.labels(&r, "ext.race.v1", "Ping");
        assert_eq!(l.mode, ModeLabel::Registered("ext.race.v1".into()));
    }
}
