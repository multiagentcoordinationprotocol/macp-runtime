use macp_core::clock::{Clock, SystemClock};
use std::sync::Arc;

use crate::error::MacpError;
use crate::extensions::ExtensionProviderRegistry;
use crate::log_store::{EntryKind, LogEntry, LogStore};
use crate::metrics::RuntimeMetrics;
use crate::mode_registry::ModeRegistry;
use crate::pb::{Envelope, ModeDescriptor};
use crate::policy::registry::PolicyRegistry;
use crate::policy::PolicyDefinition;
use crate::registry::SessionRegistry;
use crate::session::{
    validate_session_id_for_acceptance, Session, SessionState, MAX_SUSPENSION_CYCLES,
};
use crate::storage::StorageBackend;
use crate::stream_bus::SessionStreamBus;

#[derive(Debug)]
pub struct ProcessResult {
    pub session_state: SessionState,
    pub duplicate: bool,
}

#[derive(Clone, Debug)]
pub enum SessionLifecycleEvent {
    Created { session_id: String },
    Resolved { session_id: String },
    Expired { session_id: String },
    Suspended { session_id: String },
    Resumed { session_id: String },
    Cancelled { session_id: String },
}

/// Per-message audit verbosity: which accepted messages log at `info`
/// ("message accepted (audit)") rather than `debug`.
///
/// Operational configuration, deliberately **not** policy content. It was
/// once read from a `rules.audit.level` block on the bound policy, which
/// RFC-MACP-0012 §4's closed rule objects forbid, and a logging level is not
/// governance: two policies differing only in it would otherwise be distinct
/// under §8 equality.
///
/// From the environment ([`AuditConfig::from_env`]):
///
/// | Variable | Effect |
/// |---|---|
/// | `MACP_AUDIT_LEVEL` | `info` audits every session; anything else (default `debug`) audits none, except as below |
/// | `MACP_AUDIT_INFO_POLICIES` | comma-separated `policy_id`s whose sessions are audited at `info` |
/// | `MACP_AUDIT_INFO_MODES` | comma-separated mode names whose sessions are audited at `info` |
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditConfig {
    /// Audit every session at `info`.
    pub all: bool,
    /// Sessions bound to one of these `policy_version`s are audited at `info`.
    pub policies: std::collections::BTreeSet<String>,
    /// Sessions of one of these modes are audited at `info`.
    pub modes: std::collections::BTreeSet<String>,
}

impl AuditConfig {
    /// Read `MACP_AUDIT_LEVEL`, `MACP_AUDIT_INFO_POLICIES` and
    /// `MACP_AUDIT_INFO_MODES`.
    pub fn from_env() -> Self {
        Self::from_values(
            std::env::var("MACP_AUDIT_LEVEL").ok().as_deref(),
            std::env::var("MACP_AUDIT_INFO_POLICIES").ok().as_deref(),
            std::env::var("MACP_AUDIT_INFO_MODES").ok().as_deref(),
        )
    }

    /// The parse behind [`Self::from_env`], separated so it is testable
    /// without mutating the process environment.
    pub fn from_values(level: Option<&str>, policies: Option<&str>, modes: Option<&str>) -> Self {
        let list = |raw: Option<&str>| {
            raw.unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        };
        Self {
            all: level.map(str::trim) == Some("info"),
            policies: list(policies),
            modes: list(modes),
        }
    }

    /// Whether `session`'s accepted messages log at `info`.
    pub fn is_verbose(&self, session: &Session) -> bool {
        self.all
            || self.policies.contains(&session.policy_version)
            || self.modes.contains(&session.mode)
    }
}

pub struct Runtime {
    pub storage: Arc<dyn StorageBackend>,
    pub registry: Arc<SessionRegistry>,
    pub log_store: Arc<LogStore>,
    stream_bus: Arc<SessionStreamBus>,
    signal_bus: tokio::sync::broadcast::Sender<Envelope>,
    session_lifecycle_bus: tokio::sync::broadcast::Sender<SessionLifecycleEvent>,
    mode_registry: Arc<ModeRegistry>,
    policy_registry: Arc<PolicyRegistry>,
    #[allow(dead_code)] // plumbed for future session-extension providers; register API TBD
    extensions: Arc<ExtensionProviderRegistry>,
    metrics: Arc<RuntimeMetrics>,
    checkpoint_interval: usize,
    /// `MACP_COMPACT_TERMINAL_LOGS` (default on): replace a terminal
    /// session's log with one checkpoint. Off keeps the full history (a
    /// checkpoint is appended instead), so `WhatIfReplay` can replay it.
    compact_terminal_logs: bool,
    audit: AuditConfig,
    clock: Arc<dyn Clock>,
    receipts: Arc<crate::receipts::ReceiptIssuer>,
}

/// A `WhatIfReplay` result: the library report, plus each replay event's
/// 0-based accepted-history index (`SessionStart` = 0; `None` for a
/// runtime-internal entry, which takes no accepted ordinal).
#[derive(Debug)]
pub struct WhatIfRun {
    pub report: macp_modes::what_if::WhatIfReport,
    pub history_index: Vec<Option<u64>>,
}

/// What-if replay (RFC-MACP-0012 §8.1) of one session's log, with no
/// runtime: what [`Runtime::what_if_replay`] runs once it has the session and
/// its log, and what `macp replay` runs over a data directory or a log file.
///
/// `stored` is the session as the runtime holds it. When given, the recorded
/// half runs under its bound policy and must end in its state. Without it, the
/// mode and policy come from the log's `SessionStart` (the policy resolved in
/// `policy_registry`), and the recorded half only has to replay. Pass the
/// recorded policy as `candidate` to check a log's integrity alone.
pub fn what_if_over_log(
    session_id: &str,
    log: &[LogEntry],
    stored: Option<&Session>,
    mode_registry: &ModeRegistry,
    policy_registry: &PolicyRegistry,
    candidate: &PolicyDefinition,
) -> Result<WhatIfRun, WhatIfError> {
    policy_registry
        .validate_candidate(candidate)
        .map_err(WhatIfError::InvalidCandidate)?;
    let start_entry = log
        .iter()
        .find(|e| e.entry_kind == EntryKind::Incoming && e.message_type == "SessionStart");
    let mode = match (stored, start_entry) {
        (Some(s), _) => s.mode.clone(),
        (None, Some(e)) => e.mode.clone(),
        (None, None) => return Err(WhatIfError::Compacted),
    };
    if candidate.mode != mode && candidate.mode != "*" {
        return Err(WhatIfError::InvalidCandidate(format!(
            "INVALID_POLICY_DEFINITION: candidate mode '{}' does not govern session mode '{mode}'",
            candidate.mode
        )));
    }

    let history = crate::replay::recorded_history(session_id, &mode, log, mode_registry)
        .map_err(|e| WhatIfError::Integrity(format!("cannot rebuild the SessionStart: {e}")))?
        .ok_or(WhatIfError::Compacted)?;

    let start = history
        .start
        .with_policy(recorded_policy(log, stored, policy_registry));
    let report = macp_modes::what_if::replay_under_policy(
        &start,
        history.events,
        history.mode.as_ref(),
        &crate::replay::server_log_replay_options(),
        candidate,
    )
    .map_err(|e| WhatIfError::Integrity(format!("recorded replay failed: {e}")))?;
    if let Some(s) = stored {
        if report.recorded.state.as_ref() != Some(&s.state) {
            return Err(WhatIfError::Integrity(format!(
                "recorded replay ends {:?}, the stored session is {:?}",
                report.recorded.state, s.state
            )));
        }
    }
    Ok(WhatIfRun {
        report,
        history_index: history.history_index,
    })
}

/// The policy a session's recorded half replays under: the definition the
/// session bound at acceptance, not whatever the registry holds under that id
/// now. Without a stored session, the log's `SessionStart` names it (empty
/// means `policy.default`) and `policy_registry` resolves it; `None` when it
/// does not know it.
pub fn recorded_policy(
    log: &[LogEntry],
    stored: Option<&Session>,
    policy_registry: &PolicyRegistry,
) -> Option<PolicyDefinition> {
    if let Some(def) = stored.and_then(|s| s.policy_definition.clone()) {
        return Some(def);
    }
    let id = match stored {
        Some(s) => s.policy_version.clone(),
        None => log
            .iter()
            .find(|e| e.entry_kind == EntryKind::Incoming && e.message_type == "SessionStart")
            .and_then(|e| crate::session::parse_session_start_payload(&e.raw_payload).ok())
            .map(|p| p.policy_version)
            .unwrap_or_default(),
    };
    let id = if id.is_empty() {
        crate::policy::defaults::DEFAULT_POLICY_ID
    } else {
        id.as_str()
    };
    policy_registry.resolve(id).ok()
}

/// Why [`what_if_over_log`] produced no report.
#[derive(Debug)]
pub enum WhatIfError {
    /// No such session (or it was evicted from memory).
    UnknownSession,
    /// The candidate failed admission (`INVALID_POLICY_DEFINITION`).
    InvalidCandidate(String),
    /// The session's history was compacted away and cannot be replayed.
    Compacted,
    /// The recorded replay failed or did not reproduce the stored outcome
    /// (`INTERNAL_ERROR`).
    Integrity(String),
}

impl Runtime {
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        registry: Arc<SessionRegistry>,
        log_store: Arc<LogStore>,
    ) -> Self {
        Self::with_mode_registry(
            storage,
            registry,
            log_store,
            Arc::new(ModeRegistry::build_default(std::sync::Arc::new(
                macp_policy::DefaultPolicyEvaluator,
            ))),
        )
    }

    pub fn with_mode_registry(
        storage: Arc<dyn StorageBackend>,
        registry: Arc<SessionRegistry>,
        log_store: Arc<LogStore>,
        mode_registry: Arc<ModeRegistry>,
    ) -> Self {
        Self::with_registries(
            storage,
            registry,
            log_store,
            mode_registry,
            Arc::new(PolicyRegistry::new()),
        )
    }

    pub fn with_registries(
        storage: Arc<dyn StorageBackend>,
        registry: Arc<SessionRegistry>,
        log_store: Arc<LogStore>,
        mode_registry: Arc<ModeRegistry>,
        policy_registry: Arc<PolicyRegistry>,
    ) -> Self {
        let checkpoint_interval = std::env::var("MACP_CHECKPOINT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0); // 0 = disabled by default
        let compact_terminal_logs = !matches!(
            std::env::var("MACP_COMPACT_TERMINAL_LOGS").as_deref(),
            Ok("0") | Ok("false")
        );
        let (signal_tx, _) = tokio::sync::broadcast::channel(256);
        let (session_lifecycle_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            storage,
            registry,
            log_store,
            stream_bus: Arc::new(SessionStreamBus::default()),
            signal_bus: signal_tx,
            session_lifecycle_bus: session_lifecycle_tx,
            mode_registry,
            policy_registry,
            extensions: Arc::new(ExtensionProviderRegistry::new()),
            metrics: Arc::new(RuntimeMetrics::new()),
            checkpoint_interval,
            compact_terminal_logs,
            audit: AuditConfig::from_env(),
            clock: Arc::new(SystemClock),
            receipts: Arc::new(crate::receipts::ReceiptIssuer::new(
                crate::receipts::ReceiptConfig::default(),
            )),
        }
    }

    /// Replace the acceptance clock (default: [`SystemClock`]). Every
    /// time-dependent kernel decision -- TTL, the suspension cap, the
    /// implicit-accept deadline, cancel/suspend/resume stamps, cleanup, GC --
    /// reads this one clock, so injecting a [`macp_core::ManualClock`] makes
    /// the kernel fully deterministic (conformance fixtures' `accepted_at_ms`,
    /// simulation, tests). Production deployments keep the default.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The acceptance clock, in Unix milliseconds.
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Override `MACP_COMPACT_TERMINAL_LOGS` read at construction.
    pub fn with_compact_terminal_logs(mut self, compact: bool) -> Self {
        self.compact_terminal_logs = compact;
        self
    }

    /// Replace the decision-receipt configuration (default: unsigned, no
    /// published keys, issuer `runtime:macp-runtime`).
    pub fn with_receipt_config(mut self, config: crate::receipts::ReceiptConfig) -> Self {
        self.receipts = Arc::new(crate::receipts::ReceiptIssuer::new(config));
        self
    }

    /// The decision-receipt configuration in force.
    pub fn receipt_config(&self) -> &crate::receipts::ReceiptConfig {
        &self.receipts.config
    }

    /// The `GetReceiptKeys` JWKS text, rendered once at construction.
    pub fn receipt_keys_json(&self) -> &str {
        &self.receipts.jwks_json
    }

    /// Replace the audit verbosity read from the environment at construction.
    pub fn with_audit_config(mut self, audit: AuditConfig) -> Self {
        self.audit = audit;
        self
    }

    /// Returns all mode names the runtime can handle (standards-track + extensions).
    /// Used by Initialize and GetManifest to advertise full capability.
    pub fn registered_mode_names(&self) -> Vec<String> {
        self.mode_registry.all_mode_names()
    }

    /// Returns only standards-track mode descriptors for ListModes.
    pub fn standard_mode_descriptors(&self) -> Vec<ModeDescriptor> {
        self.mode_registry.standard_mode_descriptors()
    }

    /// Returns only extension mode descriptors for ListExtModes.
    pub fn extension_mode_descriptors(&self) -> Vec<ModeDescriptor> {
        self.mode_registry.extension_mode_descriptors()
    }

    pub fn register_extension(&self, descriptor: ModeDescriptor) -> Result<(), String> {
        self.mode_registry.register_extension(descriptor)
    }

    pub fn unregister_extension(&self, mode: &str) -> Result<(), String> {
        self.mode_registry.unregister_extension(mode)
    }

    pub fn promote_mode(&self, mode: &str, new_name: Option<&str>) -> Result<String, String> {
        self.mode_registry.promote_mode(mode, new_name)
    }

    pub fn subscribe_mode_changes(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.mode_registry.subscribe_changes()
    }

    pub fn mode_registry(&self) -> &Arc<ModeRegistry> {
        &self.mode_registry
    }

    // ── Policy registry delegation ──────────────────────────────────

    pub fn register_policy(&self, definition: PolicyDefinition) -> Result<(), String> {
        self.policy_registry.register(definition)
    }

    pub fn unregister_policy(&self, policy_id: &str) -> Result<(), String> {
        self.policy_registry.unregister(policy_id)
    }

    pub fn get_policy(&self, policy_id: &str) -> Option<PolicyDefinition> {
        self.policy_registry.get(policy_id)
    }

    pub fn list_policies(&self, mode_filter: Option<&str>) -> Vec<PolicyDefinition> {
        self.policy_registry.list(mode_filter)
    }

    pub fn subscribe_policy_changes(&self) -> tokio::sync::broadcast::Receiver<()> {
        self.policy_registry.subscribe_changes()
    }

    pub fn policy_registry(&self) -> &Arc<PolicyRegistry> {
        &self.policy_registry
    }

    pub fn metrics(&self) -> &Arc<RuntimeMetrics> {
        &self.metrics
    }

    pub fn subscribe_session_stream(
        &self,
        session_id: &str,
    ) -> tokio::sync::broadcast::Receiver<Envelope> {
        self.stream_bus.subscribe(session_id)
    }

    pub fn subscribe_signals(&self) -> tokio::sync::broadcast::Receiver<Envelope> {
        self.signal_bus.subscribe()
    }

    pub fn subscribe_session_lifecycle(
        &self,
    ) -> tokio::sync::broadcast::Receiver<SessionLifecycleEvent> {
        self.session_lifecycle_bus.subscribe()
    }

    /// RFC-MACP-0006 §3.2: Replay accepted envelopes from the session log for
    /// passive subscribe, strictly after `after_sequence` (1-based accepted
    /// ordinal, exclusive; 0 = from the start). `Err(base)` when the
    /// requested range was discarded by log compaction — the caller must
    /// surface an explicit error, not silently skip missing history.
    pub async fn get_session_envelopes_after(
        &self,
        session_id: &str,
        after_sequence: u64,
    ) -> Result<Vec<Envelope>, u64> {
        Ok(self
            .log_store
            .get_incoming_after(session_id, after_sequence)
            .await?
            .into_iter()
            .map(|(_idx, entry)| Envelope {
                macp_version: if entry.macp_version.is_empty() {
                    macp_core::MACP_VERSION.into()
                } else {
                    entry.macp_version
                },
                mode: entry.mode,
                message_type: entry.message_type,
                message_id: entry.message_id,
                session_id: entry.session_id,
                sender: entry.sender,
                timestamp_unix_ms: if entry.timestamp_unix_ms != 0 {
                    entry.timestamp_unix_ms
                } else {
                    entry.received_at_ms
                },
                payload: entry.raw_payload,
            })
            .collect())
    }

    fn publish_accepted_envelope(&self, env: &Envelope) {
        if !env.session_id.is_empty() {
            self.stream_bus.publish(&env.session_id, env.clone());
        }
    }

    fn make_incoming_entry(env: &Envelope, received_at_ms: i64) -> LogEntry {
        LogEntry {
            message_id: env.message_id.clone(),
            received_at_ms,
            sender: env.sender.clone(),
            message_type: env.message_type.clone(),
            raw_payload: env.payload.clone(),
            entry_kind: EntryKind::Incoming,
            session_id: env.session_id.clone(),
            mode: env.mode.clone(),
            macp_version: env.macp_version.clone(),
            timestamp_unix_ms: env.timestamp_unix_ms,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    /// Build a runtime-authored (`EntryKind::Internal`) log entry stamped with
    /// `at_ms`.
    ///
    /// The clock is **injected, never read here**, mirroring
    /// [`Self::make_incoming_entry`]'s `received_at_ms`. Replay reconstructs
    /// suspension state from these recorded stamps (`SessionSuspend` /
    /// `SessionResume` in `replay::replay_entry`), so the entry must carry the
    /// *same* instant the caller used to mutate the live `Session`. When this
    /// helper read `Utc::now()` itself, `suspend_session` / `resume_session`
    /// read the clock twice — once for `Session::suspend`/`resume`, once here —
    /// and a tick landing between the two reads made the live
    /// `accumulated_suspended_ms` differ from the replayed one by ~1 ms. That
    /// value gates the handoff implicit-accept decision, so a flip within 1 ms
    /// of the deadline could make a live-`Resolved` session fail replay
    /// entirely and be skipped at startup (`src/main.rs`, "failed to replay
    /// session; skipping"). One clock read per entry removes the whole class.
    ///
    /// # Ordinal and delivery contract
    ///
    /// Every entry this helper produces is `EntryKind::Internal`. Per
    /// RFC-MACP-0006 §3.2:117, `SessionSuspend`, `SessionResume`,
    /// `SessionCancel`, TTL expiry, and checkpoint entries are runtime
    /// bookkeeping, not accepted envelopes: they **consume no accepted
    /// ordinal** (`crates/macp-storage/src/log_store.rs`'s `get_incoming_after`
    /// filters to `EntryKind::Incoming` only) and, per §3.2:122, **MUST NOT be
    /// delivered on a subscribe stream** (`Runtime::publish_accepted_envelope`
    /// is never called from any of this helper's callers). This is the
    /// deliberate opposite of [`Self::synthesize_due_accept`], whose synthetic
    /// implicit-accept entry is `EntryKind::Incoming` and therefore does both —
    /// see that method's rustdoc for the contrast and the RFC-MACP-0010
    /// §5.1(2)/(3) argument for why it must.
    fn make_internal_entry(
        message_type: &str,
        payload: &[u8],
        session_id: &str,
        mode: &str,
        at_ms: i64,
    ) -> LogEntry {
        LogEntry {
            message_id: String::new(),
            received_at_ms: at_ms,
            sender: "_runtime".into(),
            message_type: message_type.into(),
            raw_payload: payload.to_vec(),
            entry_kind: EntryKind::Internal,
            session_id: session_id.into(),
            mode: mode.into(),
            macp_version: macp_core::MACP_VERSION.into(),
            timestamp_unix_ms: at_ms,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    async fn save_session_to_storage(&self, session: &Session) {
        if let Err(err) = self.storage.save_session(session).await {
            tracing::warn!(
                session_id = %session.session_id,
                error = %err,
                "failed to persist session snapshot"
            );
        }
    }

    async fn maybe_expire_session(
        &self,
        session_id: &str,
        session: &mut Session,
    ) -> Result<bool, MacpError> {
        let now = self.now_ms();
        // An Open session past its deadline, or a Suspended session that has
        // exceeded the MAX_SUSPEND_MS cap (RFC-MACP-0001 §7.5), expires. The
        // predicate is the one `macp_modes::step` uses, so the library and the
        // server cannot disagree on expiry.
        if session.deadline_passed(now) {
            let entry =
                Self::make_internal_entry("TtlExpired", b"", session_id, &session.mode, now);
            self.storage
                .append_log_entry(session_id, &entry)
                .await
                .map_err(|_| MacpError::StorageFailed)?;
            self.log_store.append(session_id, entry).await;
            session.expire()?;
            self.metrics.record_session_expired(&session.mode);
            tracing::info!(session_id, "session expired via TTL");
            let _ = self
                .session_lifecycle_bus
                .send(SessionLifecycleEvent::Expired {
                    session_id: session_id.to_string(),
                });
            return Ok(true);
        }
        Ok(false)
    }

    pub async fn process(
        &self,
        env: &Envelope,
        max_open_sessions: Option<usize>,
    ) -> Result<ProcessResult, MacpError> {
        match env.message_type.as_str() {
            "SessionStart" => self.process_session_start(env, max_open_sessions).await,
            "Signal" | "Progress" => self.process_signal(env).await,
            _ => self.process_message(env).await,
        }
    }

    async fn process_session_start(
        &self,
        env: &Envelope,
        max_open_sessions: Option<usize>,
    ) -> Result<ProcessResult, MacpError> {
        // The checks themselves live in `macp_modes::step` — the library
        // SessionStart acceptance path — so an embedder and this kernel run the
        // identical sequence. Only the registry lookups (mode, descriptor
        // version, strictness, policy) and the existing-session check are
        // interposed here, in the order they always ran: mode lookup before
        // payload validation, the duplicate check before policy resolution.
        if env.mode.trim().is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        validate_session_id_for_acceptance(&env.session_id)?;
        let mode_name = env.mode.as_str();
        let mode = self
            .mode_registry
            .get_mode(mode_name)
            .ok_or(MacpError::UnknownMode)?
            .instantiate()?;

        // `requires_strict_session_start` stays the source of *whether* the
        // canonical contract applies: it reads the registry's per-entry
        // `strict_session_start` flag, which `promote_mode` sets for modes the
        // core's static name list has never heard of. The `_for_mode` validator
        // decides only *which* roster rule applies within that contract, and
        // defaults to the strict one for any mode it does not recognise — so a
        // promoted mode keeps full canonical validation.
        //
        // The descriptor version: a payload naming another `mode_version` is
        // refused, and one omitting it (only possible for non-strict extension
        // modes) is bound to the descriptor's, so the Commitment version check
        // is never vacuous. The bound value is recorded on the SessionStart log
        // entry so replay uses the recorded binding, never the live registry.
        let mut opts = macp_modes::step::StartOptions::default()
            .with_strict(self.mode_registry.requires_strict_session_start(mode_name));
        if let Some(descriptor_version) = self.mode_registry.get_mode_version(mode_name) {
            opts = opts.with_descriptor_mode_version(descriptor_version);
        }
        // RFC-MACP-0003 §2: the TTL deadline is computed from the SessionStart
        // envelope's timestamp_unix_ms, not wall-clock time, falling back to
        // this acceptance clock when the envelope carries none. The resolved
        // suspension cap is bound and recorded likewise (RFC-MACP-0001 §7.5).
        let accepted_at = self.now_ms();
        let validated = macp_modes::step::validate_session_start(env, accepted_at, &opts)
            .inspect_err(|err| {
                if matches!(err, MacpError::InvalidEnvelope) {
                    tracing::debug!(
                        mode = mode_name,
                        "SessionStart refused: invalid envelope or mode_version mismatch"
                    );
                }
            })?;
        let bound_mode_version = validated.bound_mode_version.clone();
        let bound_max_suspend_ms = validated.max_suspend_ms;

        // Existing-session path: duplicate SessionStart handling. Take the
        // shared handle under a brief map read, then check dedup under the
        // session's own mutex (never await a session mutex while holding the
        // map lock).
        if let Some(existing) = self.registry.get_shared(&env.session_id).await {
            let existing = existing.lock().await;
            if existing.seen_message_ids.contains(&env.message_id) {
                return Ok(ProcessResult {
                    session_state: existing.state.clone(),
                    duplicate: true,
                });
            }
            return Err(MacpError::SessionAlreadyExists);
        }

        // RFC-MACP-0012 §6.1: policy_version is resolved at SessionStart; empty
        // resolves to "policy.default". The resolved definition is stored
        // immutably on the session for deterministic replay (RFC-MACP-0003 §3);
        // `accept` refuses a definition bound to another mode.
        let effective_policy_version = if validated.requested_policy_version().is_empty() {
            crate::policy::defaults::DEFAULT_POLICY_ID.to_string()
        } else {
            validated.requested_policy_version().to_string()
        };
        let policy_definition = self
            .policy_registry
            .resolve(&effective_policy_version)
            .map_err(|_| MacpError::UnknownPolicyVersion)?;

        // Builds the session at CURRENT_SEMANTICS_REV, runs `on_session_start`
        // and the client boundary (the reserved `message_id` namespace is
        // squattable through `SessionStart` too), applies the initial mode
        // state and consumes the start's dedup slot — all before the registry
        // reservation, so a rejection needs no rollback. Nothing is visible to
        // another task until the commit-point append below succeeds.
        let session = validated.accept(env, mode.as_ref(), Some(policy_definition))?;
        let semantics_rev = session.semantics_rev;

        // Reserve the session id atomically (dedup + max_open TOCTOU safety),
        // then do the storage I/O with the map lock RELEASED and only this
        // session's mutex held — a slow fsync on one SessionStart no longer
        // stalls every other session.
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(session));
        // Lock our own reservation BEFORE publishing it, so any concurrent
        // access to this session id blocks until start completes or rolls back.
        let mut session_guard = shared
            .clone()
            .try_lock_owned()
            .expect("freshly created mutex is uncontended");
        {
            let mut map = self.registry.sessions.write().await;
            if map.contains_key(&env.session_id) {
                // Lost a same-id race after the earlier existence check.
                return Err(MacpError::SessionAlreadyExists);
            }
            if let Some(max_open) = max_open_sessions {
                let now = self.now_ms();
                let mut count = 0usize;
                for arc in map.values() {
                    // Never await a session mutex under the map lock: a
                    // locked entry is in-flight and therefore Open —
                    // counting it is the conservative direction for a
                    // rate limit.
                    let counts = match arc.try_lock() {
                        Ok(s) => {
                            s.initiator_sender == env.sender
                                && s.state == SessionState::Open
                                && now <= s.ttl_expiry
                        }
                        Err(_) => true,
                    };
                    if counts {
                        count += 1;
                    }
                }
                if count >= max_open {
                    return Err(MacpError::RateLimited);
                }
            }
            map.insert(env.session_id.clone(), std::sync::Arc::clone(&shared));
        }

        // Roll back the reservation on any storage failure: poison the
        // placeholder (non-Open) BEFORE removing it so a waiter that already
        // cloned the Arc fails the OPEN gate instead of processing a message
        // for a session whose SessionStart never committed.
        let rollback = |runtime: &Self, session_guard: &mut Session| {
            session_guard.state = SessionState::Expired;
            let registry = std::sync::Arc::clone(&runtime.registry);
            let sid = env.session_id.clone();
            async move {
                let mut map = registry.sessions.write().await;
                map.remove(&sid);
            }
        };

        // 1. Create storage directory and write log entry (COMMIT POINT)
        if self
            .storage
            .create_session_storage(&env.session_id)
            .await
            .is_err()
        {
            rollback(self, &mut session_guard).await;
            return Err(MacpError::StorageFailed);
        }
        let mut incoming_entry = Self::make_incoming_entry(env, accepted_at);
        incoming_entry.bound_mode_version = bound_mode_version;
        incoming_entry.semantics_rev = semantics_rev;
        incoming_entry.bound_max_suspend_ms = Some(bound_max_suspend_ms);
        if self
            .storage
            .append_log_entry(&env.session_id, &incoming_entry)
            .await
            .is_err()
        {
            rollback(self, &mut session_guard).await;
            return Err(MacpError::StorageFailed);
        }

        // 2. Update in-memory caches
        self.log_store.create_session_log(&env.session_id).await;
        self.log_store.append(&env.session_id, incoming_entry).await;

        let result_state = session_guard.state.clone();
        // 3. Session snapshot — best-effort AFTER the durable append. The log
        // entry above is the COMMIT POINT: once it is durable, the session
        // exists and replay reconstructs it, so a snapshot failure must NOT
        // fail (or roll back) the start. The previous fatal+rollback here was
        // incoherent past the commit point — it could not un-append the
        // durable SessionStart, so the "failed" session resurrected on
        // restart, and a same-id client retry appended a SECOND SessionStart
        // that made the log unreplayable.
        if let Err(err) = self.storage.save_session(&session_guard).await {
            tracing::warn!(
                session_id = %session_guard.session_id,
                error = %err,
                "failed to persist session snapshot at SessionStart (recoverable via replay)"
            );
        }
        self.metrics.record_session_start(mode_name);
        tracing::info!(
            session_id = %env.session_id,
            mode = mode_name,
            sender = %env.sender,
            "session started"
        );
        // Publish while still holding the session mutex — publish order must
        // equal acceptance order (process_message publishes under the mutex
        // too). Publishing after the drop let a subscriber observe a later
        // message's broadcast BEFORE this SessionStart's, breaking the FIFO
        // premise the subscribe-window dedupe relies on.
        self.publish_accepted_envelope(env);
        drop(session_guard);
        let _ = self
            .session_lifecycle_bus
            .send(SessionLifecycleEvent::Created {
                session_id: env.session_id.clone(),
            });

        Ok(ProcessResult {
            session_state: result_state,
            duplicate: false,
        })
    }

    /// Emit the mode's due synthetic envelope, if one is due, into accepted
    /// history — the kernel half of [`macp_modes::mode::Mode::due_synthetic_envelope`]'s
    /// contract (RFC-MACP-0010 §5.1(2) for handoff's implicit accept).
    ///
    /// Called from `process_message` *before* the triggering message is
    /// dispatched, sharing the trigger's single clock read. The second caller
    /// is [`Runtime::sweep_due_synthetic_accepts`], the eager sweep driven by
    /// the background maintenance loop in `src/main.rs` (alongside
    /// `cleanup_expired_sessions`, `evict_stale_sessions` and
    /// `gc_disk_sessions`): it calls this for every open session so an offer is
    /// settled on time rather than only when the next message happens to
    /// arrive. Both callers must hold the session mutex.
    ///
    /// Returns `true` when an entry was appended — the sweep's emission count,
    /// and the only honest one: every skip below is a silent `Ok`.
    ///
    /// # The non-`Open` filter is a correctness gate, not hygiene
    ///
    /// Both computations feeding the synthetic entry deliberately ignore an
    /// *in-flight* pause: `Session::unsuspended_deadline` walks completed
    /// intervals only, and `rev2_elapsed_ms` has no in-flight term. Asking a
    /// `Suspended` session therefore over-counts elapsed time (the accept can
    /// be judged due when it is not) and under-computes `D` (a wrong
    /// `timestamp_unix_ms` baked into permanent history). The mode carries a
    /// `debug_assert!` for this, which compiles out in release builds; this
    /// filter is the enforcement that ships.
    ///
    /// # Clocks
    ///
    /// The entry is dispatched *and* stamped with the envelope's own
    /// `timestamp_unix_ms` (the computed deadline `D`), never wall-clock:
    /// `received_at_ms == timestamp_unix_ms == D`. Replay derives its dispatch
    /// clock from `received_at_ms`, so any other value forks the live session
    /// from what the log rebuilds.
    ///
    /// # `step::commit_synthetic`, not `step::commit`
    ///
    /// The commit here is `seen_message_ids` + `apply_mode_response` only.
    /// `step::commit` would also call `record_participant_activity`, which
    /// replay never does for any entry kind — so calling it live would
    /// guarantee a live/replay divergence in `participant_message_counts` /
    /// `participant_last_seen` — and crediting the target with activity they
    /// did not perform would be false besides. The consequence is deliberate
    /// and documented: the target's `message_count` in
    /// `SessionMetadata.participant_activity` does not include the synthetic
    /// accept.
    ///
    /// # The save is load-bearing
    ///
    /// `save_session_to_storage` is called *here*, not left to the caller: the
    /// trigger's own save is downstream of its `on_message_at`, which returns
    /// early when the mode rejects the trigger. Without this call, a synthesis
    /// followed by a rejected trigger would leave the in-memory session
    /// carrying new `mode_state` and a new dedup id that the on-disk snapshot
    /// lacks — and `replay::validate_replay_consistency` would warn about it on
    /// the next startup. The in-tree precedent is the `Precheck::Expired` arm
    /// below, which saves before returning `Err` for the same reason.
    ///
    /// # Freeze-profile carve-out
    ///
    /// The tracked invariant "rejected messages don't consume dedup slots or
    /// mutate history" (`CONTRIBUTING.md`) is amended, not broken, by this
    /// method: a *rejected* trigger can now leave a runtime-originated entry in
    /// accepted history. Three arguments, recorded here because this is what
    /// stops the next reader from reverting it:
    ///
    /// 1. **The precedent is already shipped.** `Precheck::Expired` does all of
    ///    this on a message it then rejects — `maybe_expire_session` appends a
    ///    durable `TtlExpired` entry and mutates `session.state`, then
    ///    `process_message` saves the snapshot and returns `Err`. The honest
    ///    delta is only that this observation lands in **accepted** history
    ///    (`EntryKind::Incoming`, so it consumes an accepted ordinal) and is
    ///    **published to `StreamSession`** subscribers; `TtlExpired` is
    ///    `EntryKind::Internal` and does neither.
    /// 2. **The "conservative" alternative is the non-conformant one.**
    ///    Synthesizing only for *accepted* triggers inverts RFC-MACP-0010
    ///    §5.1(4): a late explicit `HandoffAccept` would then be validated
    ///    against a still-unaccepted offer, pass, be accepted — and the
    ///    synthetic would never be emitted at all. §5.1(2) requires the
    ///    synthetic in history *before* any subsequent message is evaluated
    ///    against the offer's acceptance state.
    /// 3. **The dedup half is preserved exactly.** The rejected trigger's own
    ///    `message_id` is never inserted — only the synthetic's deterministic
    ///    id is — so re-sending a corrected message with that same id is still
    ///    accepted. No existing dedup-invariant test needed weakening.
    ///
    /// # Contrast with runtime-internal entries
    ///
    /// This is the one place the runtime originates an `EntryKind::Incoming`
    /// entry rather than an `EntryKind::Internal` one (see
    /// [`Self::make_internal_entry`]). The difference is not accidental: the
    /// synthetic implicit accept is a *mode message* with a sender the RFC
    /// pins (RFC-MACP-0010 §5.1(3)), so it consumes an accepted ordinal and is
    /// published to `StreamSession` subscribers like any other accepted
    /// envelope — whereas `SessionSuspend`/`SessionResume`/`SessionCancel`/TTL
    /// expiry/checkpoint entries are bookkeeping RFC-MACP-0006 §3.2:117/:122
    /// require be neither counted nor delivered.
    async fn synthesize_due_accept(
        &self,
        session_id: &str,
        session: &mut Session,
        now_ms: i64,
    ) -> Result<bool, MacpError> {
        // A mode that vanished from the registry has nothing due.
        let Some(mode) = self
            .mode_registry
            .get_mode(&session.mode)
            .and_then(|m| m.instantiate().ok())
        else {
            return Ok(false);
        };
        // `next_synthetic` is the shared, pure half (`macp_modes::step`): the
        // non-`Open` filter, the mode's `due_synthetic_envelope`, the dedup
        // backstop, `authorize_sender` (mirroring replay, which authorizes every
        // `Incoming` entry), and dispatch at the envelope's own timestamp. A
        // step-only embedder runs the identical sequence through
        // `step::advance`, so the two cannot drift.
        let Some(syn) = macp_modes::step::next_synthetic(session, mode.as_ref(), now_ms)? else {
            return Ok(false);
        };

        // COMMIT POINT: nothing above has mutated the session, so a failed
        // append rejects the *triggering* message with `StorageFailed` before
        // it consumed a dedup slot. RFC-MACP-0010 §5.1(2) forbids evaluating
        // that message without the accept in history, and this runtime never
        // acknowledges what it could not persist.
        let entry = Self::make_incoming_entry(&syn.envelope, syn.envelope.timestamp_unix_ms);
        self.storage
            .append_log_entry(session_id, &entry)
            .await
            .map_err(|_| MacpError::StorageFailed)?;
        self.log_store.append(session_id, entry).await;

        let syn = macp_modes::step::commit_synthetic(session, syn);
        self.metrics.record_message_accepted(&session.mode);

        tracing::info!(
            session_id = %session_id,
            message_type = %syn.message_type,
            message_id = %syn.message_id,
            sender = %syn.sender,
            deadline_ms = syn.timestamp_unix_ms,
            "synthetic envelope appended to accepted history"
        );

        self.save_session_to_storage(session).await;
        // The checkpoint-interval check belongs to *the append*, not to the
        // caller. It lives inside this seam rather than in either caller so the
        // two paths cannot drift: a synthetic entry advances `log_len` exactly
        // the same way from `process_message` and from
        // `sweep_due_synthetic_accepts`, so it must cross an interval boundary
        // the same way too. Put it in the sweep instead and the eager path
        // would silently skip every boundary a synthetic crossed — checkpoints
        // are only a replay optimization, but the divergence would be real and
        // invisible.
        //
        // No double-checkpoint on the lazy path. `process_message` runs its own
        // `maybe_insert_checkpoint` after appending the triggering message, by
        // which point the log is at least one entry longer than it is here, so
        // the two calls never test the same `log_len`. The check is a pure
        // function of that length, so re-asking at a different length is not a
        // repeat.
        self.maybe_insert_checkpoint(session_id, session).await;
        self.publish_accepted_envelope(&syn);
        Ok(true)
    }

    /// Process a session-scoped message following the RFC-MACP-0001 Section 7.3
    /// terminal-state transition order:
    /// 1. Check session OPEN
    /// 2. Validate message (mode.authorize_sender + mode.on_message)
    /// 3. Accept into history (log_store.append)
    /// 4. Transition to RESOLVED (session.apply_mode_response)
    /// 5. Reject subsequent messages (enforced by step 1 on next call)
    async fn process_message(&self, env: &Envelope) -> Result<ProcessResult, MacpError> {
        // Per-session serialization (RFC-0001 §8.1): clone the shared handle
        // under a brief map read, then hold ONLY this session's mutex across
        // validate + append (fsync) + commit. Different sessions' appends
        // proceed in parallel; the same session's appends stay strictly
        // ordered (which also keeps RocksDB's per-session next_seq
        // read-modify-write safe).
        let shared = self
            .registry
            .get_shared(&env.session_id)
            .await
            .ok_or(MacpError::UnknownSession)?;
        let mut session_guard = shared.lock().await;
        let session = &mut *session_guard;

        // Per-message kernel invariants (dedup, mode-binding, TTL, the monotonic
        // OPEN gate) live in `macp_modes::step` so any consumer of the
        // coordination core runs the identical checks. The runtime is the first
        // caller: it drives the phases here so it can interpose its append-only
        // write between validation and commit (a failed write must not consume a
        // dedup slot) — which a single all-in-one step could not preserve.
        // One acceptance clock for the whole message: the prechecks, the mode
        // call, the log entry and the commit all observe the same instant, so
        // a message can never pass the TTL gate at one time and be stamped
        // past its deadline at another -- and replay, which re-reads
        // received_at_ms, observes the identical time.
        let now_ms = self.now_ms();
        match macp_modes::step::check_preconditions(session, env, now_ms)? {
            macp_modes::step::Precheck::Duplicate => {
                return Ok(ProcessResult {
                    session_state: session.state.clone(),
                    duplicate: true,
                });
            }
            macp_modes::step::Precheck::Expired => {
                // Durable expiry via the existing path: it appends the
                // `TtlExpired` log entry, updates metrics/lifecycle, and marks
                // the session Expired. `check_preconditions` and
                // `maybe_expire_session` share one predicate,
                // `Session::deadline_passed` (TTL for Open, the suspension cap
                // for Suspended), so this always expires.
                let expired = self.maybe_expire_session(&env.session_id, session).await?;
                debug_assert!(expired, "check_preconditions reported Expired");
                self.save_session_to_storage(session).await;
                return Err(MacpError::TtlExpired);
            }
            macp_modes::step::Precheck::Proceed => {}
        }

        let mode = self
            .mode_registry
            .get_mode(&session.mode)
            .ok_or(MacpError::UnknownMode)?
            .instantiate()?;
        // Sender authorization, then the client boundary (RFC-MACP-0010
        // §5.1(3)): shapes that are legal as recorded history but illegal as
        // client submissions. `step::admit` runs both, in that order, so the
        // Forbidden-before-InvalidPayload ordering is unchanged. NEVER on
        // replay — replay re-reads entries that already passed this check when
        // they were first accepted, and a runtime-synthesized envelope is not a
        // client submission either. The runtime drives the phases rather than
        // `step::step` so it can interpose the durable append between
        // validation and commit.
        macp_modes::step::admit(session, env, mode.as_ref())?;
        let accepted_at_ms = now_ms;
        // RFC-MACP-0010 §5.1(2): anything the mode says is already due MUST
        // enter accepted history BEFORE this message is evaluated against the
        // state it changes. Lazy emission — the same clock reading the trigger
        // is about to be dispatched with. Deliberately after the prechecks (a
        // duplicate, TTL-expired, suspended or unauthorized trigger
        // synthesizes nothing) and after the client boundary, so a forged
        // envelope can never provoke an append.
        self.synthesize_due_accept(&env.session_id, session, accepted_at_ms)
            .await?;
        let response = macp_modes::step::dispatch(session, env, mode.as_ref(), accepted_at_ms)?;

        // 1. COMMIT POINT: write log entry to disk
        let incoming_entry = Self::make_incoming_entry(env, accepted_at_ms);
        self.storage
            .append_log_entry(&env.session_id, &incoming_entry)
            .await
            .map_err(|_| MacpError::StorageFailed)?;

        // 2. Update in-memory state via the shared commit phase (consume dedup
        //    slot, record participant activity, apply mode response) — the exact
        //    sequence a library consumer runs through `macp_modes::step`.
        self.log_store.append(&env.session_id, incoming_entry).await;
        let result_state = macp_modes::step::commit(session, env, response, now_ms);

        self.metrics.record_message_accepted(&session.mode);
        if env.message_type == "Commitment" {
            self.metrics.record_commitment_accepted(&session.mode);
        }

        // Audit verbosity is runtime configuration (`AuditConfig`), not
        // policy content; default stays debug.
        if self.audit.is_verbose(session) {
            tracing::info!(
                session_id = %env.session_id,
                message_type = %env.message_type,
                sender = %env.sender,
                state = ?result_state,
                "message accepted (audit)"
            );
        } else {
            tracing::debug!(
                session_id = %env.session_id,
                message_type = %env.message_type,
                sender = %env.sender,
                state = ?result_state,
                "message accepted"
            );
        }

        if result_state == SessionState::Resolved {
            self.metrics.record_session_resolved(&session.mode);
            tracing::info!(session_id = %env.session_id, mode = %session.mode, "session resolved");
            let _ = self
                .session_lifecycle_bus
                .send(SessionLifecycleEvent::Resolved {
                    session_id: env.session_id.clone(),
                });
        }

        // 3. Best-effort session save + checkpoint
        self.save_session_to_storage(session).await;
        if result_state == SessionState::Resolved {
            if !self.maybe_compact_log(&env.session_id, session).await {
                self.force_insert_checkpoint(&env.session_id, session).await;
            }
        } else {
            self.maybe_insert_checkpoint(&env.session_id, session).await;
        }
        self.publish_accepted_envelope(env);

        Ok(ProcessResult {
            session_state: result_state,
            duplicate: false,
        })
    }

    /// Process a Signal or Progress envelope. Signals are informational out-of-band
    /// notifications. Progress messages carry structured ProgressPayload.
    /// Neither mutates session state — both are broadcast to subscribers.
    async fn process_signal(&self, env: &Envelope) -> Result<ProcessResult, MacpError> {
        // RFC-MACP-0001 §4 / RFC-MACP-0010: validate SignalPayload structure.
        // signal_type must be non-empty when a payload is present.
        if env.message_type == "Signal" && !env.payload.is_empty() {
            let signal: crate::pb::SignalPayload =
                prost::Message::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
            if signal.signal_type.trim().is_empty() {
                return Err(MacpError::InvalidPayload);
            }
        }
        // RFC-MACP-0001: validate ProgressPayload structure for Progress messages.
        if env.message_type == "Progress" && !env.payload.is_empty() {
            let _: crate::pb::ProgressPayload =
                prost::Message::decode(&*env.payload).map_err(|_| MacpError::InvalidPayload)?;
        }
        tracing::debug!(
            sender = %env.sender,
            message_id = %env.message_id,
            message_type = %env.message_type,
            "signal received"
        );
        let _ = self.signal_bus.send(env.clone());
        Ok(ProcessResult {
            session_state: SessionState::Open,
            duplicate: false,
        })
    }

    pub async fn get_session_checked(&self, session_id: &str) -> Option<Session> {
        let shared = self.registry.get_shared(session_id).await?;
        let mut session = shared.lock().await;
        let changed = self
            .maybe_expire_session(session_id, &mut session)
            .await
            .unwrap_or(false);
        if changed {
            self.save_session_to_storage(&session).await;
        }
        Some(session.clone())
    }

    /// A session for a read-only RPC: the registry's copy (expiring it if
    /// due, as [`Self::get_session_checked`] does), else the stored snapshot
    /// of a session evicted from memory. The stored one is not re-inserted.
    pub async fn session_for_read(&self, session_id: &str) -> Result<Session, MacpError> {
        if let Some(session) = self.get_session_checked(session_id).await {
            return Ok(session);
        }
        match self.storage.load_session(session_id).await {
            Ok(Some(session)) => Ok(session),
            Ok(None) => Err(MacpError::UnknownSession),
            Err(e) => {
                tracing::warn!(session_id, error = %e, "cannot load an evicted session");
                Err(MacpError::StorageFailed)
            }
        }
    }

    /// The decision receipt for `session_id` (RFC-MACP-0013 §13): the stored
    /// one when there is one, else issued now and persisted first-writer-wins.
    /// Every call for a session serves the same bytes. `ReceiptUnavailable`
    /// when the session is not eligible; `StorageFailed` when storage fails.
    pub async fn decision_receipt(&self, session_id: &str) -> Result<Arc<Vec<u8>>, MacpError> {
        use crate::storage::ReceiptWrite;
        use macp_core::receipt::{build, ReceiptInputs};

        if let Some(bytes) = self.receipts.cached(session_id).await {
            return Ok(bytes);
        }
        let _issuing = self.receipts.stripe(session_id).lock().await;
        if let Some(bytes) = self.receipts.cached(session_id).await {
            return Ok(bytes);
        }
        // Read under the lock: disk GC deletes under it too, so a session
        // seen here is not deleted before its receipt is stored.
        let in_memory = self.registry.get_shared(session_id).await.is_some();
        let session = self.session_for_read(session_id).await?;
        // Cache only sessions the registry holds, so eviction bounds the cache.
        let keep = |bytes: Vec<u8>| async move {
            if in_memory {
                self.receipts.cache(session_id, bytes).await
            } else {
                Arc::new(bytes)
            }
        };
        let failed = |e: std::io::Error| {
            tracing::warn!(session_id, error = %e, "decision receipt: storage failed");
            self.metrics.record_receipt_failure();
            MacpError::StorageFailed
        };
        match self.storage.load_receipt(session_id).await {
            Ok(Some(bytes)) => return Ok(keep(bytes).await),
            Ok(None) => {}
            Err(e) => return Err(failed(e)),
        }

        let wire = crate::receipts::receipt_source(&session)?;
        let issued_at = crate::receipts::rfc3339_seconds(self.now_ms());
        let config = &self.receipts.config;
        let inputs =
            ReceiptInputs::new(session_id, &session.mode, wire, config.issuer(), &issued_at)
                .semantics_rev(crate::receipts::IMPLEMENTATION, session.semantics_rev);
        let bytes = build(&inputs, config.signer()).map_err(|e| {
            tracing::warn!(session_id, error = %e, "decision receipt: not issuable");
            self.metrics.record_receipt_failure();
            MacpError::ReceiptUnavailable
        })?;
        let signed = config.signed();
        match self
            .storage
            .store_receipt_if_absent(session_id, &bytes)
            .await
        {
            Ok(ReceiptWrite::Stored) => {
                self.metrics.record_receipt_issued(signed);
                tracing::info!(session_id, signed, "decision receipt issued");
                Ok(keep(bytes).await)
            }
            Ok(ReceiptWrite::Existing(stored)) => Ok(keep(stored).await),
            Ok(_) => Err(failed(std::io::Error::other(
                "unknown receipt write outcome",
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                // Not persistable here: the in-memory copy is the receipt.
                let kept = self.receipts.cache(session_id, bytes.clone()).await;
                if *kept == bytes {
                    self.metrics.record_receipt_issued(signed);
                    tracing::info!(
                        session_id,
                        signed,
                        durable = false,
                        "decision receipt issued"
                    );
                }
                Ok(kept)
            }
            Err(e) => Err(failed(e)),
        }
    }

    /// RFC-MACP-0012 §8.1 what-if replay: replay the session's accepted
    /// history under its bound policy and under `candidate`, and report the
    /// difference. Read-only: no envelope is emitted, and the session, its
    /// log, dedup state and the policy registry are untouched. The caller
    /// authorizes the read (as for `GetSession`).
    ///
    /// The candidate passes `RegisterPolicy` admission (its id excepted) and
    /// must govern the session's mode or `"*"`. The recorded half must
    /// reproduce the stored session state, or the result is an integrity
    /// failure.
    pub async fn what_if_replay(
        &self,
        session_id: &str,
        candidate: &PolicyDefinition,
    ) -> Result<WhatIfRun, WhatIfError> {
        let session = self
            .get_session_checked(session_id)
            .await
            .ok_or(WhatIfError::UnknownSession)?;
        let log =
            match self.log_store.get_log(session_id).await {
                Some(log) => log,
                None => self.storage.load_log(session_id).await.map_err(|e| {
                    WhatIfError::Integrity(format!("cannot read the session log: {e}"))
                })?,
            };
        what_if_over_log(
            session_id,
            &log,
            Some(&session),
            &self.mode_registry,
            &self.policy_registry,
            candidate,
        )
    }

    /// Cancel a session. The `cancelled_by` parameter MUST be the authenticated
    /// sender of the CancelSession RPC (RFC-MACP-0001 Section 7.3: CancelSession
    /// is a Core control-plane message; mode authorization does not apply).
    pub async fn cancel_session(
        &self,
        session_id: &str,
        reason: &str,
        cancelled_by: &str,
    ) -> Result<ProcessResult, MacpError> {
        let shared = self
            .registry
            .get_shared(session_id)
            .await
            .ok_or(MacpError::UnknownSession)?;
        let mut session_guard = shared.lock().await;
        let session = &mut *session_guard;

        self.maybe_expire_session(session_id, session).await?;

        // Already terminal (Resolved/Expired/Cancelled): nothing to do. An Open
        // or Suspended session can still be cancelled (RFC-MACP-0001 §7.2/§7.3).
        if session.state.is_terminal() {
            let result_state = session.state.clone();
            self.save_session_to_storage(session).await;
            return Ok(ProcessResult {
                session_state: result_state,
                duplicate: false,
            });
        }

        // RFC-MACP-0001: runtime encodes a proper SessionCancelPayload with
        // `cancelled_by` set to the authenticated sender identity.
        let now_ms = self.now_ms();
        let cancel_payload = crate::pb::SessionCancelPayload {
            reason: reason.to_string(),
            cancelled_by: cancelled_by.to_string(),
        };
        // Internal, non-ordinal-consuming, not delivered — see
        // `make_internal_entry`'s rustdoc for the RFC-MACP-0006 §3.2 contract.
        let cancel_entry = Self::make_internal_entry(
            "SessionCancel",
            &prost::Message::encode_to_vec(&cancel_payload),
            session_id,
            &session.mode,
            now_ms,
        );
        self.storage
            .append_log_entry(session_id, &cancel_entry)
            .await
            .map_err(|_| MacpError::StorageFailed)?;
        self.log_store.append(session_id, cancel_entry).await;
        // RFC-MACP-0001 §7.3: cancellation terminates as CANCELLED (distinct
        // from EXPIRED) — `cancel()` also clears any suspension marker.
        let _ = session.cancel();
        self.save_session_to_storage(session).await;
        if !self.maybe_compact_log(session_id, session).await {
            self.force_insert_checkpoint(session_id, session).await;
        }
        self.metrics.record_session_cancelled(&session.mode);
        tracing::info!(session_id, reason, "session cancelled");
        let _ = self
            .session_lifecycle_bus
            .send(SessionLifecycleEvent::Cancelled {
                session_id: session_id.to_string(),
            });

        Ok(ProcessResult {
            session_state: SessionState::Cancelled,
            duplicate: false,
        })
    }

    /// Suspend an `Open` session (RFC-MACP-0001 §7.5). Appends a `SessionSuspend`
    /// annotation, transitions Open -> Suspended, and emits a lifecycle event.
    /// The session's TTL is banked and restored on resume.
    pub async fn suspend_session(
        &self,
        session_id: &str,
        reason: &str,
        suspended_by: &str,
    ) -> Result<ProcessResult, MacpError> {
        let shared = self
            .registry
            .get_shared(session_id)
            .await
            .ok_or(MacpError::UnknownSession)?;
        let mut session_guard = shared.lock().await;
        let session = &mut *session_guard;

        self.maybe_expire_session(session_id, session).await?;
        if session.state != SessionState::Open {
            return Err(MacpError::SessionNotOpen);
        }

        let now_ms = self.now_ms();
        let payload = crate::pb::SessionSuspendPayload {
            reason: reason.to_string(),
            suspended_by: suspended_by.to_string(),
        };
        // Internal, non-ordinal-consuming, not delivered — see
        // `make_internal_entry`'s rustdoc for the RFC-MACP-0006 §3.2 contract.
        let entry = Self::make_internal_entry(
            "SessionSuspend",
            &prost::Message::encode_to_vec(&payload),
            session_id,
            &session.mode,
            now_ms,
        );
        self.storage
            .append_log_entry(session_id, &entry)
            .await
            .map_err(|_| MacpError::StorageFailed)?;
        self.log_store.append(session_id, entry).await;
        session.suspend(now_ms)?;
        self.save_session_to_storage(session).await;
        self.metrics.record_session_suspended(&session.mode);
        tracing::info!(session_id, reason, "session suspended");
        let _ = self
            .session_lifecycle_bus
            .send(SessionLifecycleEvent::Suspended {
                session_id: session_id.to_string(),
            });

        Ok(ProcessResult {
            session_state: SessionState::Suspended,
            duplicate: false,
        })
    }

    /// Resume a `Suspended` session (RFC-MACP-0001 §7.5), banking the suspended
    /// duration into the TTL deadline. If the `MAX_SUSPEND_MS` cap is exceeded,
    /// the session is force-expired instead.
    pub async fn resume_session(
        &self,
        session_id: &str,
        reason: &str,
        resumed_by: &str,
    ) -> Result<ProcessResult, MacpError> {
        let shared = self
            .registry
            .get_shared(session_id)
            .await
            .ok_or(MacpError::UnknownSession)?;
        let mut session_guard = shared.lock().await;
        let session = &mut *session_guard;

        if session.state != SessionState::Suspended {
            return Err(MacpError::SessionNotOpen);
        }

        let now_ms = self.now_ms();
        // `banked_ms` on the wire payload records the remaining TTL banked at
        // suspend — `deadline − t_s` (RFC-MACP-0001 §7.5, RFC-MACP-0003 §2) —
        // computed from `session.ttl_expiry` and `session.suspended_at_ms`
        // *before* `session.resume` below mutates either. `Session::suspend`
        // (crates/macp-core/src/session.rs) never touches `ttl_expiry`, so at
        // this point it still holds the pre-suspension deadline: exactly the
        // spec's `deadline`. `now_ms` is deliberately not an input to this
        // value — do not reintroduce it here.
        //
        // The field is informational only: replay ignores it and re-derives
        // the banked duration from the two entries' recorded timestamps (see
        // the `SessionSuspend`/`SessionResume` arms of `replay::replay_entry`)
        // rather than trusting this value — RFC-MACP-0003 §2's own determinism
        // argument rests on those timestamps, not on `banked_ms`. Keep it that
        // way: consuming the field would make logs written before the change
        // below replay to a different deadline than they do today, since they
        // recorded a different quantity under this same field name (next
        // paragraph).
        //
        // A resume that force-expires the session (the cap-exceeded arm below)
        // still appends this entry carrying this value: the payload is built
        // and the entry appended before `session.resume` runs, and the
        // recorded value is never applied to a deadline the session goes on to
        // have.
        //
        // Entries written before the commit that introduced this expression
        // (`git log -S banked_ms -- src/runtime.rs`, or the commit that added
        // this comment) recorded the pause's *duration* (`t_r − t_s`) under
        // this same field name instead — with no discriminator between the
        // two quantities. That is a pre-existing, accepted divergence (see
        // docs/deployment.md's `log.jsonl` audit note), not something
        // detectable from the field alone.
        let banked_ms = session
            .suspended_at_ms
            .map(|suspended_at| session.ttl_expiry.saturating_sub(suspended_at).max(0))
            .unwrap_or(0);
        let payload = crate::pb::SessionResumePayload {
            reason: reason.to_string(),
            resumed_by: resumed_by.to_string(),
            banked_ms,
        };
        // Internal, non-ordinal-consuming, not delivered — see
        // `make_internal_entry`'s rustdoc for the RFC-MACP-0006 §3.2 contract.
        let entry = Self::make_internal_entry(
            "SessionResume",
            &prost::Message::encode_to_vec(&payload),
            session_id,
            &session.mode,
            now_ms,
        );
        self.storage
            .append_log_entry(session_id, &entry)
            .await
            .map_err(|_| MacpError::StorageFailed)?;
        self.log_store.append(session_id, entry).await;

        // `resume` banks the TTL; if the suspend cap is exceeded it force-expires.
        match session.resume(now_ms) {
            Ok(()) => {
                self.save_session_to_storage(session).await;
                self.metrics.record_session_resumed(&session.mode);
                tracing::info!(session_id, reason, "session resumed");
                let _ = self
                    .session_lifecycle_bus
                    .send(SessionLifecycleEvent::Resumed {
                        session_id: session_id.to_string(),
                    });
                Ok(ProcessResult {
                    session_state: SessionState::Open,
                    duplicate: false,
                })
            }
            Err(_) => {
                // A suspension cap was exceeded — either MAX_SUSPEND_MS
                // (cumulative suspended duration) or, at semantics_rev >= 2,
                // MAX_SUSPENSION_CYCLES (completed suspend/resume cycles).
                // Both take the same posture in `Session::resume`: the
                // session is now Expired. `resume` mutates `session` before
                // returning `Err`, so both fields are readable here to name
                // which cap actually fired instead of leaving an operator to
                // guess between two causes that share one error variant.
                let cycle_cap_exceeded = session.suspension_intervals.len() > MAX_SUSPENSION_CYCLES;
                let duration_cap_exceeded =
                    session.accumulated_suspended_ms > session.effective_max_suspend_ms();
                tracing::warn!(
                    session_id,
                    cycle_cap_exceeded,
                    duration_cap_exceeded,
                    suspension_cycles = session.suspension_intervals.len(),
                    accumulated_suspended_ms = session.accumulated_suspended_ms,
                    "session force-expired: suspension cap exceeded"
                );
                self.save_session_to_storage(session).await;
                self.metrics.record_session_expired(&session.mode);
                let _ = self
                    .session_lifecycle_bus
                    .send(SessionLifecycleEvent::Expired {
                        session_id: session_id.to_string(),
                    });
                Err(MacpError::TtlExpired)
            }
        }
    }

    /// Best-effort log compaction for terminal sessions.
    /// Returns `true` if compaction succeeded, `false` if skipped or failed.
    async fn maybe_compact_log(&self, session_id: &str, session: &Session) -> bool {
        if !self.compact_terminal_logs {
            return false;
        }
        // Ordinal accounting for the sequence contract: the checkpoint must
        // record every accepted ordinal it discards, including any base from
        // a prior compaction recorded in the current log.
        let discarded = match self.log_store.get_log(session_id).await {
            Some(entries) => {
                let prior_base: u64 = entries
                    .iter()
                    .filter(|e| e.entry_kind == EntryKind::Checkpoint)
                    .map(|e| e.compacted_incoming_ordinals)
                    .max()
                    .unwrap_or(0);
                prior_base
                    + entries
                        .iter()
                        .filter(|e| e.entry_kind == EntryKind::Incoming)
                        .count() as u64
            }
            None => 0,
        };
        match crate::storage::compaction::compact_session_log(
            &*self.storage,
            session_id,
            session,
            discarded,
        )
        .await
        {
            Ok(checkpoint) => {
                // Keep the in-memory log in step with storage — previously
                // only disk was rewritten, so memory and disk diverged and
                // post-restart passive-subscribe history vanished silently.
                self.log_store
                    .replace_session_log(session_id, vec![checkpoint])
                    .await;
                true
            }
            Err(e) => {
                tracing::debug!(
                    session_id,
                    error = %e,
                    "log compaction skipped (backend may not support it)"
                );
                false
            }
        }
    }

    /// Force a checkpoint entry regardless of interval settings.
    /// Used as a fallback when compaction fails on terminal sessions.
    async fn force_insert_checkpoint(&self, session_id: &str, session: &Session) {
        let persisted = crate::registry::PersistedSession::from(session);
        let raw_payload = match serde_json::to_vec(&persisted) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(session_id, error = %e, "failed to serialize forced checkpoint");
                return;
            }
        };
        let now = self.now_ms();
        let checkpoint = LogEntry {
            message_id: String::new(),
            received_at_ms: now,
            sender: "_runtime".into(),
            message_type: "Checkpoint".into(),
            raw_payload,
            entry_kind: EntryKind::Checkpoint,
            session_id: session_id.into(),
            mode: session.mode.clone(),
            macp_version: String::new(),
            timestamp_unix_ms: now,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        };
        if let Err(e) = self.storage.append_log_entry(session_id, &checkpoint).await {
            tracing::warn!(session_id, error = %e, "failed to write forced checkpoint");
            return;
        }
        self.log_store.append(session_id, checkpoint).await;
        tracing::debug!(
            session_id,
            "forced checkpoint inserted for terminal session"
        );
    }

    /// Insert a checkpoint entry if the log has reached the configured interval.
    ///
    /// Called after **every** append that can cross a boundary: the tail of
    /// `process_message`, and [`Runtime::synthesize_due_accept`] for the
    /// synthetic entry it writes. The second call site is what keeps the eager
    /// sweep and the lazy trigger placing checkpoints identically — see the
    /// note at that call. The decision is a pure function of the current
    /// `log_len`, so asking twice within one `process_message` (at two
    /// different lengths) is not a repeat.
    async fn maybe_insert_checkpoint(&self, session_id: &str, session: &Session) {
        if self.checkpoint_interval == 0 {
            return;
        }
        let log_len = self
            .log_store
            .get_log(session_id)
            .await
            .map(|l| l.len())
            .unwrap_or(0);
        // Only checkpoint at interval boundaries, and not on the first entry
        if log_len < self.checkpoint_interval || log_len % self.checkpoint_interval != 0 {
            return;
        }
        self.force_insert_checkpoint(session_id, session).await;
        tracing::debug!(session_id, log_len, "checkpoint inserted at interval");
    }

    /// Expire all sessions that have exceeded their TTL.
    /// Called by the background cleanup task to proactively transition
    /// stale sessions without waiting for the next incoming message.
    pub async fn cleanup_expired_sessions(&self) {
        let now = self.now_ms();
        // Snapshot the shared handles under a brief map read; never hold the
        // map lock across per-session locks or storage I/O. Each session is
        // re-checked under its own mutex (it may have been touched since the
        // snapshot).
        let candidates: Vec<(String, crate::registry::SharedSession)> = {
            let guard = self.registry.sessions.read().await;
            guard
                .iter()
                .map(|(id, arc)| (id.clone(), std::sync::Arc::clone(arc)))
                .collect()
        };

        let mut expired_count = 0usize;
        for (session_id, shared) in candidates {
            let mut session = shared.lock().await;
            if session.state != SessionState::Open || now <= session.ttl_expiry {
                continue;
            }
            let entry =
                Self::make_internal_entry("TtlExpired", b"", &session_id, &session.mode, now);
            if let Err(e) = self.storage.append_log_entry(&session_id, &entry).await {
                tracing::warn!(
                    session_id,
                    error = %e,
                    "failed to write TTL expiry during cleanup"
                );
                continue;
            }
            self.log_store.append(&session_id, entry).await;
            session.state = SessionState::Expired;
            self.metrics.record_session_expired(&session.mode);
            self.save_session_to_storage(&session).await;
            if !self.maybe_compact_log(&session_id, &session).await {
                self.force_insert_checkpoint(&session_id, &session).await;
            }
            expired_count += 1;
            tracing::info!(session_id = %session_id, "session expired via background cleanup");
            let _ = self
                .session_lifecycle_bus
                .send(SessionLifecycleEvent::Expired {
                    session_id: session_id.clone(),
                });
        }

        if expired_count > 0 {
            tracing::info!(count = expired_count, "background cleanup expired sessions");
        }
    }

    /// Emit every synthetic envelope that has become due, across all open
    /// sessions — RFC-MACP-0010 §5.1(2)'s **eager** observation of the
    /// implicit-accept deadline.
    ///
    /// Lazy observation (`process_message` -> `synthesize_due_accept`) is the
    /// MUST and ships on its own: it guarantees no message is ever evaluated
    /// against a stale offer. This is the SHOULD on top of it — without it a
    /// session where nobody speaks again keeps an accepted offer out of history
    /// indefinitely, and `GetSession` / `StreamSession` show an offer that the
    /// protocol says was accepted at `D`. Called from the background
    /// maintenance loop in `src/main.rs`, so `MACP_CLEANUP_INTERVAL_SECS` is
    /// the latency bound on the observation (never on the recorded timestamp,
    /// which is `D` whichever path emits — see below).
    ///
    /// # Not a variant of `cleanup_expired_sessions`
    ///
    /// Different predicate (a mode-computed deadline inside `mode_state`, not
    /// `ttl_expiry`) and a different product: an `EntryKind::Incoming` entry
    /// that consumes an accepted ordinal and is published to `StreamSession`,
    /// where `TtlExpired` is `Internal` and is published to neither. It is
    /// deliberately ordered *after* `cleanup_expired_sessions` in that loop so
    /// a TTL-expired session is already non-`Open` when the sweep reaches it —
    /// which is exactly the precedence the lazy path gives (`Precheck::Expired`
    /// returns before `synthesize_due_accept` is ever called), so the two paths
    /// cannot disagree about a session whose TTL and implicit-accept deadline
    /// both passed unobserved.
    ///
    /// # Locking
    ///
    /// The registry map lock is held only for the snapshot of `(id, Arc)` pairs
    /// and is released before any session mutex is taken or any I/O happens —
    /// the lock-ordering contract on `SessionRegistry`. The snapshot fixes the
    /// *set*, not the state.
    ///
    /// # Why no snapshot-to-append race can leave an orphan entry
    ///
    /// A session can resolve, cancel or expire between the snapshot and the
    /// moment this loop reaches it, and the `Arc` keeps it alive (and writable)
    /// regardless. Nothing here reads state at snapshot time: every decision is
    /// made *under the session mutex*, which is the same mutex every writer —
    /// `process_message`, `cancel_session`, `suspend_session`, `resume_session`,
    /// `cleanup_expired_sessions` — holds across its own validate-append-commit.
    /// So the `state != Open` re-check inside `synthesize_due_accept` observes
    /// the session as the last writer left it, and a session that terminated
    /// after the snapshot is skipped. Two further guards make it belt and
    /// braces: the mode returns `None` once the offer's disposition is no
    /// longer `Offered` (so an explicit `HandoffAccept` that won the race
    /// disarms the synthesis), and the deterministic `message_id` is already in
    /// `seen_message_ids` after any emission. Eviction cannot orphan one
    /// either: `evict_stale_sessions` and `gc_disk_sessions` only ever drop
    /// *terminal* sessions, which fail the `Open` check.
    ///
    /// # Cost
    ///
    /// One `mode_state` decode per open session whose mode implements the hook
    /// and whose policy binds a timeout, per tick; everything else short-circuits
    /// before decoding (`Mode::due_synthetic_envelope`'s own cost note).
    ///
    /// Returns the number of synthetic entries appended.
    pub async fn sweep_due_synthetic_accepts(&self) -> usize {
        let now = self.now_ms();
        // Snapshot the shared handles under a brief map read; never hold the
        // map lock across per-session locks or storage I/O. Mirrors
        // `cleanup_expired_sessions`.
        let candidates: Vec<(String, crate::registry::SharedSession)> = {
            let guard = self.registry.sessions.read().await;
            guard
                .iter()
                .map(|(id, arc)| (id.clone(), std::sync::Arc::clone(arc)))
                .collect()
        };

        let mut emitted = 0usize;
        for (session_id, shared) in candidates {
            let mut session = shared.lock().await;
            // `Suspended` (and every other non-`Open` state) is skipped here
            // *and* inside `synthesize_due_accept`. Measured: removing this
            // check alone changes no behaviour — the seam's own filter still
            // declines — so treat it as the documented precondition of the
            // call below rather than as the enforcement. The enforcement
            // matters: both computations behind the synthetic entry ignore an
            // in-flight pause, so asking about a paused session over-counts
            // elapsed time and bakes a wrong `D` into permanent history, and
            // this is the only caller that can ever be handed one (no message
            // path reaches a non-`Open` session at all).
            if session.state != SessionState::Open {
                continue;
            }
            match self
                .synthesize_due_accept(&session_id, &mut session, now)
                .await
            {
                Ok(true) => emitted += 1,
                Ok(false) => {}
                // No triggering message to reject: a failed append (or a mode
                // that refused its own synthetic) is logged and the offer stays
                // outstanding, to be retried on the next tick or settled by the
                // lazy path. The same posture `cleanup_expired_sessions` takes
                // on a failed `TtlExpired` append.
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "eager sweep could not emit a due synthetic envelope"
                    );
                }
            }
        }

        if emitted > 0 {
            tracing::info!(
                count = emitted,
                "eager sweep appended due synthetic envelopes"
            );
        }
        emitted
    }

    /// Delete terminal sessions' durable data older than `retention_secs`
    /// (opt-in via `MACP_SESSION_DISK_RETENTION_SECS`). Before this existed,
    /// `storage.delete_session` had no callers at all: disk grew without
    /// bound and every restart reloaded every session ever completed.
    /// Enumerates STORAGE (not memory — eviction may already have dropped the
    /// registry entry), deletes the session's snapshot+log, and clears any
    /// in-memory remnants. Returns the number of sessions deleted.
    pub async fn gc_disk_sessions(&self, retention_secs: u64) -> usize {
        let now = self.now_ms();
        let cutoff = now - (retention_secs as i64 * 1000);
        let ids = match self.storage.list_session_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "disk GC: cannot list sessions");
                return 0;
            }
        };
        let mut removed = 0usize;
        for id in ids {
            // Prefer the in-memory state when present (cheap + current);
            // fall back to the stored snapshot for evicted sessions.
            let eligible = if let Some(shared) = self.registry.get_shared(&id).await {
                let s = shared.lock().await;
                s.state.is_terminal() && s.started_at_unix_ms < cutoff
            } else {
                match self.storage.load_session(&id).await {
                    Ok(Some(s)) => s.state.is_terminal() && s.started_at_unix_ms < cutoff,
                    // No snapshot (or unreadable): leave it for operator
                    // inspection rather than guessing.
                    _ => false,
                }
            };
            if !eligible {
                continue;
            }
            // Hold the session's receipt lock so an issuance in flight either
            // stores before this delete (and is deleted with it) or sees the
            // session gone.
            let _issuing = self.receipts.stripe(&id).lock().await;
            match self.storage.delete_session(&id).await {
                Ok(()) => {
                    {
                        let mut guard = self.registry.sessions.write().await;
                        guard.remove(&id);
                    }
                    self.log_store.remove_session_log(&id).await;
                    let _ = self.stream_bus.remove_if_unused(&id);
                    self.receipts.forget(std::slice::from_ref(&id)).await;
                    removed += 1;
                }
                Err(e) => {
                    tracing::warn!(session_id = %id, error = %e, "disk GC: delete failed");
                }
            }
        }
        if removed > 0 {
            tracing::info!(count = removed, "disk GC removed terminal sessions");
        }
        removed
    }

    /// Evict resolved/expired sessions older than `retention_secs` from
    /// memory: the registry entry, the in-memory log cache, AND the stream
    /// broadcast channel (all three previously grew for the process lifetime;
    /// the log cache and stream bus were never evicted at all). Sessions
    /// remain queryable from durable storage after eviction.
    pub async fn evict_stale_sessions(&self, retention_secs: u64) {
        let now = self.now_ms();
        let cutoff = now - (retention_secs as i64 * 1000);

        let candidates: Vec<(String, crate::registry::SharedSession)> = {
            let guard = self.registry.sessions.read().await;
            guard
                .iter()
                .map(|(id, arc)| (id.clone(), std::sync::Arc::clone(arc)))
                .collect()
        };
        let mut evict_ids = Vec::new();
        for (id, shared) in candidates {
            let session = shared.lock().await;
            if matches!(
                session.state,
                SessionState::Resolved | SessionState::Expired | SessionState::Cancelled
            ) && session.started_at_unix_ms < cutoff
            {
                evict_ids.push(id);
            }
        }

        if evict_ids.is_empty() {
            return;
        }
        {
            let mut guard = self.registry.sessions.write().await;
            for id in &evict_ids {
                guard.remove(id);
            }
        }
        // The stored receipt stays; only the in-memory copy goes.
        self.receipts.forget(&evict_ids).await;
        for id in &evict_ids {
            self.log_store.remove_session_log(id).await;
            // Left in place if a subscriber is still attached; retried on the
            // next sweep once receivers drop.
            let _ = self.stream_bus.remove_if_unused(id);
        }
        tracing::info!(
            count = evict_ids.len(),
            "evicted stale sessions from memory (registry + log cache + stream bus)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_pb::ProposalPayload;
    use crate::pb::{CommitmentPayload, SessionStartPayload};
    use chrono::Utc;
    use prost::Message;

    fn new_sid() -> String {
        uuid::Uuid::new_v4().as_hyphenated().to_string()
    }

    fn make_runtime() -> Runtime {
        let storage: Arc<dyn StorageBackend> = Arc::new(crate::storage::MemoryBackend);
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        Runtime::new(storage, registry, log_store)
    }

    fn session_start(participants: Vec<String>) -> Vec<u8> {
        SessionStartPayload {
            intent: "intent".into(),
            participants,
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec()
    }

    fn env(
        mode: &str,
        message_type: &str,
        message_id: &str,
        session_id: &str,
        sender: &str,
        payload: Vec<u8>,
    ) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: mode.into(),
            message_type: message_type.into(),
            message_id: message_id.into(),
            session_id: session_id.into(),
            sender: sender.into(),
            timestamp_unix_ms: Utc::now().timestamp_millis(),
            payload,
        }
    }

    #[tokio::test]
    async fn standard_session_start_is_strict() {
        let rt = make_runtime();
        let sid = new_sid();
        let bad = SessionStartPayload {
            ttl_ms: 0,
            ..Default::default()
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    bad,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MacpError::InvalidPayload | MacpError::InvalidTtl
        ));
    }

    /// A **promoted** extension mode keeps the full canonical `SessionStart`
    /// contract, including the roster requirement.
    ///
    /// This pins the *call site's* choice of strictness source, which no other
    /// test covers. `ModeRegistry::requires_strict_session_start` reads a
    /// per-entry `strict_session_start` flag that `promote_mode` sets to `true`;
    /// `macp_core::session::requires_strict_session_start` reads a static name
    /// list that has never heard of a promoted mode's name. The two disagree
    /// exactly here, so routing this call through
    /// `validate_strict_session_start_payload` — which consults the static list
    /// — would silently skip canonical validation for every promoted mode while
    /// leaving the whole suite green. Measured: it does.
    #[tokio::test]
    async fn a_promoted_mode_still_gets_canonical_session_start_validation() {
        let mode_registry = Arc::new(ModeRegistry::build_default(std::sync::Arc::new(
            macp_policy::DefaultPolicyEvaluator,
        )));
        mode_registry
            .register_extension(crate::pb::ModeDescriptor {
                mode: "ext.promoted.v1".into(),
                mode_version: "1.0.0".into(),
                title: "Promoted".into(),
                description: "promotion target".into(),
                determinism_class: "semantic-deterministic".into(),
                participant_model: "declared".into(),
                message_types: vec!["SessionStart".into(), "Commitment".into()],
                terminal_message_types: vec!["Commitment".into()],
                ..Default::default()
            })
            .expect("register extension");
        assert_eq!(
            mode_registry.promote_mode("ext.promoted.v1", None).unwrap(),
            "ext.promoted.v1"
        );
        assert!(
            mode_registry.requires_strict_session_start("ext.promoted.v1"),
            "promotion must mark the entry strict"
        );
        assert!(
            !crate::session::requires_strict_session_start("ext.promoted.v1"),
            "the core's static list must NOT know this name — that disagreement is the point"
        );

        let rt = Runtime::with_mode_registry(
            Arc::new(crate::storage::MemoryBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
            mode_registry,
        );

        // An empty roster: refused, because the carve-out names Decision only.
        let err = rt
            .process(
                &env(
                    "ext.promoted.v1",
                    "SessionStart",
                    "m1",
                    &new_sid(),
                    "agent://orchestrator",
                    session_start(vec![]),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // And the rest of the canonical contract too, so the assertion above
        // cannot be satisfied by a runtime that only kept the roster rule.
        let no_versions = SessionStartPayload {
            participants: vec!["agent://fraud".into()],
            ttl_ms: 1_000,
            ..Default::default()
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "ext.promoted.v1",
                    "SessionStart",
                    "m2",
                    &new_sid(),
                    "agent://orchestrator",
                    no_versions,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // Positive control: a complete payload is accepted, so the two refusals
        // above are the roster and version rules and not a broken mode.
        rt.process(
            &env(
                "ext.promoted.v1",
                "SessionStart",
                "m3",
                &new_sid(),
                "agent://orchestrator",
                session_start(vec!["agent://fraud".into()]),
            ),
            None,
        )
        .await
        .expect("a complete SessionStart must still be accepted for a promoted mode");
    }

    #[tokio::test]
    async fn empty_mode_is_rejected() {
        let rt = make_runtime();
        let sid = new_sid();
        let err = rt
            .process(
                &env(
                    "",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    session_start(vec!["agent://fraud".into()]),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidEnvelope");
    }

    #[tokio::test]
    async fn rejected_messages_do_not_enter_dedup_state() {
        let rt = make_runtime();
        let sid = new_sid();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                session_start(vec!["agent://orchestrator".into(), "agent://fraud".into()]),
            ),
            None,
        )
        .await
        .unwrap();

        let bad = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "Proposal",
                    "m2",
                    &sid,
                    "agent://fraud",
                    b"not-protobuf".to_vec(),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(bad.to_string(), "InvalidPayload");

        let good = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let result = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "Proposal",
                    "m2",
                    &sid,
                    "agent://orchestrator",
                    good,
                ),
                None,
            )
            .await
            .unwrap();
        assert!(!result.duplicate);
    }

    #[tokio::test]
    async fn get_session_transitions_expired_sessions() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(session.state, SessionState::Expired);
    }

    #[tokio::test]
    async fn multi_round_requires_standard_session_start() {
        let rt = make_runtime();
        let sid = new_sid();
        // multi-round is now standards-track: empty mode_version should fail
        let payload = SessionStartPayload {
            participants: vec!["creator".into(), "other".into()],
            ..Default::default()
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "ext.multi_round.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "creator",
                    payload,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            MacpError::InvalidPayload | MacpError::InvalidTtl
        ));
    }

    #[tokio::test]
    async fn multi_round_valid_session_start() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = session_start(vec!["alice".into(), "bob".into()]);
        rt.process(
            &env(
                "ext.multi_round.v1",
                "SessionStart",
                "m1",
                &sid,
                "coordinator",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(session.mode, "ext.multi_round.v1");
        assert_eq!(session.participants, vec!["alice", "bob"]);
    }

    #[tokio::test]
    async fn duplicate_session_start_message_id_returns_duplicate() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = session_start(vec!["agent://fraud".into()]);
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                payload.clone(),
            ),
            None,
        )
        .await
        .unwrap();

        let result = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    payload,
                ),
                None,
            )
            .await
            .unwrap();
        assert!(result.duplicate);
    }

    #[tokio::test]
    async fn non_start_mode_mismatch_rejected() {
        let rt = make_runtime();
        let sid = new_sid();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                session_start(vec!["agent://fraud".into()]),
            ),
            None,
        )
        .await
        .unwrap();

        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "macp.mode.task.v1",
                    "Proposal",
                    "m2",
                    &sid,
                    "agent://orchestrator",
                    proposal,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidEnvelope");
    }

    #[tokio::test]
    async fn cancel_idempotent_on_already_expired() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let result = rt
            .cancel_session(&sid, "cleanup", "agent://orchestrator")
            .await
            .unwrap();
        assert_eq!(result.session_state, SessionState::Expired);
    }

    #[tokio::test]
    async fn accepted_envelopes_are_published_in_order() {
        let rt = make_runtime();
        let sid = new_sid();
        let mut events = rt.subscribe_session_stream(&sid);

        let start = env(
            "macp.mode.decision.v1",
            "SessionStart",
            "m1",
            &sid,
            "agent://orchestrator",
            session_start(vec!["agent://orchestrator".into(), "agent://fraud".into()]),
        );
        rt.process(&start, None).await.unwrap();
        let first = events.recv().await.unwrap();
        assert_eq!(first.message_id, "m1");
        assert_eq!(first.message_type, "SessionStart");

        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let proposal_env = env(
            "macp.mode.decision.v1",
            "Proposal",
            "m2",
            &sid,
            "agent://orchestrator",
            proposal,
        );
        rt.process(&proposal_env, None).await.unwrap();
        let second = events.recv().await.unwrap();
        assert_eq!(second.message_id, "m2");
        assert_eq!(second.message_type, "Proposal");
    }

    #[tokio::test]
    async fn commitment_versions_are_carried_into_resolution() {
        let rt = make_runtime();
        let sid = new_sid();
        rt.process(
            &env(
                "macp.mode.proposal.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://buyer",
                session_start(vec!["agent://buyer".into(), "agent://seller".into()]),
            ),
            None,
        )
        .await
        .unwrap();

        let proposal = crate::proposal_pb::ProposalPayload {
            proposal_id: "p1".into(),
            title: "offer".into(),
            summary: "summary".into(),
            details: vec![],
            tags: vec![],
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.proposal.v1",
                "Proposal",
                "m2",
                &sid,
                "agent://seller",
                proposal,
            ),
            None,
        )
        .await
        .unwrap();
        let accept = crate::proposal_pb::AcceptPayload {
            proposal_id: "p1".into(),
            reason: String::new(),
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.proposal.v1",
                "Accept",
                "m3",
                &sid,
                "agent://seller",
                accept.clone(),
            ),
            None,
        )
        .await
        .unwrap();
        rt.process(
            &env(
                "macp.mode.proposal.v1",
                "Accept",
                "m4",
                &sid,
                "agent://buyer",
                accept,
            ),
            None,
        )
        .await
        .unwrap();
        let commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "proposal.accepted".into(),
            authority_scope: "commercial".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let result = rt
            .process(
                &env(
                    "macp.mode.proposal.v1",
                    "Commitment",
                    "m5",
                    &sid,
                    "agent://buyer",
                    commitment,
                ),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.session_state, SessionState::Resolved);
    }

    #[tokio::test]
    async fn max_open_sessions_enforced_under_write_lock() {
        let rt = make_runtime();
        let sid1 = new_sid();
        let sid2 = new_sid();
        let sid3 = new_sid();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid1,
                "agent://orchestrator",
                session_start(vec!["agent://fraud".into()]),
            ),
            Some(1),
        )
        .await
        .unwrap();

        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m2",
                    &sid2,
                    "agent://orchestrator",
                    session_start(vec!["agent://fraud".into()]),
                ),
                Some(1),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MacpError::RateLimited));

        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m3",
                &sid3,
                "agent://other",
                session_start(vec!["agent://fraud".into()]),
            ),
            Some(1),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn weak_session_id_rejected() {
        let rt = make_runtime();
        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    "s1",
                    "agent://orchestrator",
                    session_start(vec!["agent://fraud".into()]),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidSessionId");
    }

    #[tokio::test]
    async fn log_append_failure_rejects_session_start() {
        use std::io;
        struct FailingBackend;
        #[async_trait::async_trait]
        impl StorageBackend for FailingBackend {
            async fn save_session(&self, _: &Session) -> io::Result<()> {
                Ok(())
            }
            async fn load_session(&self, _: &str) -> io::Result<Option<Session>> {
                Ok(None)
            }
            async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
                Ok(vec![])
            }
            async fn delete_session(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
            async fn list_session_ids(&self) -> io::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn append_log_entry(&self, _: &str, _: &LogEntry) -> io::Result<()> {
                Err(io::Error::other("disk full"))
            }
            async fn load_log(&self, _: &str) -> io::Result<Vec<LogEntry>> {
                Ok(vec![])
            }
            async fn create_session_storage(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
        }

        let storage: Arc<dyn StorageBackend> = Arc::new(FailingBackend);
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let rt = Runtime::new(storage, registry, log_store);
        let sid = new_sid();

        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    session_start(vec!["agent://fraud".into()]),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "StorageFailed");
    }

    #[tokio::test]
    async fn log_append_failure_rejects_in_session_message() {
        use std::io;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FailOnSecondAppend {
            count: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl StorageBackend for FailOnSecondAppend {
            async fn save_session(&self, _: &Session) -> io::Result<()> {
                Ok(())
            }
            async fn load_session(&self, _: &str) -> io::Result<Option<Session>> {
                Ok(None)
            }
            async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
                Ok(vec![])
            }
            async fn delete_session(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
            async fn list_session_ids(&self) -> io::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn append_log_entry(&self, _: &str, _: &LogEntry) -> io::Result<()> {
                let n = self.count.fetch_add(1, Ordering::SeqCst);
                if n >= 1 {
                    Err(io::Error::other("disk full"))
                } else {
                    Ok(())
                }
            }
            async fn load_log(&self, _: &str) -> io::Result<Vec<LogEntry>> {
                Ok(vec![])
            }
            async fn create_session_storage(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
        }

        let storage: Arc<dyn StorageBackend> = Arc::new(FailOnSecondAppend {
            count: AtomicUsize::new(0),
        });
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let rt = Runtime::new(storage, registry, log_store);
        let sid = new_sid();

        // SessionStart succeeds (first append)
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                session_start(vec!["agent://orchestrator".into(), "agent://fraud".into()]),
            ),
            None,
        )
        .await
        .unwrap();

        // Proposal fails (second append)
        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "Proposal",
                    "m2",
                    &sid,
                    "agent://orchestrator",
                    proposal,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "StorageFailed");

        // Verify the message was not added to dedup state
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert!(!session.seen_message_ids.contains("m2"));
    }

    #[tokio::test]
    async fn cancel_session_fails_if_log_append_fails() {
        use std::io;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FailOnSecondAppend {
            count: AtomicUsize,
        }
        #[async_trait::async_trait]
        impl StorageBackend for FailOnSecondAppend {
            async fn save_session(&self, _: &Session) -> io::Result<()> {
                Ok(())
            }
            async fn load_session(&self, _: &str) -> io::Result<Option<Session>> {
                Ok(None)
            }
            async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
                Ok(vec![])
            }
            async fn delete_session(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
            async fn list_session_ids(&self) -> io::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn append_log_entry(&self, _: &str, _: &LogEntry) -> io::Result<()> {
                let n = self.count.fetch_add(1, Ordering::SeqCst);
                if n >= 1 {
                    Err(io::Error::other("disk full"))
                } else {
                    Ok(())
                }
            }
            async fn load_log(&self, _: &str) -> io::Result<Vec<LogEntry>> {
                Ok(vec![])
            }
            async fn create_session_storage(&self, _: &str) -> io::Result<()> {
                Ok(())
            }
        }

        let storage: Arc<dyn StorageBackend> = Arc::new(FailOnSecondAppend {
            count: AtomicUsize::new(0),
        });
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let rt = Runtime::new(storage, registry, log_store);
        let sid = new_sid();

        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                session_start(vec!["agent://fraud".into()]),
            ),
            None,
        )
        .await
        .unwrap();

        let err = rt
            .cancel_session(&sid, "test cancel", "agent://orchestrator")
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "StorageFailed");
    }

    #[tokio::test]
    async fn ttl_expiration_rejects_message() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://orchestrator".into(), "agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "Proposal",
                    "m2",
                    &sid,
                    "agent://orchestrator",
                    proposal,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "TtlExpired");
    }

    #[tokio::test]
    async fn cleanup_expired_sessions_marks_expired() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 1,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        rt.cleanup_expired_sessions().await;
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(session.state, SessionState::Expired);
    }

    #[tokio::test]
    async fn evict_stale_sessions_removes_resolved() {
        let rt = make_runtime();
        let sid = new_sid();
        // Start a decision session
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "agent://orchestrator",
                session_start(vec!["agent://orchestrator".into(), "agent://fraud".into()]),
            ),
            None,
        )
        .await
        .unwrap();
        // Send a Proposal
        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "step-up".into(),
            rationale: "risk".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "Proposal",
                "m2",
                &sid,
                "agent://orchestrator",
                proposal,
            ),
            None,
        )
        .await
        .unwrap();
        // Commit to resolve the session
        let commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.selected".into(),
            authority_scope: "payments".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let result = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "Commitment",
                    "m3",
                    &sid,
                    "agent://orchestrator",
                    commitment,
                ),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.session_state, SessionState::Resolved);
        // Wait a moment so the session's started_at_unix_ms is strictly in the past
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        // Evict with retention = 0 (evict immediately)
        rt.evict_stale_sessions(0).await;
        // Session should no longer be in the in-memory registry
        assert!(rt.registry.get_session(&sid).await.is_none());
    }

    #[tokio::test]
    async fn session_start_with_wrong_mode_version_rejected() {
        let rt = make_runtime();
        let sid = new_sid();
        let payload = SessionStartPayload {
            intent: "test".into(),
            participants: vec!["agent://orchestrator".into(), "agent://worker".into()],
            mode_version: "99.0.0".into(), // wrong version
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();

        let err = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    payload,
                ),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.error_code(), "INVALID_ENVELOPE");
    }

    #[tokio::test]
    async fn signal_empty_signal_type_rejected() {
        let rt = make_runtime();
        // Use non-default data so proto3 serializes a non-empty payload
        let signal_payload = crate::pb::SignalPayload {
            signal_type: String::new(),
            data: b"some data".to_vec(),
            confidence: 0.0,
            correlation_session_id: String::new(),
        }
        .encode_to_vec();
        let signal = Envelope {
            macp_version: "1.0".into(),
            mode: String::new(),
            message_type: "Signal".into(),
            message_id: "sig-1".into(),
            session_id: String::new(),
            sender: "agent://a".into(),
            timestamp_unix_ms: 0,
            payload: signal_payload,
        };
        let err = rt.process_signal(&signal).await.unwrap_err();
        assert_eq!(err.error_code(), "INVALID_ENVELOPE");
    }

    #[tokio::test]
    async fn signal_valid_payload_accepted() {
        let rt = make_runtime();
        let signal_payload = crate::pb::SignalPayload {
            signal_type: "heartbeat".into(),
            data: vec![],
            confidence: 0.8,
            correlation_session_id: String::new(),
        }
        .encode_to_vec();
        let signal = Envelope {
            macp_version: "1.0".into(),
            mode: String::new(),
            message_type: "Signal".into(),
            message_id: "sig-2".into(),
            session_id: String::new(),
            sender: "agent://a".into(),
            timestamp_unix_ms: 0,
            payload: signal_payload,
        };
        rt.process_signal(&signal).await.unwrap();
    }

    #[tokio::test]
    async fn signal_empty_payload_accepted() {
        let rt = make_runtime();
        let signal = Envelope {
            macp_version: "1.0".into(),
            mode: String::new(),
            message_type: "Signal".into(),
            message_id: "sig-3".into(),
            session_id: String::new(),
            sender: "agent://a".into(),
            timestamp_unix_ms: 0,
            payload: vec![],
        };
        rt.process_signal(&signal).await.unwrap();
    }

    /// Freeze invariant: CommitmentPayload version fields must match the
    /// session-bound versions — for extension modes too. When a non-strict ext
    /// mode's SessionStart omits mode_version, the runtime binds the registered
    /// descriptor's version; a Commitment carrying "" must no longer match
    /// vacuously.
    #[tokio::test]
    async fn ext_mode_empty_version_binds_descriptor_version() {
        let rt = make_runtime();
        rt.register_extension(ModeDescriptor {
            mode: "ext.dyn.v1".into(),
            mode_version: "2.5.0".into(),
            message_types: vec!["SessionStart".into(), "Note".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        })
        .unwrap();

        let sid = new_sid();
        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        rt.process(
            &env("ext.dyn.v1", "SessionStart", "m1", &sid, "alice", payload),
            None,
        )
        .await
        .unwrap();

        // The session is bound to the descriptor's version, not "".
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(session.mode_version, "2.5.0");

        // Commitment with empty mode_version: rejected (no vacuous match).
        let bad = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "work.completed".into(),
            authority_scope: "test".into(),
            reason: "done".into(),
            mode_version: String::new(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let err = rt
            .process(
                &env("ext.dyn.v1", "Commitment", "m2", &sid, "alice", bad),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");

        // Commitment echoing the bound descriptor version: accepted, resolves.
        let good = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "work.completed".into(),
            authority_scope: "test".into(),
            reason: "done".into(),
            mode_version: "2.5.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let result = rt
            .process(
                &env("ext.dyn.v1", "Commitment", "m3", &sid, "alice", good),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.session_state, SessionState::Resolved);
    }

    /// The binding must be recorded on the SessionStart log entry (replay reads
    /// it from there), and only when the payload actually omitted the version.
    #[tokio::test]
    async fn ext_mode_binding_recorded_on_session_start_log_entry() {
        let rt = make_runtime();
        rt.register_extension(ModeDescriptor {
            mode: "ext.dyn2.v1".into(),
            mode_version: "3.0.0".into(),
            message_types: vec!["SessionStart".into(), "Commitment".into()],
            terminal_message_types: vec!["Commitment".into()],
            ..Default::default()
        })
        .unwrap();

        let sid = new_sid();
        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        rt.process(
            &env("ext.dyn2.v1", "SessionStart", "m1", &sid, "alice", payload),
            None,
        )
        .await
        .unwrap();

        let log = rt.log_store.get_log(&sid).await.unwrap();
        assert_eq!(log[0].message_type, "SessionStart");
        assert_eq!(log[0].bound_mode_version.as_deref(), Some("3.0.0"));

        // A payload that carries the version explicitly records no binding.
        let sid2 = new_sid();
        let payload2 = SessionStartPayload {
            participants: vec!["alice".into()],
            mode_version: "3.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        rt.process(
            &env(
                "ext.dyn2.v1",
                "SessionStart",
                "m1",
                &sid2,
                "alice",
                payload2,
            ),
            None,
        )
        .await
        .unwrap();
        let log2 = rt.log_store.get_log(&sid2).await.unwrap();
        assert_eq!(log2[0].bound_mode_version, None);
    }

    /// The RESOLVED suspension cap is bound on the session and recorded on
    /// the SessionStart log entry (RFC-MACP-0001 §7.5, RFC-MACP-0003 §2):
    /// the payload's positive value verbatim, or the runtime default when
    /// the payload carried 0 — never left unrecorded on new sessions.
    #[tokio::test]
    async fn session_start_binds_and_records_max_suspend_cap() {
        let rt = make_runtime();

        // Explicit cap: recorded verbatim.
        let sid = new_sid();
        let payload = SessionStartPayload {
            participants: vec!["alice".into(), "bob".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            max_suspend_ms: 12_345,
            ..Default::default()
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m1",
                &sid,
                "alice",
                payload,
            ),
            None,
        )
        .await
        .unwrap();
        let log = rt.log_store.get_log(&sid).await.unwrap();
        assert_eq!(log[0].bound_max_suspend_ms, Some(12_345));

        // Payload 0: the runtime default is resolved and recorded.
        let sid2 = new_sid();
        let payload2 = SessionStartPayload {
            participants: vec!["alice".into(), "bob".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            max_suspend_ms: 0,
            ..Default::default()
        }
        .encode_to_vec();
        rt.process(
            &env(
                "macp.mode.decision.v1",
                "SessionStart",
                "m2",
                &sid2,
                "alice",
                payload2,
            ),
            None,
        )
        .await
        .unwrap();
        let log2 = rt.log_store.get_log(&sid2).await.unwrap();
        assert_eq!(
            log2[0].bound_max_suspend_ms,
            Some(macp_core::session::MAX_SUSPEND_MS)
        );
    }

    #[test]
    fn audit_verbosity_is_configuration_not_policy() {
        let mut session = Session::builder("s1", "macp.mode.decision.v1", "a")
            .policy_version("policy.audited")
            .build();
        assert!(!AuditConfig::default().is_verbose(&session));
        assert!(AuditConfig::from_values(Some("info"), None, None).is_verbose(&session));
        assert!(!AuditConfig::from_values(Some("debug"), None, None).is_verbose(&session));
        assert!(
            AuditConfig::from_values(None, Some(" other , policy.audited "), None)
                .is_verbose(&session)
        );
        assert!(
            AuditConfig::from_values(None, None, Some("macp.mode.decision.v1"))
                .is_verbose(&session)
        );
        assert!(!AuditConfig::from_values(None, Some(""), Some(",")).is_verbose(&session));

        // A legacy `rules.audit` block on a stored session's policy is inert.
        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "policy.legacy".into(),
            mode: "*".into(),
            description: "pre-0.9 audited".into(),
            rules: serde_json::json!({ "audit": { "level": "info" } }),
            schema_version: 1,
        });
        assert!(!AuditConfig::default().is_verbose(&session));
    }

    /// Post-commit-point coherence: once the SessionStart log entry is
    /// durable, a snapshot failure must NOT fail (or roll back) the start —
    /// the previous fatal path left the durable entry behind, so the
    /// "failed" session resurrected on restart and a same-id retry appended
    /// a second SessionStart that made the log unreplayable.
    #[tokio::test]
    async fn session_start_snapshot_failure_is_nonfatal_after_commit_point() {
        use std::io;

        struct FailSnapshotBackend;
        #[async_trait::async_trait]
        impl StorageBackend for FailSnapshotBackend {
            async fn create_session_storage(&self, _s: &str) -> io::Result<()> {
                Ok(())
            }
            async fn save_session(&self, _s: &Session) -> io::Result<()> {
                Err(io::Error::other("snapshot disk full"))
            }
            async fn load_session(&self, _s: &str) -> io::Result<Option<Session>> {
                Ok(None)
            }
            async fn load_all_sessions(&self) -> io::Result<Vec<Session>> {
                Ok(vec![])
            }
            async fn delete_session(&self, _s: &str) -> io::Result<()> {
                Ok(())
            }
            async fn list_session_ids(&self) -> io::Result<Vec<String>> {
                Ok(vec![])
            }
            async fn append_log_entry(
                &self,
                _s: &str,
                _e: &crate::log_store::LogEntry,
            ) -> io::Result<()> {
                Ok(())
            }
            async fn load_log(&self, _s: &str) -> io::Result<Vec<crate::log_store::LogEntry>> {
                Ok(vec![])
            }
        }

        let rt = Runtime::new(
            Arc::new(FailSnapshotBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
        );
        let sid = new_sid();
        let result = rt
            .process(
                &env(
                    "macp.mode.decision.v1",
                    "SessionStart",
                    "m1",
                    &sid,
                    "agent://orchestrator",
                    session_start(vec!["agent://orchestrator".into()]),
                ),
                None,
            )
            .await
            .expect("start must succeed: the log append (commit point) succeeded");
        assert!(!result.duplicate);
        // The session exists and is usable.
        assert!(rt.get_session_checked(&sid).await.is_some());
    }

    // --- The client boundary at the kernel's two live entry points (11c) ---
    //
    // RFC-MACP-0010 §5.1(3). These are the runtime-level halves; the
    // mode-level rules are unit-tested in
    // `crates/macp-modes/src/mode/handoff.rs`, `step::validate_message` in
    // `crates/macp-modes/src/step.rs`, and the proof that the hook is NOT on
    // the replay path in `src/replay.rs`
    // (`reserved_prefix_entry_replays_at_every_rev`).
    //
    // Both entry points matter independently: `process_message` and
    // `process_session_start` each call the hook themselves, because the
    // runtime deliberately bypasses `macp_modes::step::validate_message` (it
    // interposes its durable append between validation and commit), so neither
    // call site is covered by the other.

    const HANDOFF_MODE: &str = "macp.mode.handoff.v1";
    const OWNER: &str = "agent://owner";
    const TARGET: &str = "agent://target";

    fn reserved_id(handoff_id: &str) -> String {
        format!(
            "{}{handoff_id}",
            crate::mode::handoff::IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX
        )
    }

    fn handoff_start_payload() -> Vec<u8> {
        session_start(vec![OWNER.into(), TARGET.into()])
    }

    /// A `Commitment` that echoes the versions `handoff_session_with_offer`
    /// binds, so the only thing left to reject it is the `message_id`.
    fn handoff_commitment_payload() -> Vec<u8> {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy.default".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec()
    }

    fn handoff_offer(handoff_id: &str) -> Vec<u8> {
        crate::handoff_pb::HandoffOfferPayload {
            handoff_id: handoff_id.into(),
            target_participant: TARGET.into(),
            scope: "support".into(),
            reason: "escalate".into(),
        }
        .encode_to_vec()
    }

    fn handoff_context(handoff_id: &str) -> Vec<u8> {
        crate::handoff_pb::HandoffContextPayload {
            handoff_id: handoff_id.into(),
            content_type: "text/plain".into(),
            context: b"background".to_vec(),
        }
        .encode_to_vec()
    }

    fn handoff_accept(handoff_id: &str, implicit: bool) -> Vec<u8> {
        crate::handoff_pb::HandoffAcceptPayload {
            handoff_id: handoff_id.into(),
            accepted_by: TARGET.into(),
            reason: "ready".into(),
            implicit,
        }
        .encode_to_vec()
    }

    /// An Open handoff session at the current semantics revision with one
    /// outstanding offer `h1`. Returns the session id.
    async fn handoff_session_with_offer(rt: &Runtime) -> String {
        let sid = new_sid();
        rt.process(
            &env(
                HANDOFF_MODE,
                "SessionStart",
                "start-1",
                &sid,
                OWNER,
                handoff_start_payload(),
            ),
            None,
        )
        .await
        .expect("handoff session start");
        rt.process(
            &env(
                HANDOFF_MODE,
                "HandoffOffer",
                "offer-1",
                &sid,
                OWNER,
                handoff_offer("h1"),
            ),
            None,
        )
        .await
        .expect("handoff offer");
        assert_eq!(
            rt.get_session_checked(&sid).await.unwrap().semantics_rev,
            macp_core::session::CURRENT_SEMANTICS_REV
        );
        sid
    }

    /// Phase 11c criterion 1, message path: at the current revision a client
    /// envelope whose `message_id` is in the reserved `implicit-accept:`
    /// namespace is rejected `InvalidEnvelope`, and the rejection mutates
    /// nothing — neither accepted history nor `seen_message_ids` (CLAUDE.md §8
    /// dedup invariant).
    ///
    /// `HandoffContext` is the interesting carrier: it is the one mode message
    /// the offerer may send at any disposition, and it is rejected here
    /// **before** dispatch, which is why the check cannot live in the mode's
    /// own rules. `Commitment` is included because the initiator can squat the
    /// id that way too, and because it proves the reservation is not scoped to
    /// `HandoffAccept`.
    #[tokio::test]
    async fn reserved_message_id_namespace_is_rejected_at_rev2() {
        let rt = make_runtime();
        let sid = handoff_session_with_offer(&rt).await;

        let history_before = rt.log_store.get_log(&sid).await.unwrap().len();
        let dedup_before = rt
            .get_session_checked(&sid)
            .await
            .unwrap()
            .seen_message_ids
            .clone();

        for (message_type, sender, payload) in [
            ("HandoffContext", OWNER, handoff_context("h1")),
            ("Commitment", OWNER, handoff_commitment_payload()),
            ("HandoffAccept", TARGET, handoff_accept("h1", false)),
        ] {
            let err = rt
                .process(
                    &env(
                        HANDOFF_MODE,
                        message_type,
                        &reserved_id("h1"),
                        &sid,
                        sender,
                        payload,
                    ),
                    None,
                )
                .await
                .unwrap_err();
            assert!(
                matches!(err, MacpError::InvalidEnvelope),
                "{message_type} with a reserved id must be InvalidEnvelope, got {err}"
            );
        }

        // Nothing was appended and no dedup slot was consumed.
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(
            rt.log_store.get_log(&sid).await.unwrap().len(),
            history_before
        );
        assert_eq!(session.seen_message_ids, dedup_before);
        assert!(!session.seen_message_ids.contains(&reserved_id("h1")));
        assert_eq!(session.state, SessionState::Open);

        // Control: the same `HandoffContext` with an ordinary id is accepted,
        // so the rejections above are the id's doing and not the payload's.
        rt.process(
            &env(
                HANDOFF_MODE,
                "HandoffContext",
                "ctx-1",
                &sid,
                OWNER,
                handoff_context("h1"),
            ),
            None,
        )
        .await
        .expect("an ordinary id is accepted");
        assert_eq!(
            rt.log_store.get_log(&sid).await.unwrap().len(),
            history_before + 1
        );
    }

    /// Phase 11c criterion 1, start path. This call site exists precisely
    /// because the namespace is squattable through `SessionStart`, whose
    /// `message_id` enters `seen_message_ids` at the commit point — after
    /// which the runtime's own later synthesis would be silently skipped.
    ///
    /// The rejection must leave **no** trace: no session in the registry, no
    /// session log, and no reservation of the session id — proved by starting
    /// the same session id again with a clean `message_id` and having it
    /// succeed (`SessionAlreadyExists` would be the failure signature of a
    /// leaked reservation).
    #[tokio::test]
    async fn reserved_message_id_is_rejected_on_the_session_start_path() {
        let rt = make_runtime();
        let sid = new_sid();

        let err = rt
            .process(
                &env(
                    HANDOFF_MODE,
                    "SessionStart",
                    &reserved_id("h1"),
                    &sid,
                    OWNER,
                    handoff_start_payload(),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, MacpError::InvalidEnvelope),
            "reserved id on SessionStart must be InvalidEnvelope, got {err}"
        );

        // No session, no log, nothing to roll back.
        assert!(rt.get_session_checked(&sid).await.is_none());
        assert!(rt.log_store.get_log(&sid).await.is_none());
        assert!(!rt.registry.sessions.read().await.contains_key(&sid));

        // The session id was never reserved and the id never consumed a dedup
        // slot: the same session starts cleanly.
        rt.process(
            &env(
                HANDOFF_MODE,
                "SessionStart",
                "start-1",
                &sid,
                OWNER,
                handoff_start_payload(),
            ),
            None,
        )
        .await
        .expect("a rejected SessionStart must not reserve the session id");
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert!(session.seen_message_ids.contains("start-1"));
        assert!(!session.seen_message_ids.contains(&reserved_id("h1")));
    }

    /// Phase 11c criterion 2, runtime path: a client `HandoffAccept` carrying
    /// `implicit = true` never enters history.
    ///
    /// Two envelopes, as the criterion requires:
    /// (a) `implicit = true` with an ordinary `message_id` — `InvalidPayload`.
    ///     **This assertion is double-guarded**: `handle_message` rejects the
    ///     envelope at the client boundary before dispatch ever sees it
    ///     (`crates/macp-modes/src/mode/handoff.rs`, the client-envelope
    ///     validation that refuses a client-submitted `implicit = true`), and
    ///     — since 11d — `dispatch_implicit_accept`'s own reserved-`message_id`
    ///     check (`handoff.rs:733-740`) would refuse the same envelope on its
    ///     `message_id` alone even if the boundary check were removed. What it
    ///     pins is the criterion's actual requirement — that the rev-2 error
    ///     *surface* through `Send` is unchanged — not which single guard is
    ///     load-bearing.
    /// (b) `implicit = true` with the **reserved** `message_id` and the
    ///     correct sender — `InvalidEnvelope`, which only the boundary can
    ///     produce (dispatch would say `InvalidPayload`). The envelope is
    ///     byte-shaped exactly like the one the runtime synthesizes in
    ///     `synthesize_due_accept`.
    ///
    /// Measured, **neither half pins the `implicit` rule in isolation**: (b)
    /// is killed by the *reserved-prefix* rule, which fires first and returns
    /// `InvalidEnvelope` whatever the flag says, so deleting the `implicit`
    /// rule leaves this whole test green. The `implicit` rule's only
    /// non-vacuous guard is the mode-level unit test
    /// `handoff::tests::client_implicit_accept_rejected_at_the_boundary`.
    /// What this test pins is the runtime-level *error surface* at rev 2 —
    /// which is the criterion's requirement.
    #[tokio::test]
    async fn client_implicit_accept_rejected_through_the_runtime() {
        let rt = make_runtime();
        let sid = handoff_session_with_offer(&rt).await;
        let history_before = rt.log_store.get_log(&sid).await.unwrap().len();

        // (a) ordinary id.
        let err = rt
            .process(
                &env(
                    HANDOFF_MODE,
                    "HandoffAccept",
                    "accept-1",
                    &sid,
                    TARGET,
                    handoff_accept("h1", true),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MacpError::InvalidPayload), "got {err}");

        // (b) the runtime's own synthetic shape, submitted by the target.
        let err = rt
            .process(
                &env(
                    HANDOFF_MODE,
                    "HandoffAccept",
                    &reserved_id("h1"),
                    &sid,
                    TARGET,
                    handoff_accept("h1", true),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MacpError::InvalidEnvelope), "got {err}");

        // The offer is still outstanding and history is untouched.
        let session = rt.get_session_checked(&sid).await.unwrap();
        assert_eq!(
            rt.log_store.get_log(&sid).await.unwrap().len(),
            history_before
        );
        assert!(session.seen_message_ids.is_disjoint(
            &["accept-1".to_string(), reserved_id("h1")]
                .into_iter()
                .collect()
        ));
        let mode_state: serde_json::Value = serde_json::from_slice(&session.mode_state).unwrap();
        assert_eq!(mode_state["offers"]["h1"]["disposition"], "Offered");

        // Control: the explicit accept (`implicit = false`, ordinary id) is
        // accepted, so the rejections above are not the envelope's other
        // fields.
        rt.process(
            &env(
                HANDOFF_MODE,
                "HandoffAccept",
                "accept-2",
                &sid,
                TARGET,
                handoff_accept("h1", false),
            ),
            None,
        )
        .await
        .expect("an explicit accept is still accepted");
    }

    /// Error-code ordering at the kernel is unchanged by the phase: an
    /// envelope that is **both** unauthorized and carries a reserved id
    /// reports `Forbidden`, because the hook is called after
    /// `mode.authorize_sender`. Rev <= 1's Forbidden-before-payload ordering
    /// therefore does not shift at rev 2.
    #[tokio::test]
    async fn client_boundary_error_ordering_is_unchanged_at_rev2() {
        let rt = make_runtime();
        let sid = handoff_session_with_offer(&rt).await;

        // `agent://stranger` is not a declared participant.
        let err = rt
            .process(
                &env(
                    HANDOFF_MODE,
                    "HandoffAccept",
                    &reserved_id("h1"),
                    &sid,
                    "agent://stranger",
                    handoff_accept("h1", true),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, MacpError::Forbidden),
            "authorization must be reported before the client boundary, got {err}"
        );

        // Same envelope from the authorized sender: now the boundary speaks.
        let err = rt
            .process(
                &env(
                    HANDOFF_MODE,
                    "HandoffAccept",
                    &reserved_id("h1"),
                    &sid,
                    TARGET,
                    handoff_accept("h1", true),
                ),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, MacpError::InvalidEnvelope), "got {err}");
    }

    // --- The synthesis seam's own guards (11e) ---

    /// A handoff session with a bound `implicit_accept_timeout_ms` and an
    /// outstanding offer. Unlike [`handoff_session_with_offer`] this one binds
    /// a policy, so an implicit accept can actually become due.
    async fn handoff_session_with_timed_offer(rt: &Runtime, timeout_ms: i64) -> String {
        rt.register_policy(macp_core::policy::PolicyDefinition {
            policy_id: "handoff-timed".into(),
            mode: HANDOFF_MODE.into(),
            description: "implicit accept".into(),
            rules: serde_json::json!({
                "acceptance": { "implicit_accept_timeout_ms": timeout_ms },
                "commitment": { "authority": "initiator_only" }
            }),
            schema_version: 1,
        })
        .expect("policy registers");

        let sid = new_sid();
        let start = SessionStartPayload {
            intent: "escalate".into(),
            participants: vec![OWNER.into(), TARGET.into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: "handoff-timed".into(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        rt.process(
            &env(HANDOFF_MODE, "SessionStart", "start-1", &sid, OWNER, start),
            None,
        )
        .await
        .expect("session start");
        rt.process(
            &env(
                HANDOFF_MODE,
                "HandoffOffer",
                "offer-1",
                &sid,
                OWNER,
                handoff_offer("h1"),
            ),
            None,
        )
        .await
        .expect("offer");
        sid
    }

    /// Phase 11e criterion 11, first half: `synthesize_due_accept` must refuse
    /// to synthesize for a session that is not `Open`.
    ///
    /// This is a **correctness** gate, not hygiene, and it is the only one
    /// that ships. Both computations behind the synthetic entry ignore an
    /// in-flight pause by design — `Session::unsuspended_deadline` walks
    /// completed intervals only, and `HandoffMode::rev2_elapsed_ms` has no
    /// in-flight term — so asking a `Suspended` session over-counts elapsed
    /// time (the accept can be judged due when it is not) and under-computes
    /// `D` (a wrong `timestamp_unix_ms` baked into permanent history, in
    /// silence). The mode carries a `debug_assert!` for the same thing, which
    /// compiles out in release builds and therefore guards nothing where it
    /// matters.
    ///
    /// The method is called directly because no message path can reach it with
    /// a suspended session — `step::check_preconditions` rejects every message
    /// to a non-`Open` session first. The eager sweep (`sweep_due_synthetic_accepts`)
    /// is the caller that *does* see suspended sessions, which is exactly why
    /// this filter has to live in the seam rather than at a single call site.
    #[tokio::test]
    async fn synthesis_is_skipped_for_a_non_open_session() {
        let rt = make_runtime();
        let sid = handoff_session_with_timed_offer(&rt, 20).await;
        rt.suspend_session(&sid, "hold", OWNER)
            .await
            .expect("suspend");

        let shared = rt.registry.get_shared(&sid).await.unwrap();
        let mut guard = shared.lock().await;
        let session = &mut *guard;
        assert_eq!(session.state, SessionState::Suspended);
        assert!(session.suspended_at_ms.is_some());

        // Far past the deadline on wall time — the only thing stopping a
        // synthesis here is the state filter.
        let long_after = session.suspended_at_ms.unwrap() + 10_000;
        let log_before = rt.log_store.get_log(&sid).await.unwrap_or_default().len();
        let dedup_before = session.seen_message_ids.len();
        let mode_state_before = session.mode_state.clone();

        rt.synthesize_due_accept(&sid, session, long_after)
            .await
            .expect("the filter is a skip, not an error");

        assert_eq!(
            rt.log_store.get_log(&sid).await.unwrap_or_default().len(),
            log_before,
            "a suspended session must not gain a synthetic entry"
        );
        assert_eq!(session.seen_message_ids.len(), dedup_before);
        assert_eq!(session.mode_state, mode_state_before);

        // Control: the filter is what declined, not the arithmetic — and what
        // it kept out of history was wrong, not merely early.
        //
        // The mode cannot be asked about the genuinely suspended session in a
        // debug build: `due_synthetic_envelope` carries a `debug_assert!` on
        // `suspended_at_ms.is_none()` and would panic. So the probe is a clone
        // that differs *only* by that tripwire — same offer, same banked
        // suspension (none: the pause is still in flight and banks on resume),
        // same clock. That is exactly the state the mode would see if the
        // kernel filter were removed and the assertion compiled out, which is
        // what a release build does.
        let mut without_the_tripwire = session.clone();
        without_the_tripwire.state = SessionState::Open;
        without_the_tripwire.suspended_at_ms = None;
        let mode = rt.mode_registry.get_mode(&session.mode).unwrap();
        let would_have_emitted = mode
            .due_synthetic_envelope(&without_the_tripwire, long_after)
            .expect("the mode would have synthesized; only the kernel filter stopped it");
        // The harm, concretely: the D it would have recorded falls *inside* the
        // pause that is still running — a moment at which the session was not
        // ticking at all. `unsuspended_deadline` walks completed intervals
        // only, and this pause is not completed, so it is invisible to the
        // walk. Recorded, that timestamp would be permanent and wrong.
        let suspended_at = session.suspended_at_ms.unwrap();
        assert!(
            would_have_emitted.timestamp_unix_ms >= suspended_at
                && would_have_emitted.timestamp_unix_ms < long_after,
            "D {} must fall inside the still-open pause starting at {suspended_at}",
            would_have_emitted.timestamp_unix_ms
        );

        // And once the session is Open again the seam works normally, so the
        // filter is a skip rather than a permanent disable.
        drop(guard);
        rt.resume_session(&sid, "go", OWNER).await.expect("resume");
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let shared = rt.registry.get_shared(&sid).await.unwrap();
        let mut guard = shared.lock().await;
        let session = &mut *guard;
        let now = Utc::now().timestamp_millis();
        rt.synthesize_due_accept(&sid, session, now).await.unwrap();
        assert!(session.seen_message_ids.contains(&reserved_id("h1")));
    }

    /// Phase 11e criterion 11, second half: the appended entry stamps
    /// `received_at_ms` with the envelope's own `timestamp_unix_ms` (the
    /// deadline `D`), never wall-clock.
    ///
    /// Asserted on the stamped value directly, at the seam, rather than
    /// inferred from a replay — a replay would pass under a wall-clock stamp
    /// too, because handoff's accept arm is time-blind. The contract is
    /// general: replay derives its dispatch clock from `received_at_ms`, so
    /// the next mode to use this hook would silently diverge.
    #[tokio::test]
    async fn synthetic_entry_stamps_received_at_with_the_deadline() {
        let rt = make_runtime();
        let sid = handoff_session_with_timed_offer(&rt, 20).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let offer_received_at = rt
            .log_store
            .get_log(&sid)
            .await
            .unwrap()
            .iter()
            .find(|e| e.message_type == "HandoffOffer")
            .expect("offer entry")
            .received_at_ms;
        let expected_d = offer_received_at + 20;

        let shared = rt.registry.get_shared(&sid).await.unwrap();
        let mut guard = shared.lock().await;
        let session = &mut *guard;
        // Observed far later than D, so a wall-clock stamp would be obvious.
        let observed = Utc::now().timestamp_millis();
        assert!(observed > expected_d);
        rt.synthesize_due_accept(&sid, session, observed)
            .await
            .unwrap();
        drop(guard);

        let entry = rt
            .log_store
            .get_log(&sid)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.message_id == reserved_id("h1"))
            .expect("the synthetic entry");
        assert_eq!(entry.timestamp_unix_ms, expected_d, "envelope clock is D");
        assert_eq!(entry.received_at_ms, expected_d, "entry clock is D");
        assert_ne!(
            entry.received_at_ms, observed,
            "received_at_ms must not be the observation time"
        );
        assert_eq!(entry.entry_kind, EntryKind::Incoming);
    }

    /// The synthetic accept is deliberately NOT credited as participant
    /// activity: `replay_entry` never records activity for any entry kind, so
    /// calling `record_participant_activity` live would guarantee a
    /// live/replay divergence in `participant_message_counts` — and the target
    /// did not, in fact, send anything.
    ///
    /// The consequence is user-visible (`SessionMetadata.participant_activity`
    /// via `server::session_to_metadata`), so it is pinned rather than left as
    /// a comment.
    #[tokio::test]
    async fn synthetic_accept_is_not_credited_as_participant_activity() {
        let rt = make_runtime();
        let sid = handoff_session_with_timed_offer(&rt, 20).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let shared = rt.registry.get_shared(&sid).await.unwrap();
        let mut guard = shared.lock().await;
        let session = &mut *guard;
        let before = session.participant_message_counts.get(TARGET).copied();
        rt.synthesize_due_accept(&sid, session, Utc::now().timestamp_millis())
            .await
            .unwrap();
        assert!(session.seen_message_ids.contains(&reserved_id("h1")));
        assert_eq!(
            session.participant_message_counts.get(TARGET).copied(),
            before,
            "the target must not be credited with a message they did not send"
        );
    }
    /// The checkpoint-interval check belongs to the *append*, so the eager
    /// sweep and a lazy trigger place the same checkpoint in the same
    /// position.
    ///
    /// `maybe_insert_checkpoint` used to be called only from the tail of
    /// `process_message`. A synthetic entry advances `log_len` without ever
    /// reaching it: the lazy path checked only the length *after* the
    /// trigger's own append (one greater), and the eager path — where
    /// `sweep_due_synthetic_accepts` is the whole call — checked nothing at
    /// all. With `MACP_CHECKPOINT_INTERVAL > 0` that is a genuine eager/lazy
    /// divergence: identical accepted histories, different checkpoint
    /// placement, and a boundary the synthetic crossed silently skipped.
    /// Calling it from inside `synthesize_due_accept` makes the two agree by
    /// construction, which is what this pins.
    ///
    /// Checkpoints are a replay optimization rather than a correctness
    /// property, which is exactly why this needs a test: nothing else would
    /// ever notice.
    ///
    /// `checkpoint_interval` is set on the struct rather than through
    /// `MACP_CHECKPOINT_INTERVAL`, because that variable is read once in
    /// `Runtime::with_registries` and this binary runs its tests in parallel —
    /// setting it here would leak into every other runtime built concurrently.
    ///
    /// Interval 3, with `SessionStart` + `HandoffOffer` already logged, puts
    /// the synthetic accept exactly on the boundary: the skipped case.
    #[tokio::test]
    async fn a_synthetic_entry_on_the_checkpoint_boundary_checkpoints_either_path() {
        async fn shape(rt: &Runtime, sid: &str) -> Vec<(EntryKind, String)> {
            rt.log_store
                .get_log(sid)
                .await
                .expect("log")
                .iter()
                .map(|e| (e.entry_kind.clone(), e.message_type.clone()))
                .collect()
        }

        // Eager: the sweep is the only thing that touches the session.
        let mut eager = make_runtime();
        eager.checkpoint_interval = 3;
        let eager_sid = handoff_session_with_timed_offer(&eager, 20).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(
            eager.sweep_due_synthetic_accepts().await,
            1,
            "sweep emitted"
        );

        // Lazy: a trigger message arrives instead. `HandoffContext` is the one
        // mode message the offerer may send at any disposition, so it provokes
        // the synthesis without being an accept itself.
        let mut lazy = make_runtime();
        lazy.checkpoint_interval = 3;
        let lazy_sid = handoff_session_with_timed_offer(&lazy, 20).await;
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        lazy.process(
            &env(
                HANDOFF_MODE,
                "HandoffContext",
                "ctx-1",
                &lazy_sid,
                OWNER,
                handoff_context("h1"),
            ),
            None,
        )
        .await
        .expect("context accepted");

        let expected = vec![
            (EntryKind::Incoming, "SessionStart".to_string()),
            (EntryKind::Incoming, "HandoffOffer".to_string()),
            (EntryKind::Incoming, "HandoffAccept".to_string()),
            (EntryKind::Checkpoint, "Checkpoint".to_string()),
        ];
        assert_eq!(shape(&eager, &eager_sid).await, expected, "eager sweep");

        let lazy_shape = shape(&lazy, &lazy_sid).await;
        assert_eq!(
            lazy_shape[..4],
            expected[..],
            "the lazy path must checkpoint in the same place as the eager one"
        );
        // ... and exactly once: `process_message` runs its own check after
        // appending the trigger, at a length the synthesis never tested.
        assert_eq!(
            lazy_shape[4..],
            [(EntryKind::Incoming, "HandoffContext".to_string())],
            "no second checkpoint for the trigger's own append"
        );
    }
}
