use crate::error::MacpError;
use crate::log_store::{EntryKind, LogEntry};
use crate::mode_registry::ModeRegistry;
use crate::pb::Envelope;
use crate::policy::registry::PolicyRegistry;
use crate::registry::PersistedSession;
use crate::session::{
    extract_ttl_ms, parse_session_start_payload, validate_canonical_session_start_payload, Session,
    SessionState,
};

/// Rebuild a `Session` from its append-only log.
///
/// If the log contains `Checkpoint` entries, replay starts from the last
/// checkpoint (restoring the serialized session state) and only replays
/// subsequent entries. Otherwise, a full replay from `SessionStart` is
/// performed.
pub fn replay_session(
    session_id: &str,
    log_entries: &[LogEntry],
    registry: &ModeRegistry,
    policy_registry: Option<&PolicyRegistry>,
) -> Result<Session, MacpError> {
    // Try checkpoint-based fast path first
    if let Some(session) =
        try_replay_from_checkpoint(session_id, log_entries, registry, policy_registry)?
    {
        return Ok(session);
    }

    replay_from_start(session_id, log_entries, registry, policy_registry)
}

/// Attempt to restore from the last checkpoint entry and replay remaining entries.
/// Returns `Ok(None)` if no checkpoint exists.
fn try_replay_from_checkpoint(
    session_id: &str,
    log_entries: &[LogEntry],
    registry: &ModeRegistry,
    _policy_registry: Option<&PolicyRegistry>,
) -> Result<Option<Session>, MacpError> {
    let checkpoint_idx = log_entries
        .iter()
        .rposition(|e| e.entry_kind == EntryKind::Checkpoint);

    let idx = match checkpoint_idx {
        Some(idx) => idx,
        None => return Ok(None),
    };

    let checkpoint = &log_entries[idx];
    let persisted: PersistedSession =
        serde_json::from_slice(&checkpoint.raw_payload).map_err(|_| MacpError::InvalidPayload)?;
    let mut session = Session::from(persisted);
    session.session_id = session_id.into();

    // Re-resolve policy definition if policy_version is bound but missing from checkpoint.
    // This can happen with legacy checkpoints. The resolved definition may differ from the
    // original if the policy was modified since the session started (RFC-MACP-0012 Section 8).
    // Policy definitions MUST be serialized in checkpoint entries. Any checkpoint
    // missing a policy definition was created by a legacy version and cannot be
    // trusted for deterministic replay — fall back to full replay from SessionStart.
    if !session.policy_version.is_empty() && session.policy_definition.is_none() {
        tracing::warn!(
            session_id,
            policy_version = %session.policy_version,
            "checkpoint missing policy_definition; falling back to full replay for deterministic policy resolution"
        );
        return Ok(None);
    }

    let mode = registry
        .get_mode(&session.mode)
        .ok_or(MacpError::UnknownMode)?;

    // Replay entries after the checkpoint
    for entry in &log_entries[idx + 1..] {
        replay_entry(&mut session, session_id, entry, &mode)?;
    }

    Ok(Some(session))
}

/// Replay a single log entry onto a session.
fn replay_entry(
    session: &mut Session,
    session_id: &str,
    entry: &LogEntry,
    mode: &crate::mode_registry::ModeRef<'_>,
) -> Result<(), MacpError> {
    match entry.entry_kind {
        EntryKind::Incoming => {
            let replay_env = Envelope {
                macp_version: if entry.macp_version.is_empty() {
                    "1.0".into()
                } else {
                    entry.macp_version.clone()
                },
                mode: if entry.mode.is_empty() {
                    session.mode.clone()
                } else {
                    entry.mode.clone()
                },
                message_type: entry.message_type.clone(),
                message_id: entry.message_id.clone(),
                session_id: session_id.into(),
                sender: entry.sender.clone(),
                // Use original envelope timestamp for replay determinism;
                // fall back to received_at_ms for legacy log entries.
                timestamp_unix_ms: if entry.timestamp_unix_ms != 0 {
                    entry.timestamp_unix_ms
                } else {
                    entry.received_at_ms
                },
                payload: entry.raw_payload.clone(),
            };

            if session.state != SessionState::Open {
                if !replay_env.message_id.is_empty() {
                    session.seen_message_ids.insert(replay_env.message_id);
                }
                return Ok(());
            }

            mode.authorize_sender(session, &replay_env)?;
            // The acceptance clock replays as the recorded `received_at_ms`
            // (the same value the live path passed), never wall-clock.
            let ctx = macp_core::mode::MessageContext::new(if entry.received_at_ms != 0 {
                entry.received_at_ms
            } else {
                entry.timestamp_unix_ms
            });
            let response = mode.on_message_at(session, &replay_env, &ctx)?;
            session.apply_mode_response(response);
            if !replay_env.message_id.is_empty() {
                session.seen_message_ids.insert(replay_env.message_id);
            }
        }
        EntryKind::Internal => match entry.message_type.as_str() {
            "TtlExpired" => {
                session.state = SessionState::Expired;
            }
            // RFC-MACP-0001 §7.3: cancellation replays to the terminal CANCELLED
            // state (distinct from EXPIRED).
            "SessionCancel" => {
                let _ = session.cancel();
            }
            // RFC-MACP-0001 §7.5 / RFC-MACP-0003 §2: suspend/resume are on the
            // replayed timeline; banking uses the recorded entry timestamp so a
            // suspended-then-resumed session replays to the identical deadline.
            "SessionSuspend" => {
                let at = if entry.received_at_ms != 0 {
                    entry.received_at_ms
                } else {
                    entry.timestamp_unix_ms
                };
                let _ = session.suspend(at);
            }
            "SessionResume" => {
                let at = if entry.received_at_ms != 0 {
                    entry.received_at_ms
                } else {
                    entry.timestamp_unix_ms
                };
                let _ = session.resume(at);
            }
            _ => {}
        },
        EntryKind::Checkpoint => {
            // Skip intermediate checkpoints when replaying from an earlier one
        }
    }
    Ok(())
}

/// Warn-only replay/snapshot divergence check (D7, promoted from
/// plans/defer/replay_validation.md). The log is authoritative and snapshots
/// are best-effort, so a mismatch is diagnostic, never fatal — but state or
/// dedup-count divergence between "what the log replays to" and "what the
/// snapshot recorded" is exactly the class of bug the determinism guarantees
/// (RFC-MACP-0003) forbid, so it must be visible. Returns the number of
/// mismatched fields (0 = consistent).
pub fn validate_replay_consistency(
    session_id: &str,
    replayed: &Session,
    snapshot: &Session,
) -> u32 {
    let mut mismatches = 0u32;
    if replayed.state != snapshot.state {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_state = ?replayed.state,
            snapshot_state = ?snapshot.state,
            "replay/snapshot state mismatch"
        );
    }
    if replayed.seen_message_ids.len() != snapshot.seen_message_ids.len() {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_dedup = replayed.seen_message_ids.len(),
            snapshot_dedup = snapshot.seen_message_ids.len(),
            "replay/snapshot dedup count mismatch"
        );
    }
    if replayed.participants != snapshot.participants {
        mismatches += 1;
        tracing::warn!(session_id, "replay/snapshot participants mismatch");
    }
    if replayed.mode_version != snapshot.mode_version
        || replayed.configuration_version != snapshot.configuration_version
        || replayed.policy_version != snapshot.policy_version
    {
        mismatches += 1;
        tracing::warn!(
            session_id,
            "replay/snapshot bound-version mismatch (mode/configuration/policy)"
        );
    }
    mismatches
}

/// Full replay from the SessionStart entry.
fn replay_from_start(
    session_id: &str,
    log_entries: &[LogEntry],
    registry: &ModeRegistry,
    policy_registry: Option<&PolicyRegistry>,
) -> Result<Session, MacpError> {
    // 1. Find the SessionStart entry
    let start_entry = log_entries
        .iter()
        .find(|e| e.entry_kind == EntryKind::Incoming && e.message_type == "SessionStart")
        .ok_or(MacpError::InvalidPayload)?;

    // Determine mode: prefer entry-level field, fall back to empty for legacy
    let mode_name = if start_entry.mode.is_empty() {
        // Legacy v2 entry — cannot determine mode from log entry alone;
        // caller should skip or use directory heuristic
        return Err(MacpError::InvalidPayload);
    } else {
        &start_entry.mode
    };

    let mode = registry.get_mode(mode_name).ok_or(MacpError::UnknownMode)?;

    // 2. Parse SessionStartPayload
    let require_complete_start = registry.requires_strict_session_start(mode_name);
    let start_payload = if start_entry.raw_payload.is_empty() && !require_complete_start {
        crate::pb::SessionStartPayload::default()
    } else {
        parse_session_start_payload(&start_entry.raw_payload)?
    };
    if require_complete_start {
        validate_canonical_session_start_payload(&start_payload)?;
    }

    let ttl_ms = if !require_complete_start && start_payload.ttl_ms == 0 {
        // Legacy experimental modes may have 0 ttl_ms
        60_000i64
    } else {
        extract_ttl_ms(&start_payload)?
    };

    // 3. Construct base session — use original received_at_ms, never Utc::now()
    let started_at_unix_ms = start_entry.received_at_ms;
    let ttl_expiry = started_at_unix_ms.saturating_add(ttl_ms);

    let env = Envelope {
        macp_version: if start_entry.macp_version.is_empty() {
            "1.0".into()
        } else {
            start_entry.macp_version.clone()
        },
        mode: mode_name.to_string(),
        message_type: "SessionStart".into(),
        message_id: start_entry.message_id.clone(),
        session_id: session_id.into(),
        sender: start_entry.sender.clone(),
        timestamp_unix_ms: if start_entry.timestamp_unix_ms != 0 {
            start_entry.timestamp_unix_ms
        } else {
            start_entry.received_at_ms
        },
        payload: start_entry.raw_payload.clone(),
    };

    let mut session = Session::builder(session_id, mode_name, start_entry.sender.clone())
        // Replay under the semantics revision the session was accepted with
        // (legacy entries record 0 via serde default).
        .semantics_rev(start_entry.semantics_rev)
        // Suspension cap recorded at acceptance; legacy entries (None) load
        // as 0 = default-cap semantics, matching how they were accepted.
        .max_suspend_ms(start_entry.bound_max_suspend_ms.unwrap_or(0))
        .ttl_expiry(ttl_expiry)
        .ttl_ms(ttl_ms)
        .started_at_unix_ms(started_at_unix_ms)
        .participants(start_payload.participants.clone())
        .intent(start_payload.intent.clone())
        // Use the binding recorded at acceptance time when present (extension
        // modes whose SessionStart payload omitted mode_version). Never re-derive
        // from the live registry — dynamic registrations may have changed or be
        // absent after restart. Legacy entries (None) keep the payload's value,
        // preserving their original (possibly empty) binding semantics.
        .mode_version(
            start_entry
                .bound_mode_version
                .clone()
                .unwrap_or_else(|| start_payload.mode_version.clone()),
        )
        .configuration_version(start_payload.configuration_version.clone())
        .policy_version(start_payload.policy_version.clone())
        .context_id(start_payload.context_id.clone())
        .extensions(start_payload.extensions.clone())
        .roots(start_payload.roots.clone())
        .policy_definition(if !start_payload.policy_version.is_empty() {
            policy_registry.and_then(|pr| pr.resolve(&start_payload.policy_version).ok())
        } else {
            None
        })
        .build();

    // 4. Call mode.on_session_start(), apply response
    let response = mode.on_session_start(&session, &env)?;
    session.seen_message_ids.insert(env.message_id.clone());
    session.apply_mode_response(response);

    // 5. Replay subsequent entries
    for entry in log_entries.iter().skip(1) {
        replay_entry(&mut session, session_id, entry, &mode)?;
    }

    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision_pb::ProposalPayload;
    use crate::decision_pb::VotePayload;
    use crate::log_store::EntryKind;
    use crate::pb::{CommitmentPayload, SessionStartPayload};
    use prost::Message;

    fn make_registry() -> ModeRegistry {
        ModeRegistry::build_default(std::sync::Arc::new(macp_policy::DefaultPolicyEvaluator))
    }

    fn start_payload_bytes() -> Vec<u8> {
        SessionStartPayload {
            intent: "test".into(),
            participants: vec!["agent://orchestrator".into(), "agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: "policy-1".into(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec()
    }

    fn incoming_entry(
        message_id: &str,
        message_type: &str,
        sender: &str,
        payload: Vec<u8>,
        received_at_ms: i64,
    ) -> LogEntry {
        LogEntry {
            message_id: message_id.into(),
            received_at_ms,
            sender: sender.into(),
            message_type: message_type.into(),
            raw_payload: payload,
            entry_kind: EntryKind::Incoming,
            session_id: "s1".into(),
            mode: "macp.mode.decision.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: received_at_ms,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    fn internal_entry(message_type: &str, received_at_ms: i64) -> LogEntry {
        LogEntry {
            message_id: String::new(),
            received_at_ms,
            sender: "_runtime".into(),
            message_type: message_type.into(),
            raw_payload: vec![],
            entry_kind: EntryKind::Internal,
            session_id: "s1".into(),
            mode: "macp.mode.decision.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: received_at_ms,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    #[test]
    fn replay_rebuilds_decision_session() {
        let registry = make_registry();
        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "deploy".into(),
            rationale: "ready".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();
        let vote = VotePayload {
            proposal_id: "p1".into(),
            vote: "approve".into(),
            reason: "lgtm".into(),
        }
        .encode_to_vec();
        let commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.selected".into(),
            authority_scope: "payments".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "policy-1".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();

        let entries = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload_bytes(),
                1000,
            ),
            incoming_entry("m2", "Proposal", "agent://orchestrator", proposal, 2000),
            incoming_entry("m3", "Vote", "agent://fraud", vote, 3000),
            incoming_entry("m4", "Commitment", "agent://orchestrator", commitment, 4000),
        ];

        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.state, SessionState::Resolved);
        assert_eq!(session.session_id, "s1");
        assert!(session.seen_message_ids.contains("m1"));
        assert!(session.seen_message_ids.contains("m2"));
        assert!(session.seen_message_ids.contains("m3"));
        assert!(session.seen_message_ids.contains("m4"));
        assert!(session.resolution.is_some());
    }

    #[test]
    fn replay_preserves_original_ttl() {
        let registry = make_registry();
        let original_time = 1_700_000_000_000i64;
        let entries = vec![incoming_entry(
            "m1",
            "SessionStart",
            "agent://orchestrator",
            start_payload_bytes(),
            original_time,
        )];

        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.started_at_unix_ms, original_time);
        assert_eq!(session.ttl_expiry, original_time + 60_000);
        assert_eq!(session.ttl_ms, 60_000);
    }

    #[test]
    fn replay_handles_ttl_expired() {
        let registry = make_registry();
        let entries = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload_bytes(),
                1000,
            ),
            internal_entry("TtlExpired", 61001),
        ];

        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.state, SessionState::Expired);
    }

    #[test]
    fn replay_handles_session_cancel() {
        let registry = make_registry();
        let entries = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload_bytes(),
                1000,
            ),
            internal_entry("SessionCancel", 5000),
        ];

        let session = replay_session("s1", &entries, &registry, None).unwrap();
        // RFC-MACP-0001 §7.3: cancellation now terminates as CANCELLED.
        assert_eq!(session.state, SessionState::Cancelled);
    }

    #[test]
    fn replay_fails_when_accepted_history_no_longer_applies() {
        let registry = make_registry();
        let vote = VotePayload {
            proposal_id: "p1".into(),
            vote: "approve".into(),
            reason: String::new(),
        }
        .encode_to_vec();
        let entries = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload_bytes(),
                1000,
            ),
            incoming_entry("m2", "Vote", "agent://fraud", vote, 2000),
        ];

        let err = replay_session("s1", &entries, &registry, None).unwrap_err();
        // The exact error variant depends on which check fails first (authorize_sender
        // or on_message); what matters is that replay does NOT silently succeed.
        let msg = err.to_string();
        assert!(
            msg == "InvalidTransition" || msg == "InvalidPayload" || msg == "Forbidden",
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn replay_empty_log_returns_error() {
        let registry = make_registry();
        let result = replay_session("s1", &[], &registry, None);
        assert!(result.is_err());
    }

    #[test]
    fn backward_compat_old_log_entry_without_new_fields() {
        // Simulate deserializing a v2 log entry without session_id/mode/macp_version
        let json = r#"{"message_id":"m1","received_at_ms":1000,"sender":"test","message_type":"Message","raw_payload":[],"entry_kind":"Incoming"}"#;
        let entry: LogEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.session_id, "");
        assert_eq!(entry.mode, "");
        assert_eq!(entry.macp_version, "");
    }

    #[test]
    fn replay_from_checkpoint_restores_state() {
        use crate::registry::PersistedSession;

        let registry = make_registry();

        // Build a session via normal replay first
        let proposal = ProposalPayload {
            proposal_id: "p1".into(),
            option: "deploy".into(),
            rationale: "ready".into(),
            supporting_data: vec![],
        }
        .encode_to_vec();

        let full_entries = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload_bytes(),
                1000,
            ),
            incoming_entry(
                "m2",
                "Proposal",
                "agent://orchestrator",
                proposal.clone(),
                2000,
            ),
        ];
        let full_session = replay_session("s1", &full_entries, &registry, None).unwrap();

        // Create a checkpoint from the replayed session state
        let persisted = PersistedSession::from(&full_session);
        let checkpoint_payload = serde_json::to_vec(&persisted).unwrap();
        let checkpoint = LogEntry {
            message_id: String::new(),
            received_at_ms: 3000,
            sender: "_runtime".into(),
            message_type: "Checkpoint".into(),
            raw_payload: checkpoint_payload,
            entry_kind: EntryKind::Checkpoint,
            session_id: "s1".into(),
            mode: "macp.mode.decision.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: 3000,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        };

        // A vote after the checkpoint
        let vote = VotePayload {
            proposal_id: "p1".into(),
            vote: "approve".into(),
            reason: "lgtm".into(),
        }
        .encode_to_vec();

        // Log: SessionStart, Proposal, Checkpoint, Vote
        let entries_with_checkpoint = vec![
            full_entries[0].clone(),
            full_entries[1].clone(),
            checkpoint,
            incoming_entry("m3", "Vote", "agent://fraud", vote, 4000),
        ];

        let session = replay_session("s1", &entries_with_checkpoint, &registry, None).unwrap();
        assert_eq!(session.state, SessionState::Open);
        // Should have dedup from checkpoint (m1, m2) plus newly replayed m3
        assert!(session.seen_message_ids.contains("m1"));
        assert!(session.seen_message_ids.contains("m2"));
        assert!(session.seen_message_ids.contains("m3"));
    }

    #[test]
    fn replay_without_checkpoint_still_works() {
        // Ensure logs without checkpoints replay correctly (backward compat)
        let registry = make_registry();
        let entries = vec![incoming_entry(
            "m1",
            "SessionStart",
            "agent://orchestrator",
            start_payload_bytes(),
            1000,
        )];
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.state, SessionState::Open);
        assert!(session.seen_message_ids.contains("m1"));
    }

    fn ext_registry_with_dyn_mode(version: &str) -> ModeRegistry {
        let registry = make_registry();
        registry
            .register_extension(macp_pb::pb::ModeDescriptor {
                mode: "ext.dyn.v1".into(),
                mode_version: version.into(),
                message_types: vec!["SessionStart".into(), "Commitment".into()],
                terminal_message_types: vec!["Commitment".into()],
                ..Default::default()
            })
            .unwrap();
        registry
    }

    fn ext_start_entry(bound_mode_version: Option<String>) -> LogEntry {
        // Non-strict ext SessionStart whose payload omits mode_version.
        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec();
        LogEntry {
            message_id: "m1".into(),
            received_at_ms: 1000,
            sender: "alice".into(),
            message_type: "SessionStart".into(),
            raw_payload: payload,
            entry_kind: EntryKind::Incoming,
            session_id: "s1".into(),
            mode: "ext.dyn.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: 1000,
            bound_mode_version,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    /// Replay uses the binding recorded at acceptance time — never the live
    /// registry. The registry here deliberately carries a *different* version
    /// than the recorded binding to prove no re-derivation happens.
    #[test]
    fn replay_uses_recorded_mode_version_binding() {
        let registry = ext_registry_with_dyn_mode("9.9.9");
        let entries = vec![ext_start_entry(Some("2.5.0".into()))];
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.mode_version, "2.5.0");
    }

    /// Legacy logs (entries recorded before the binding existed) keep their
    /// original empty-version binding — the vacuous-match semantics they were
    /// accepted under. Migration rule: new semantics apply to new sessions only.
    #[test]
    fn replay_legacy_entry_without_binding_keeps_empty_version() {
        let registry = ext_registry_with_dyn_mode("9.9.9");
        let entries = vec![ext_start_entry(None)];
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.mode_version, "");
    }

    /// A legacy log entry serialized without the field must deserialize (serde
    /// default) and replay under legacy semantics.
    #[test]
    fn legacy_log_entry_json_without_binding_field_deserializes() {
        let json = serde_json::json!({
            "message_id": "m1",
            "received_at_ms": 1000,
            "sender": "alice",
            "message_type": "SessionStart",
            "raw_payload": [],
            "entry_kind": "Incoming",
            "session_id": "s1",
            "mode": "ext.dyn.v1",
            "macp_version": "1.0",
            "timestamp_unix_ms": 1000
        });
        let entry: LogEntry = serde_json::from_value(json).unwrap();
        assert_eq!(entry.bound_mode_version, None);
        // Legacy entries also carry no semantics revision: rev 0 (legacy
        // acceptance-time behavior) via serde default.
        assert_eq!(entry.semantics_rev, 0);
        // And no bound suspension cap: default-cap semantics via serde default.
        assert_eq!(entry.bound_max_suspend_ms, None);
    }

    /// Replay applies the suspension cap recorded at acceptance — never a
    /// re-derived or configured value.
    #[test]
    fn replay_uses_recorded_max_suspend_cap() {
        let registry = ext_registry_with_dyn_mode("1.0.0");
        let mut entry = ext_start_entry(Some("1.0.0".into()));
        entry.bound_max_suspend_ms = Some(1234);
        let session = replay_session("s1", &[entry], &registry, None).unwrap();
        assert_eq!(session.max_suspend_ms, 1234);
        assert_eq!(session.effective_max_suspend_ms(), 1234);
    }

    /// Legacy entries (recorded before the cap was bindable) load unbound and
    /// keep default-cap semantics — how they were accepted.
    #[test]
    fn replay_legacy_entry_keeps_default_cap_semantics() {
        let registry = ext_registry_with_dyn_mode("1.0.0");
        let entries = vec![ext_start_entry(None)];
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.max_suspend_ms, 0);
        assert_eq!(
            session.effective_max_suspend_ms(),
            macp_core::session::MAX_SUSPEND_MS
        );
    }

    /// Replay binds the session to the semantics revision recorded at
    /// acceptance: legacy entries (rev 0) must NOT be upgraded to the current
    /// revision, or their acceptance-time behavior (e.g. the handoff
    /// implicit-accept clock) would change under replay.
    #[test]
    fn replay_preserves_recorded_semantics_rev() {
        let registry = ext_registry_with_dyn_mode("1.0.0");
        let entries = vec![ext_start_entry(Some("1.0.0".into()))]; // semantics_rev: 0
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.semantics_rev, 0);
        assert_ne!(
            session.semantics_rev,
            macp_core::session::CURRENT_SEMANTICS_REV
        );
    }

    #[test]
    fn replay_consistency_flags_state_and_dedup_divergence() {
        let a = Session::builder("s1", "macp.mode.decision.v1", "agent://a")
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .build();
        // Identical sessions: consistent.
        assert_eq!(validate_replay_consistency("s1", &a, &a.clone()), 0);

        // Diverged state + dedup count: two mismatches, warn-only.
        let mut b = a.clone();
        b.state = SessionState::Resolved;
        b.seen_message_ids.insert("m1".into());
        assert_eq!(validate_replay_consistency("s1", &a, &b), 2);
    }
}
