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

    // RFC-MACP-0012 §6.1: an empty policy_version at SessionStart resolves to
    // "policy.default", and the runtime rewrites session.policy_version to the
    // resolved id. A client that started with "" must not be forced to echo a
    // value it never set, so an empty commitment.policy_version defers to the
    // session's bound policy. A non-empty value must match the binding exactly.
    // (The echo question is ambiguous upstream — filed as an RFC issue; empty-
    // matches is forward-compatible with either resolution.)
    if !commitment.policy_version.is_empty()
        && !session.policy_version.is_empty()
        && commitment.policy_version != session.policy_version
    {
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

/// Shared commitment policy gate (extracted from five per-mode copies).
/// Fail closed: only an explicit `Allow` proceeds — `PolicyDecision` is
/// `#[non_exhaustive]`, and any unknown decision denies.
pub fn enforce_commitment_policy(
    session: &Session,
    mode: macp_core::policy::CommitmentMode<'_>,
    outcome_positive: bool,
    evaluator: &dyn macp_core::policy::PolicyEvaluator,
) -> Result<(), MacpError> {
    let Some(ref policy) = session.policy_definition else {
        return Ok(());
    };
    let decision = evaluator.evaluate_commitment(&macp_core::policy::CommitmentContext {
        policy,
        participants: &session.participants,
        outcome_positive,
        mode,
    });
    match decision {
        macp_core::policy::PolicyDecision::Allow { .. } => Ok(()),
        macp_core::policy::PolicyDecision::Deny { reasons } => {
            tracing::warn!(
                session_id = %session.session_id,
                policy_id = %policy.policy_id,
                reasons = ?reasons,
                "policy denied commitment"
            );
            Err(MacpError::PolicyDenied { reasons })
        }
        other => {
            tracing::warn!(
                session_id = %session.session_id,
                policy_id = %policy.policy_id,
                decision = ?other,
                "unrecognized policy decision treated as denial"
            );
            Err(MacpError::PolicyDenied {
                reasons: vec!["unrecognized policy decision".into()],
            })
        }
    }
}

/// Shared mode-state JSON codec (extracted from six per-mode copies).
pub fn encode_mode_state<T: serde::Serialize>(state: &T) -> Vec<u8> {
    serde_json::to_vec(state).unwrap_or_default()
}

pub fn decode_mode_state<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, MacpError> {
    serde_json::from_slice(bytes).map_err(|_| MacpError::InvalidModeState)
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

fn extract_commitment_rules(
    rules: &serde_json::Value,
) -> macp_core::policy::rules::CommitmentRules {
    // Single implementation lives in macp-core (this was a byte-for-byte copy).
    macp_core::policy::extract_commitment_rules(rules)
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
        Session::builder("s1", "macp.mode.decision.v1", "agent://a")
            .ttl_ms(60_000)
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .build()
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

    // --- policy_version echo (master plan §2.3) ---

    /// A session that started with empty policy_version is rewritten to
    /// "policy.default" by the runtime; the client must not be required to echo
    /// a value it never sent.
    #[test]
    fn empty_commitment_policy_version_matches_bound_policy() {
        let mut session = session_for_commitment();
        session.policy_version = "policy.default".into();
        let c = make_commitment("decision.selected", true); // policy_version: ""
        assert!(validate_commitment_payload_for_session(&session, &c.encode_to_vec()).is_ok());
    }

    #[test]
    fn wrong_commitment_policy_version_rejected() {
        let mut session = session_for_commitment();
        session.policy_version = "policy.default".into();
        let mut c = make_commitment("decision.selected", true);
        c.policy_version = "policy.other.v1".into();
        assert!(validate_commitment_payload_for_session(&session, &c.encode_to_vec()).is_err());
    }

    #[test]
    fn exact_commitment_policy_version_accepted() {
        let mut session = session_for_commitment();
        session.policy_version = "policy.default".into();
        let mut c = make_commitment("decision.selected", true);
        c.policy_version = "policy.default".into();
        assert!(validate_commitment_payload_for_session(&session, &c.encode_to_vec()).is_ok());
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
