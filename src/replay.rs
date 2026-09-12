use crate::error::MacpError;
use crate::log_store::{EntryKind, LogEntry};
use crate::mode_registry::ModeRegistry;
use crate::pb::Envelope;
use crate::policy::registry::PolicyRegistry;
use crate::registry::PersistedSession;
use crate::session::{
    extract_ttl_ms, parse_session_start_payload, validate_canonical_session_start_payload_for_mode,
    Session, SessionState,
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
/// are best-effort, so a mismatch is diagnostic, never fatal — but divergence
/// between "what the log replays to" and "what the snapshot recorded" is
/// exactly the class of bug the determinism guarantees (RFC-MACP-0003) forbid,
/// so it must be visible. Returns the number of mismatched fields
/// (0 = consistent).
///
/// Compared: `state`, dedup count, `participants`, the bound versions
/// (mode/configuration/policy, counted as one), `mode_state` (byte equality),
/// `accumulated_suspended_ms` and `suspended_at_ms`.
///
/// Deliberately **warn-only**: making it fatal would turn a benign snapshot
/// lag (a crash between the log append and the snapshot write) into a startup
/// outage, even though the log — which is authoritative — is intact.
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
    // Opaque per-mode state: compared byte-for-byte, since a mode's own
    // accept/reject decisions are driven by it and the runtime cannot
    // interpret it here.
    if replayed.mode_state != snapshot.mode_state {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_len = replayed.mode_state.len(),
            snapshot_len = snapshot.mode_state.len(),
            "replay/snapshot mode_state mismatch"
        );
    }
    // Suspension state (RFC-MACP-0001 §7.5). `accumulated_suspended_ms` feeds
    // the TTL deadline and the handoff implicit-accept arithmetic, so a
    // divergence here is a determinism bug even when `state` still agrees.
    if replayed.accumulated_suspended_ms != snapshot.accumulated_suspended_ms {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_accumulated_suspended_ms = replayed.accumulated_suspended_ms,
            snapshot_accumulated_suspended_ms = snapshot.accumulated_suspended_ms,
            "replay/snapshot accumulated_suspended_ms mismatch"
        );
    }
    if replayed.suspended_at_ms != snapshot.suspended_at_ms {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_suspended_at_ms = ?replayed.suspended_at_ms,
            snapshot_suspended_at_ms = ?snapshot.suspended_at_ms,
            "replay/snapshot suspended_at_ms mismatch"
        );
    }
    // Completed suspend/resume pairs (Phase 11b). These feed the
    // implicit-accept deadline walk (RFC-MACP-0010 §5.1(3)), so a snapshot
    // that disagrees with the log about *when* a session was paused is the
    // same class of determinism bug as disagreeing about how long.
    if replayed.suspension_intervals != snapshot.suspension_intervals {
        mismatches += 1;
        tracing::warn!(
            session_id,
            replayed_suspension_cycles = replayed.suspension_intervals.len(),
            snapshot_suspension_cycles = snapshot.suspension_intervals.len(),
            "replay/snapshot suspension_intervals mismatch"
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
    // Same split as the acceptance path in `runtime.rs`: the registry decides
    // whether the canonical contract applies, the validator decides which
    // roster rule applies within it.
    if require_complete_start {
        validate_canonical_session_start_payload_for_mode(mode_name, &start_payload)?;
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

    /// The checkpoint fast path carries dedup state (`seen_message_ids`) for
    /// the entries it subsumes, and replays only the tail after it.
    ///
    /// The SessionStart payload here deliberately binds **no** policy version.
    /// `try_replay_from_checkpoint` bails to a full replay whenever a
    /// checkpoint has a bound `policy_version` but no serialized
    /// `policy_definition`, and this test used `start_payload_bytes()` (which
    /// binds `policy-1`) with `replay_session(.., None)` -- so it always took
    /// the fallback, and its three dedup assertions were satisfied by a plain
    /// full replay. The tripwire below now pins which path ran.
    #[test]
    fn replay_from_checkpoint_restores_state() {
        use crate::registry::PersistedSession;

        let registry = make_registry();
        let start_payload = SessionStartPayload {
            intent: "test".into(),
            participants: vec!["agent://orchestrator".into(), "agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();

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
                start_payload,
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
        let mut persisted = PersistedSession::from(&full_session);
        // Tripwire: a value only the snapshot can supply. A fallback full
        // replay would rebuild `intent` from the SessionStart payload ("test"),
        // so this assertion is what proves the fast path ran.
        persisted.intent = "restored-from-checkpoint".into();
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
        assert_eq!(
            session.intent, "restored-from-checkpoint",
            "the checkpoint fast path must have been taken, else this test \
             proves nothing about the checkpoint"
        );
        // Should have dedup from checkpoint (m1, m2) plus newly replayed m3
        assert!(session.seen_message_ids.contains("m1"));
        assert!(session.seen_message_ids.contains("m2"));
        assert!(session.seen_message_ids.contains("m3"));
    }

    /// Phase 11b acceptance criterion 3 — a checkpoint written *after* a
    /// suspend/resume pair carries `suspension_intervals` through the
    /// `PersistedSession` round-trip, so the checkpoint fast path (which
    /// replays only the entries after the checkpoint, and therefore never
    /// sees the earlier `SessionSuspend`/`SessionResume` entries) restores
    /// the pause the deadline walk depends on.
    ///
    /// The SessionStart payload here deliberately binds **no** policy version:
    /// `try_replay_from_checkpoint` falls back to a full replay whenever a
    /// checkpoint has a bound `policy_version` but no serialized
    /// `policy_definition`, and a full replay would rebuild the vec from the
    /// pre-checkpoint entries — hiding the very round-trip under test.
    #[test]
    fn replay_from_checkpoint_restores_suspension_intervals() {
        use crate::registry::PersistedSession;

        let registry = make_registry();
        let start_payload = SessionStartPayload {
            intent: "test".into(),
            participants: vec!["agent://orchestrator".into(), "agent://fraud".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();

        let prefix = vec![
            incoming_entry(
                "m1",
                "SessionStart",
                "agent://orchestrator",
                start_payload,
                1_000,
            ),
            internal_entry("SessionSuspend", 1_050),
            internal_entry("SessionResume", 1_300),
        ];
        let before_checkpoint = replay_session("s1", &prefix, &registry, None).unwrap();
        assert_eq!(before_checkpoint.suspension_intervals, vec![(1_050, 1_300)]);

        let mut persisted = PersistedSession::from(&before_checkpoint);
        // Tripwire: a value only the snapshot can supply, so the assertions
        // below cannot silently be satisfied by a fallback full replay.
        persisted.intent = "restored-from-checkpoint".into();
        // Go through the wire format, not just the struct: `#[serde(default)]`
        // must not be the thing that supplies the value here.
        let checkpoint_payload = serde_json::to_vec(&persisted).unwrap();
        let checkpoint = LogEntry {
            message_id: String::new(),
            received_at_ms: 1_400,
            sender: "_runtime".into(),
            message_type: "Checkpoint".into(),
            raw_payload: checkpoint_payload,
            entry_kind: EntryKind::Checkpoint,
            session_id: "s1".into(),
            mode: "macp.mode.decision.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: 1_400,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        };

        // The checkpoint fast path replays only what follows the checkpoint,
        // so the pause can only survive via the snapshot.
        let entries = vec![
            prefix[0].clone(),
            prefix[1].clone(),
            prefix[2].clone(),
            checkpoint,
            internal_entry("SessionSuspend", 1_500),
            internal_entry("SessionResume", 1_600),
        ];
        let session = replay_session("s1", &entries, &registry, None).unwrap();
        assert_eq!(session.state, SessionState::Open);
        assert_eq!(
            session.intent, "restored-from-checkpoint",
            "the checkpoint fast path must have been taken, else this test \
             proves nothing about the snapshot round-trip"
        );
        assert_eq!(
            session.suspension_intervals,
            vec![(1_050, 1_300), (1_500, 1_600)],
            "the pre-checkpoint pause must come from the snapshot and the \
             post-checkpoint pause from the replayed tail"
        );
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

        // `mode_state` is compared byte-for-byte, on its own.
        let mut c = a.clone();
        c.mode_state = vec![7, 7, 7];
        assert_eq!(validate_replay_consistency("s1", &a, &c), 1);

        // Suspension state is counted per field: a session the log replays to
        // "resumed after 5s" against a snapshot that recorded "still
        // suspended, nothing banked" is two mismatches.
        let mut d = a.clone();
        d.accumulated_suspended_ms = 5_000;
        assert_eq!(validate_replay_consistency("s1", &a, &d), 1);
        d.suspended_at_ms = Some(1_000);
        assert_eq!(validate_replay_consistency("s1", &a, &d), 2);
        // Completed pairs are a third, independent suspension comparison.
        d.suspension_intervals = vec![(1_000, 6_000)];
        assert_eq!(validate_replay_consistency("s1", &a, &d), 3);

        // All six at once, to pin that each comparison contributes exactly
        // one count and none of them shadow another.
        let mut e = b.clone();
        e.mode_state = vec![7, 7, 7];
        e.accumulated_suspended_ms = 5_000;
        e.suspended_at_ms = Some(1_000);
        e.suspension_intervals = vec![(1_000, 6_000)];
        assert_eq!(validate_replay_consistency("s1", &a, &e), 6);
    }

    // ---------------------------------------------------------------------
    // Legacy-log fixtures for `Session::semantics_rev` (CONTRIBUTING.md
    // ground rule: a change to persisted-history semantics ships a fixture
    // proving old logs still replay under their original semantics).
    //
    // All three fixtures below are the *same* three handoff entries; only the
    // revision recorded on the SessionStart entry differs. The entries carry
    // deliberately disagreeing envelope and acceptance timestamps, so the
    // recorded revision alone decides whether the implicit-accept timeout
    // fires — which makes each fixture a differential proof, not just a
    // "replay does not crash" smoke test.
    // ---------------------------------------------------------------------

    const HANDOFF_TIMEOUT_MS: i64 = 100;

    fn handoff_policy_registry() -> PolicyRegistry {
        let registry = PolicyRegistry::new();
        registry
            .register(macp_core::policy::PolicyDefinition {
                policy_id: "handoff-auto-accept".into(),
                mode: "macp.mode.handoff.v1".into(),
                description: "implicit accept after 100ms".into(),
                rules: serde_json::json!({
                    "acceptance": { "implicit_accept_timeout_ms": HANDOFF_TIMEOUT_MS },
                    "commitment": { "authority": "initiator_only" }
                }),
                schema_version: 1,
            })
            .unwrap();
        registry
    }

    fn handoff_entry(
        message_id: &str,
        message_type: &str,
        payload: Vec<u8>,
        envelope_ms: i64,
        received_ms: i64,
    ) -> LogEntry {
        LogEntry {
            message_id: message_id.into(),
            received_at_ms: received_ms,
            sender: "alice".into(),
            message_type: message_type.into(),
            raw_payload: payload,
            entry_kind: EntryKind::Incoming,
            session_id: "s1".into(),
            mode: "macp.mode.handoff.v1".into(),
            macp_version: "1.0".into(),
            timestamp_unix_ms: envelope_ms,
            bound_mode_version: None,
            semantics_rev: 0,
            bound_max_suspend_ms: None,
            compacted_incoming_ordinals: 0,
        }
    }

    /// SessionStart + HandoffOffer + Commitment, with the offer/commitment
    /// clocks supplied by the caller so a fixture can make the two clocks
    /// disagree.
    fn handoff_history(
        semantics_rev: u32,
        commit_envelope_ms: i64,
        commit_received_ms: i64,
    ) -> Vec<LogEntry> {
        let start_payload = SessionStartPayload {
            intent: "escalate".into(),
            participants: vec!["alice".into(), "bob".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: "handoff-auto-accept".into(),
            ttl_ms: 60_000,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec();
        let offer = crate::handoff_pb::HandoffOfferPayload {
            handoff_id: "h1".into(),
            target_participant: "bob".into(),
            scope: "support".into(),
            reason: "escalate".into(),
        }
        .encode_to_vec();
        let commitment = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "handoff.accepted".into(),
            authority_scope: "support".into(),
            reason: "bound".into(),
            mode_version: "1.0.0".into(),
            policy_version: "handoff-auto-accept".into(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();

        let mut start = handoff_entry("m1", "SessionStart", start_payload, 1_000, 1_000);
        start.semantics_rev = semantics_rev;
        vec![
            start,
            // Offer: both clocks agree at 1_000, so only the commitment's
            // clock choice can move the outcome.
            handoff_entry("m2", "HandoffOffer", offer, 1_000, 1_000),
            handoff_entry(
                "m3",
                "Commitment",
                commitment,
                commit_envelope_ms,
                commit_received_ms,
            ),
        ]
    }

    /// The outcome a fixture was originally accepted with: the offer is
    /// implicitly accepted and the commitment resolves the session.
    fn assert_implicitly_accepted(session: &Session) {
        assert_eq!(session.state, SessionState::Resolved);
        let state: serde_json::Value = serde_json::from_slice(&session.mode_state).unwrap();
        let offer = &state["offers"]["h1"];
        assert_eq!(offer["disposition"], "Accepted");
        assert_eq!(offer["accepted_by"], "bob");
        assert_eq!(offer["outcome_reason"], "implicit accept (timeout)");
    }

    /// Legacy (rev 0) history: the implicit-accept timeout was measured
    /// against the client envelope timestamp. These entries only clear the
    /// timeout on that clock (envelope: 300ms elapsed; acceptance: 50ms), so a
    /// replay that resolves is a replay that used the legacy clock.
    #[test]
    fn legacy_rev0_handoff_history_replays_under_envelope_clock() {
        let registry = make_registry();
        let policies = handoff_policy_registry();
        let entries = handoff_history(0, 1_300, 1_050);

        let session = replay_session("s1", &entries, &registry, Some(&policies)).unwrap();
        assert_eq!(session.semantics_rev, 0);
        assert_implicitly_accepted(&session);

        // Differential proof: the identical entries under any newer revision
        // do NOT implicitly accept (50ms of acceptance time < 100ms), so the
        // commitment is not ready and replay fails. Only the recorded
        // revision keeps this history replayable.
        for rev in [1, macp_core::session::CURRENT_SEMANTICS_REV] {
            let mut newer = entries.clone();
            newer[0].semantics_rev = rev;
            assert!(
                replay_session("s1", &newer, &registry, Some(&policies)).is_err(),
                "rev {rev} must not reproduce the rev-0 outcome"
            );
        }
    }

    /// Rev-1 history: the timeout was measured against the runtime acceptance
    /// clock. Mirror image of the rev-0 fixture — these entries only clear the
    /// timeout on `received_at_ms` (acceptance: 300ms; envelope: 50ms).
    #[test]
    fn legacy_rev1_handoff_history_replays_under_acceptance_clock() {
        let registry = make_registry();
        let policies = handoff_policy_registry();
        let entries = handoff_history(1, 1_050, 1_300);

        let session = replay_session("s1", &entries, &registry, Some(&policies)).unwrap();
        assert_eq!(session.semantics_rev, 1);
        assert_implicitly_accepted(&session);

        // Under the legacy clock the same entries do not reach the timeout.
        let mut legacy = entries.clone();
        legacy[0].semantics_rev = 0;
        assert!(replay_session("s1", &legacy, &registry, Some(&policies)).is_err());
    }

    /// For a history with **no suspension**, the current revision replays to
    /// exactly the rev-1 outcome, including the byte-level `mode_state`. Rev 2
    /// is not behavior-neutral in general — it deliberately changed the
    /// implicit-accept deadline (RFC-MACP-0010 §5.1(1)) — but the only term it
    /// added is the suspension accrued since the offer, which is zero here. So
    /// this pins the property that keeps unsuspended legacy histories
    /// replaying identically. The suspended counterpart, where the revisions
    /// diverge, is
    /// `legacy_rev1_handoff_history_with_suspension_still_implicitly_accepts`.
    #[test]
    fn current_rev_handoff_history_replays_identically_to_rev1() {
        let registry = make_registry();
        let policies = handoff_policy_registry();

        let rev1 = replay_session(
            "s1",
            &handoff_history(1, 1_050, 1_300),
            &registry,
            Some(&policies),
        )
        .unwrap();
        let current = replay_session(
            "s1",
            &handoff_history(macp_core::session::CURRENT_SEMANTICS_REV, 1_050, 1_300),
            &registry,
            Some(&policies),
        )
        .unwrap();

        assert_implicitly_accepted(&current);
        assert_eq!(current.state, rev1.state);
        assert_eq!(current.mode_state, rev1.mode_state);
        assert_eq!(current.resolution, rev1.resolution);
    }

    /// The same three handoff entries with a suspend/resume pair spliced
    /// between the offer and the commitment, so the replayed session banks
    /// `accumulated_suspended_ms` from the recorded internal-entry timestamps
    /// (RFC-MACP-0001 §7.5 / RFC-MACP-0003 §2).
    fn handoff_history_with_suspension(
        semantics_rev: u32,
        suspend_at_ms: i64,
        resume_at_ms: i64,
        commit_ms: i64,
    ) -> Vec<LogEntry> {
        // Both commitment clocks agree here: the suspension term, not the
        // clock choice, is what the revision changes.
        let mut entries = handoff_history(semantics_rev, commit_ms, commit_ms);
        let commit = entries.pop().expect("commitment is the last entry");
        entries.push(internal_entry("SessionSuspend", suspend_at_ms));
        entries.push(internal_entry("SessionResume", resume_at_ms));
        entries.push(commit);
        entries
    }

    /// Rev-1 history containing an implicit accept that only happened because
    /// suspended time counted toward the deadline. It must keep replaying to
    /// that accept: the log is authoritative and the session already resolved
    /// on it.
    ///
    /// Offer at 1_000, suspended 1_050..1_300 (250ms), commitment at 1_300 —
    /// 300ms elapsed, 50ms of it unsuspended, against a 100ms timeout. So the
    /// recorded revision alone decides the outcome, which makes this a
    /// differential proof rather than a smoke test.
    #[test]
    fn legacy_rev1_handoff_history_with_suspension_still_implicitly_accepts() {
        let registry = make_registry();
        let policies = handoff_policy_registry();
        let entries = handoff_history_with_suspension(1, 1_050, 1_300, 1_300);

        let session = replay_session("s1", &entries, &registry, Some(&policies)).unwrap();
        assert_eq!(session.semantics_rev, 1);
        assert_eq!(session.accumulated_suspended_ms, 250);
        assert_implicitly_accepted(&session);

        // Under rev 2 the identical entries do NOT implicitly accept
        // (RFC-MACP-0010 §5.1(1)): only 50ms of unsuspended time elapsed, so
        // no offer is accepted, the commitment is not ready, and replay fails.
        let mut rev2 = entries.clone();
        rev2[0].semantics_rev = macp_core::session::CURRENT_SEMANTICS_REV;
        assert!(
            replay_session("s1", &rev2, &registry, Some(&policies)).is_err(),
            "rev 2 must not reproduce the rev-1 outcome"
        );
    }

    /// The rev-2 side of the same fixture: once enough *unsuspended* time has
    /// elapsed the implicit accept fires through the real replay path.
    ///
    /// Offer at 1_000, suspended 1_050..1_300 (250ms), commitment at 1_450 —
    /// 450ms elapsed, 200ms of it unsuspended, past the 100ms timeout.
    #[test]
    fn rev2_handoff_history_implicitly_accepts_on_unsuspended_time() {
        let registry = make_registry();
        let policies = handoff_policy_registry();
        let entries = handoff_history_with_suspension(
            macp_core::session::CURRENT_SEMANTICS_REV,
            1_050,
            1_300,
            1_450,
        );

        let session = replay_session("s1", &entries, &registry, Some(&policies)).unwrap();
        assert_eq!(session.accumulated_suspended_ms, 250);
        assert_implicitly_accepted(&session);
    }

    /// Sibling of [`handoff_history_with_suspension`] carrying **two**
    /// suspend/resume pairs, so the replayed `accumulated_suspended_ms` is a
    /// sum of banked pauses rather than a single one. (A sibling rather than a
    /// second pair spliced into that fixture: its single 250ms pause is
    /// load-bearing arithmetic for both of its callers.)
    ///
    /// Timeline — every stamp is the recorded `received_at_ms`, and the
    /// timeout is the 100ms `implicit_accept_timeout_ms` from
    /// [`handoff_policy_registry`]:
    ///
    /// ```text
    /// 1_000  SessionStart + HandoffOffer     unsuspended run:  50ms
    /// 1_050  SessionSuspend  ┐ banks 250ms
    /// 1_300  SessionResume   ┘               unsuspended run:  30ms
    /// 1_330  SessionSuspend  ┐ banks 170ms
    /// 1_500  SessionResume   ┘               unsuspended run: commit_ms - 1_500
    /// commit_ms  Commitment
    /// ```
    ///
    /// So `accumulated_suspended_ms == 250 + 170 == 420`, the rev-2
    /// unsuspended elapsed is `commit_ms - 1_000 - 420` (equivalently
    /// `80 + (commit_ms - 1_500)`), and rev 1 ignores the pauses entirely at
    /// `commit_ms - 1_000`. Both resumes also re-run the cumulative cap check
    /// in `Session::resume` against the running total, not the latest pause.
    fn handoff_history_with_two_suspensions(semantics_rev: u32, commit_ms: i64) -> Vec<LogEntry> {
        let mut entries = handoff_history(semantics_rev, commit_ms, commit_ms);
        let commit = entries.pop().expect("commitment is the last entry");
        entries.push(internal_entry("SessionSuspend", 1_050));
        entries.push(internal_entry("SessionResume", 1_300));
        entries.push(internal_entry("SessionSuspend", 1_330));
        entries.push(internal_entry("SessionResume", 1_500));
        entries.push(commit);
        entries
    }

    /// Multi-pause differential. Commitment at 1_510: 510ms since the offer,
    /// of which only 90ms is unsuspended (50 + 30 + 10) against the 100ms
    /// timeout. Rev 1 counts all 510ms and implicitly accepts; rev 2 counts
    /// 90ms and does not, so the commitment has no resolved offer to bind and
    /// replay fails.
    ///
    /// This is the determinism claim for a history with *multiple*
    /// suspend/resume pairs: rev 2 subtracts the accumulated suspension, so
    /// banking only the most recent pause (170ms) would leave 340ms of
    /// apparent unsuspended time and wrongly accept — which a single-pair
    /// fixture cannot distinguish.
    #[test]
    fn rev2_handoff_history_subtracts_every_suspension_pair() {
        let registry = make_registry();
        let policies = handoff_policy_registry();

        let rev1 = handoff_history_with_two_suspensions(1, 1_510);
        let session = replay_session("s1", &rev1, &registry, Some(&policies)).unwrap();
        assert_eq!(session.semantics_rev, 1);
        assert_eq!(session.accumulated_suspended_ms, 420);
        assert_implicitly_accepted(&session);

        let rev2 =
            handoff_history_with_two_suspensions(macp_core::session::CURRENT_SEMANTICS_REV, 1_510);
        assert!(
            replay_session("s1", &rev2, &registry, Some(&policies)).is_err(),
            "rev 2 must subtract both pauses (90ms unsuspended < 100ms timeout)"
        );
    }

    /// The rev-2 positive path across two pauses. Commitment at 1_600: 600ms
    /// since the offer, 180ms of it unsuspended (50 + 30 + 100), which clears
    /// the 100ms timeout even after both pauses are excluded.
    #[test]
    fn rev2_handoff_history_accepts_on_unsuspended_time_across_two_pauses() {
        let registry = make_registry();
        let policies = handoff_policy_registry();
        let entries =
            handoff_history_with_two_suspensions(macp_core::session::CURRENT_SEMANTICS_REV, 1_600);

        let session = replay_session("s1", &entries, &registry, Some(&policies)).unwrap();
        assert_eq!(session.accumulated_suspended_ms, 420);
        assert_implicitly_accepted(&session);
    }

    /// Phase 11b acceptance criterion 2 — replay rebuilds
    /// `suspension_intervals` with **zero replay-code changes**, because the
    /// `SessionSuspend`/`SessionResume` arms already drive
    /// `Session::suspend`/`Session::resume` from the recorded
    /// `received_at_ms` and `resume` is what records the pair.
    ///
    /// Asserted at both revisions: the vec is recorded everywhere (read only
    /// at rev >= 2), so a rev gate on the *recording* would red this.
    #[test]
    fn replay_rebuilds_suspension_intervals_from_the_log() {
        let registry = make_registry();
        let policies = handoff_policy_registry();

        let rev1 = handoff_history_with_two_suspensions(1, 1_510);
        let session = replay_session("s1", &rev1, &registry, Some(&policies)).unwrap();
        assert_eq!(
            session.suspension_intervals,
            vec![(1_050, 1_300), (1_330, 1_500)]
        );

        let rev2 =
            handoff_history_with_two_suspensions(macp_core::session::CURRENT_SEMANTICS_REV, 1_600);
        let session = replay_session("s1", &rev2, &registry, Some(&policies)).unwrap();
        assert_eq!(
            session.suspension_intervals,
            vec![(1_050, 1_300), (1_330, 1_500)]
        );
        // And the walk reads them: an offer at 1_000 with a 100ms timeout
        // lands past both pauses rather than at the naive 1_100.
        // 50ms unsuspended before the first pause + 30ms between the pauses +
        // 20ms after the second = the 100ms timeout, so D = 1_500 + 20.
        assert_eq!(session.unsuspended_deadline(1_000, 100), 1_520);
    }
}
