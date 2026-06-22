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
            let response = mode.on_message(session, &replay_env)?;
            session.apply_mode_response(response);
            if !replay_env.message_id.is_empty() {
                session.seen_message_ids.insert(replay_env.message_id);
            }
        }
        EntryKind::Internal => match entry.message_type.as_str() {
            "TtlExpired" | "SessionCancel" => {
                session.state = SessionState::Expired;
            }
            _ => {}
        },
        EntryKind::Checkpoint => {
            // Skip intermediate checkpoints when replaying from an earlier one
        }
    }
    Ok(())
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

    let mut session = Session {
        session_id: session_id.into(),
        state: SessionState::Open,
        ttl_expiry,
        ttl_ms,
        started_at_unix_ms,
        resolution: None,
        mode: mode_name.to_string(),
        mode_state: vec![],
        participants: start_payload.participants.clone(),
        seen_message_ids: std::collections::HashSet::new(),
        intent: start_payload.intent.clone(),
        mode_version: start_payload.mode_version.clone(),
        configuration_version: start_payload.configuration_version.clone(),
        policy_version: start_payload.policy_version.clone(),
        context_id: start_payload.context_id.clone(),
        extensions: start_payload.extensions.clone(),
        roots: start_payload.roots.clone(),
        initiator_sender: start_entry.sender.clone(),
        participant_message_counts: std::collections::HashMap::new(),
        participant_last_seen: std::collections::HashMap::new(),
        policy_definition: if !start_payload.policy_version.is_empty() {
            policy_registry.and_then(|pr| pr.resolve(&start_payload.policy_version).ok())
        } else {
            None
        },
    };

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
        assert_eq!(session.state, SessionState::Expired);
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
}
