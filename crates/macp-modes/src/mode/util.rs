use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::CommitmentPayload;
use prost::Message;

pub fn decode_commitment_payload(payload: &[u8]) -> Result<CommitmentPayload, MacpError> {
    CommitmentPayload::decode(payload).map_err(|_| MacpError::InvalidPayload)
}

pub fn validate_commitment_payload_for_session(
    session: &Session,
    payload: &[u8],
) -> Result<CommitmentPayload, MacpError> {
    let commitment = decode_commitment_payload(payload)?;

    if commitment.commitment_id.trim().is_empty()
        || commitment.action.trim().is_empty()
        || commitment.authority_scope.trim().is_empty()
        || commitment.reason.trim().is_empty()
    {
        return Err(MacpError::InvalidPayload);
    }

    if commitment.mode_version != session.mode_version
        || commitment.configuration_version != session.configuration_version
    {
        return Err(MacpError::InvalidPayload);
    }

    if !session.policy_version.is_empty() && commitment.policy_version != session.policy_version {
        return Err(MacpError::InvalidPayload);
    }

    // RFC-MACP-0001 §7.3.1: if this commitment supersedes a prior one, the
    // reference must be structurally well-formed. Supersession is inherently
    // cross-session, so the kernel checks only well-formedness here (and
    // authority, separately) — it does NOT verify the referenced commitment
    // exists, was sealed, or is unforked. Those are consumer governance.
    if let Some(ref sup) = commitment.supersedes {
        if sup.session_id.trim().is_empty() || sup.commitment_hash.trim().is_empty() {
            return Err(MacpError::InvalidPayload);
        }
    }

    // Validate outcome_positive consistency with action (RFC-0001 §7.3)
    validate_outcome_positive(&commitment)?;

    Ok(commitment)
}

/// Validate that `outcome_positive` is consistent with the `action` field.
/// Actions ending in `rejected`, `failed`, or `declined` must have `outcome_positive = false`.
/// Actions ending in `selected`, `accepted`, `completed`, or `approved` must have `outcome_positive = true`.
fn validate_outcome_positive(commitment: &CommitmentPayload) -> Result<(), MacpError> {
    let action = commitment.action.as_str();
    let negative_actions = ["rejected", "failed", "declined"];
    let positive_actions = ["selected", "accepted", "completed", "approved"];

    let is_negative = negative_actions
        .iter()
        .any(|suffix| action.ends_with(suffix));
    let is_positive = positive_actions
        .iter()
        .any(|suffix| action.ends_with(suffix));

    if is_negative && commitment.outcome_positive {
        return Err(MacpError::InvalidPayload);
    }
    if is_positive && !commitment.outcome_positive {
        return Err(MacpError::InvalidPayload);
    }
    Ok(())
}

pub fn is_declared_participant(participants: &[String], sender: &str) -> bool {
    participants.iter().any(|participant| participant == sender)
}

/// Check whether the sender is authorized to commit per the policy's `commitment.authority` rule.
///
/// RFC-MACP-0012 §4: the `commitment` rule group controls who can emit a Commitment
/// envelope. If no policy is bound, defaults to initiator-only (RFC-MACP-0001 §7.3).
pub fn check_commitment_authority(session: &Session, sender: &str) -> Result<(), MacpError> {
    if let Some(ref policy) = session.policy_definition {
        let rules: macp_core::policy::rules::CommitmentRules =
            extract_commitment_rules(&policy.rules);
        match rules.authority.as_str() {
            "any_participant" => {
                if sender == session.initiator_sender
                    || is_declared_participant(&session.participants, sender)
                {
                    Ok(())
                } else {
                    Err(MacpError::Forbidden)
                }
            }
            "designated_role" => {
                if rules.designated_roles.iter().any(|r| r == sender) {
                    Ok(())
                } else {
                    Err(MacpError::Forbidden)
                }
            }
            _ => {
                // "initiator_only" (default)
                if sender == session.initiator_sender {
                    Ok(())
                } else {
                    Err(MacpError::Forbidden)
                }
            }
        }
    } else {
        // No policy bound — default to initiator-only
        if sender == session.initiator_sender {
            Ok(())
        } else {
            Err(MacpError::Forbidden)
        }
    }
}

/// Extract the `commitment` section from any mode's policy rules JSON.
/// All RFC mode schemas include a `commitment` sub-object with `authority` and `designated_roles`.
fn extract_commitment_rules(
    rules: &serde_json::Value,
) -> macp_core::policy::rules::CommitmentRules {
    rules
        .get("commitment")
        .and_then(|c| serde_json::from_value(c.clone()).ok())
        .unwrap_or_default()
}

pub fn participants_all_accept(
    participants: &[String],
    accepts: &std::collections::BTreeMap<String, String>,
    proposal_id: &str,
) -> bool {
    !participants.is_empty()
        && participants
            .iter()
            .all(|participant| accepts.get(participant).map(String::as_str) == Some(proposal_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_pb::pb::CommitmentPayload;

    fn make_commitment(action: &str, outcome_positive: bool) -> CommitmentPayload {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: action.into(),
            authority_scope: "scope".into(),
            reason: "reason".into(),
            mode_version: "1.0.0".into(),
            policy_version: String::new(),
            configuration_version: "cfg-1".into(),
            outcome_positive,
            supersedes: None,
        }
    }

    // --- supersedes structural validation (RFC-MACP-0001 §7.3.1) ---

    fn session_for_commitment() -> Session {
        use std::collections::{HashMap, HashSet};
        Session {
            session_id: "s1".into(),
            state: macp_core::session::SessionState::Open,
            ttl_expiry: i64::MAX,
            ttl_ms: 60_000,
            started_at_unix_ms: 0,
            resolution: None,
            mode: "macp.mode.decision.v1".into(),
            mode_state: vec![],
            participants: vec![],
            seen_message_ids: HashSet::new(),
            intent: String::new(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            context_id: String::new(),
            extensions: HashMap::new(),
            roots: vec![],
            initiator_sender: "agent://a".into(),
            participant_message_counts: HashMap::new(),
            participant_last_seen: HashMap::new(),
            policy_definition: None,
            suspended_at_ms: None,
            accumulated_suspended_ms: 0,
        }
    }

    #[test]
    fn well_formed_supersedes_is_accepted() {
        let session = session_for_commitment();
        let mut c = make_commitment("decision.selected", true);
        c.supersedes = Some(macp_pb::pb::CommitmentRef {
            session_id: "prior-session".into(),
            commitment_hash: "abc123".into(),
        });
        assert!(validate_commitment_payload_for_session(&session, &c.encode_to_vec()).is_ok());
    }

    #[test]
    fn malformed_supersedes_is_rejected() {
        let session = session_for_commitment();
        for bad in [("", "abc123"), ("prior-session", ""), ("  ", "abc123")] {
            let mut c = make_commitment("decision.selected", true);
            c.supersedes = Some(macp_pb::pb::CommitmentRef {
                session_id: bad.0.into(),
                commitment_hash: bad.1.into(),
            });
            assert!(
                validate_commitment_payload_for_session(&session, &c.encode_to_vec()).is_err(),
                "expected rejection for supersedes {bad:?}"
            );
        }
    }

    // --- outcome_positive validation: RFC-defined positive actions ---

    #[test]
    fn decision_selected_positive_ok() {
        assert!(validate_outcome_positive(&make_commitment("decision.selected", true)).is_ok());
    }

    #[test]
    fn decision_selected_negative_rejected() {
        assert!(validate_outcome_positive(&make_commitment("decision.selected", false)).is_err());
    }

    #[test]
    fn decision_rejected_negative_ok() {
        assert!(validate_outcome_positive(&make_commitment("decision.rejected", false)).is_ok());
    }

    #[test]
    fn decision_rejected_positive_rejected() {
        assert!(validate_outcome_positive(&make_commitment("decision.rejected", true)).is_err());
    }

    #[test]
    fn proposal_accepted_positive_ok() {
        assert!(validate_outcome_positive(&make_commitment("proposal.accepted", true)).is_ok());
    }

    #[test]
    fn proposal_accepted_negative_rejected() {
        assert!(validate_outcome_positive(&make_commitment("proposal.accepted", false)).is_err());
    }

    #[test]
    fn proposal_rejected_negative_ok() {
        assert!(validate_outcome_positive(&make_commitment("proposal.rejected", false)).is_ok());
    }

    #[test]
    fn proposal_rejected_positive_rejected() {
        assert!(validate_outcome_positive(&make_commitment("proposal.rejected", true)).is_err());
    }

    #[test]
    fn task_completed_positive_ok() {
        assert!(validate_outcome_positive(&make_commitment("task.completed", true)).is_ok());
    }

    #[test]
    fn task_completed_negative_rejected() {
        assert!(validate_outcome_positive(&make_commitment("task.completed", false)).is_err());
    }

    #[test]
    fn task_failed_negative_ok() {
        assert!(validate_outcome_positive(&make_commitment("task.failed", false)).is_ok());
    }

    #[test]
    fn task_failed_positive_rejected() {
        assert!(validate_outcome_positive(&make_commitment("task.failed", true)).is_err());
    }

    #[test]
    fn handoff_accepted_positive_ok() {
        assert!(validate_outcome_positive(&make_commitment("handoff.accepted", true)).is_ok());
    }

    #[test]
    fn handoff_declined_negative_ok() {
        assert!(validate_outcome_positive(&make_commitment("handoff.declined", false)).is_ok());
    }

    #[test]
    fn handoff_declined_positive_rejected() {
        assert!(validate_outcome_positive(&make_commitment("handoff.declined", true)).is_err());
    }

    #[test]
    fn quorum_approved_positive_ok() {
        assert!(validate_outcome_positive(&make_commitment("quorum.approved", true)).is_ok());
    }

    #[test]
    fn quorum_rejected_negative_ok() {
        assert!(validate_outcome_positive(&make_commitment("quorum.rejected", false)).is_ok());
    }

    #[test]
    fn quorum_rejected_positive_rejected() {
        assert!(validate_outcome_positive(&make_commitment("quorum.rejected", true)).is_err());
    }

    #[test]
    fn custom_action_no_known_suffix_any_outcome_ok() {
        // Actions without recognized suffixes pass validation regardless of outcome_positive
        assert!(validate_outcome_positive(&make_commitment("custom.action", true)).is_ok());
        assert!(validate_outcome_positive(&make_commitment("custom.action", false)).is_ok());
    }
}
