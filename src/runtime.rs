use chrono::Utc;
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
    extract_ttl_ms, parse_session_start_payload, validate_canonical_session_start_payload,
    validate_session_id_for_acceptance, Session, SessionState,
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
        }
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
                    "1.0".into()
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

    /// Whether the session's bound policy requests info-level per-message
    /// audit lines (`rules.audit.level == "info"`).
    fn audit_verbose(session: &Session) -> bool {
        session
            .policy_definition
            .as_ref()
            .and_then(|p| p.rules.get("audit"))
            .and_then(|a| a.get("level"))
            .and_then(|l| l.as_str())
            == Some("info")
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
            compacted_incoming_ordinals: 0,
        }
    }

    fn make_internal_entry(
        message_type: &str,
        payload: &[u8],
        session_id: &str,
        mode: &str,
    ) -> LogEntry {
        let now = Utc::now().timestamp_millis();
        LogEntry {
            message_id: String::new(),
            received_at_ms: now,
            sender: "_runtime".into(),
            message_type: message_type.into(),
            raw_payload: payload.to_vec(),
            entry_kind: EntryKind::Internal,
            session_id: session_id.into(),
            mode: mode.into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: now,
            bound_mode_version: None,
            semantics_rev: 0,
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
        let now = Utc::now().timestamp_millis();
        // An Open session past its deadline, or a Suspended session that has
        // exceeded the MAX_SUSPEND_MS cap (RFC-MACP-0001 §7.5), expires.
        let expires = (session.state == SessionState::Open && now > session.ttl_expiry)
            || (session.state == SessionState::Suspended && session.suspend_cap_exceeded(now));
        if expires {
            let entry = Self::make_internal_entry("TtlExpired", b"", session_id, &session.mode);
            self.storage
                .append_log_entry(session_id, &entry)
                .await
                .map_err(|_| MacpError::StorageFailed)?;
            self.log_store.append(session_id, entry).await;
            session.state = SessionState::Expired;
            session.suspended_at_ms = None;
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
        if env.mode.trim().is_empty() {
            return Err(MacpError::InvalidEnvelope);
        }
        validate_session_id_for_acceptance(&env.session_id)?;
        let mode_name = env.mode.as_str();
        let mode = self
            .mode_registry
            .get_mode(mode_name)
            .ok_or(MacpError::UnknownMode)?;

        let start_payload = parse_session_start_payload(&env.payload)?;
        let require_complete_start = self.mode_registry.requires_strict_session_start(mode_name);
        if require_complete_start {
            validate_canonical_session_start_payload(&start_payload)?;
        }

        // Validate mode_version matches the registered descriptor's version.
        // When the payload omits mode_version (only possible for non-strict
        // extension modes), bind the descriptor's version instead of leaving the
        // session bound to "" — an empty binding makes the Commitment version
        // check vacuous (any commitment with mode_version "" would match).
        // The bound value is recorded on the SessionStart log entry so replay
        // uses the recorded binding, never the live registry.
        let descriptor_version = self.mode_registry.get_mode_version(mode_name);
        if let Some(descriptor_version) = &descriptor_version {
            if !start_payload.mode_version.is_empty()
                && &start_payload.mode_version != descriptor_version
            {
                tracing::warn!(
                    mode = mode_name,
                    payload_version = %start_payload.mode_version,
                    descriptor_version = %descriptor_version,
                    "mode_version mismatch"
                );
                return Err(MacpError::InvalidEnvelope);
            }
        }
        let bound_mode_version: Option<String> = if start_payload.mode_version.is_empty() {
            descriptor_version
        } else {
            None
        };
        let effective_mode_version = bound_mode_version
            .clone()
            .unwrap_or_else(|| start_payload.mode_version.clone());

        let ttl_ms = extract_ttl_ms(&start_payload)?;

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

        // Resolve the governance policy for this session.
        // RFC-MACP-0012 §6.1: policy_version is resolved at SessionStart; empty
        // resolves to "policy.default". The resolved PolicyDescriptor is stored
        // immutably on the session for deterministic replay (RFC-MACP-0003 §3).
        let effective_policy_version = if start_payload.policy_version.is_empty() {
            crate::policy::defaults::DEFAULT_POLICY_ID.to_string()
        } else {
            start_payload.policy_version.clone()
        };
        let policy_definition = match self.policy_registry.resolve(&effective_policy_version) {
            Ok(policy) => {
                // RFC 6.1: reject if policy mode doesn't match session mode
                if policy.mode != "*" && policy.mode != mode_name {
                    return Err(MacpError::InvalidPolicyDefinition);
                }
                Some(policy)
            }
            Err(_) => {
                return Err(MacpError::UnknownPolicyVersion);
            }
        };

        let accepted_at = Utc::now().timestamp_millis();
        // RFC-MACP-0003 §2: TTL deadline is computed from the SessionStart
        // envelope's timestamp_unix_ms, not wall-clock time. This ensures
        // deterministic replay. Fall back to accepted_at if envelope has no timestamp.
        let ttl_base = if env.timestamp_unix_ms > 0 {
            env.timestamp_unix_ms
        } else {
            accepted_at
        };
        let ttl_expiry = ttl_base.saturating_add(ttl_ms);
        let session = Session::builder(env.session_id.clone(), mode_name, env.sender.clone())
            .ttl_expiry(ttl_expiry)
            .ttl_ms(ttl_ms)
            .started_at_unix_ms(accepted_at)
            .participants(start_payload.participants.clone())
            .intent(start_payload.intent.clone())
            .mode_version(effective_mode_version)
            .configuration_version(start_payload.configuration_version.clone())
            .policy_version(effective_policy_version)
            .context_id(start_payload.context_id.clone())
            .extensions(start_payload.extensions.clone())
            .roots(start_payload.roots.clone())
            .policy_definition(policy_definition)
            .build();

        let response = mode.on_session_start(&session, env)?;
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
                let now = Utc::now().timestamp_millis();
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

        session_guard
            .seen_message_ids
            .insert(env.message_id.clone());
        session_guard.apply_mode_response(response);

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
        let now_ms = chrono::Utc::now().timestamp_millis();
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
                // `maybe_expire_session` share the same strict `>`, OPEN-guarded
                // rule, so this always expires.
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
            .ok_or(MacpError::UnknownMode)?;
        mode.authorize_sender(session, env)?;
        // One acceptance clock for both the mode call and the log entry, so
        // replay (which re-reads received_at_ms) observes the identical time.
        let accepted_at_ms = Utc::now().timestamp_millis();
        let response = mode.on_message_at(
            session,
            env,
            &macp_core::mode::MessageContext::new(accepted_at_ms),
        )?;

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

        // Policy-driven audit verbosity (E3b): a bound policy may request
        // per-message audit lines at info level via an `audit.level` rules
        // block ("info"); default stays debug. Mode rule schemas ignore
        // unknown blocks, so `audit` composes with any mode's rules.
        if Self::audit_verbose(session) {
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
        let cancel_payload = crate::pb::SessionCancelPayload {
            reason: reason.to_string(),
            cancelled_by: cancelled_by.to_string(),
        };
        let cancel_entry = Self::make_internal_entry(
            "SessionCancel",
            &prost::Message::encode_to_vec(&cancel_payload),
            session_id,
            &session.mode,
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

        let now_ms = chrono::Utc::now().timestamp_millis();
        let payload = crate::pb::SessionSuspendPayload {
            reason: reason.to_string(),
            suspended_by: suspended_by.to_string(),
        };
        let entry = Self::make_internal_entry(
            "SessionSuspend",
            &prost::Message::encode_to_vec(&payload),
            session_id,
            &session.mode,
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

        let now_ms = chrono::Utc::now().timestamp_millis();
        let banked_before = session
            .suspended_at_ms
            .map(|at| (now_ms - at).max(0))
            .unwrap_or(0);
        let payload = crate::pb::SessionResumePayload {
            reason: reason.to_string(),
            resumed_by: resumed_by.to_string(),
            banked_ms: banked_before,
        };
        let entry = Self::make_internal_entry(
            "SessionResume",
            &prost::Message::encode_to_vec(&payload),
            session_id,
            &session.mode,
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
                // MAX_SUSPEND_MS exceeded: the session is now Expired.
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
        let now = Utc::now().timestamp_millis();
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
        let now = Utc::now().timestamp_millis();
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
            let entry = Self::make_internal_entry("TtlExpired", b"", &session_id, &session.mode);
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

    /// Delete terminal sessions' durable data older than `retention_secs`
    /// (opt-in via `MACP_SESSION_DISK_RETENTION_SECS`). Before this existed,
    /// `storage.delete_session` had no callers at all: disk grew without
    /// bound and every restart reloaded every session ever completed.
    /// Enumerates STORAGE (not memory — eviction may already have dropped the
    /// registry entry), deletes the session's snapshot+log, and clears any
    /// in-memory remnants. Returns the number of sessions deleted.
    pub async fn gc_disk_sessions(&self, retention_secs: u64) -> usize {
        let now = Utc::now().timestamp_millis();
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
            match self.storage.delete_session(&id).await {
                Ok(()) => {
                    {
                        let mut guard = self.registry.sessions.write().await;
                        guard.remove(&id);
                    }
                    self.log_store.remove_session_log(&id).await;
                    let _ = self.stream_bus.remove_if_unused(&id);
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
        let now = Utc::now().timestamp_millis();
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

    #[test]
    fn audit_verbosity_reads_policy_rules() {
        let mut session = Session::builder("s1", "macp.mode.decision.v1", "a").build();
        assert!(!Runtime::audit_verbose(&session));

        session.policy_definition = Some(macp_core::policy::PolicyDefinition {
            policy_id: "policy.test.audit".into(),
            mode: "*".into(),
            description: "audited".into(),
            rules: serde_json::json!({ "audit": { "level": "info" } }),
            schema_version: 1,
        });
        assert!(Runtime::audit_verbose(&session));

        session.policy_definition.as_mut().unwrap().rules =
            serde_json::json!({ "audit": { "level": "debug" } });
        assert!(!Runtime::audit_verbose(&session));
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
}
