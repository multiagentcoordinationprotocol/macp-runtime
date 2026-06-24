use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;

/// Generic extension mode handler for dynamically registered modes.
///
/// Accepts any message type listed in the mode descriptor. Commitment messages
/// from the initiator resolve the session. All other messages are accepted and
/// the payload is persisted as mode state.
pub struct PassthroughMode {
    pub allowed_message_types: Vec<String>,
}

impl Mode for PassthroughMode {
    fn on_session_start(
        &self,
        _session: &Session,
        _env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        Ok(ModeResponse::NoOp)
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        if !self.allowed_message_types.is_empty()
            && !self
                .allowed_message_types
                .iter()
                .any(|t| t == &env.message_type)
        {
            return Err(MacpError::InvalidPayload);
        }

        if env.message_type == "Commitment" {
            let commitment =
                crate::mode::util::validate_commitment_payload_for_session(session, &env.payload)?;
            let resolution = serde_json::json!({
                "action": commitment.action,
                "commitment_id": commitment.commitment_id,
            })
            .to_string()
            .into_bytes();
            return Ok(ModeResponse::Resolve(resolution));
        }

        Ok(ModeResponse::PersistState(env.payload.clone()))
    }

    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        if env.message_type == "Commitment" {
            if env.sender != session.initiator_sender {
                return Err(MacpError::Forbidden);
            }
            return Ok(());
        }
        if !session.participants.is_empty() && !session.participants.contains(&env.sender) {
            return Err(MacpError::Forbidden);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_core::session::SessionState;
    use macp_pb::pb::CommitmentPayload;
    use prost::Message;

    fn make_session() -> Session {
        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry: i64::MAX,
            ttl_ms: 60_000,
            started_at_unix_ms: 1000,
            resolution: None,
            mode: "ext.test.v1".into(),
            mode_state: vec![],
            participants: vec!["alice".into(), "bob".into()],
            seen_message_ids: std::collections::HashSet::new(),
            intent: String::new(),
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            initiator_sender: "alice".into(),
            participant_message_counts: std::collections::HashMap::new(),
            participant_last_seen: std::collections::HashMap::new(),
            policy_definition: None,
            suspended_at_ms: None,
            accumulated_suspended_ms: 0,
        }
    }

    fn make_env(sender: &str, message_type: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "ext.test.v1".into(),
            message_type: message_type.into(),
            message_id: "m1".into(),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 1000,
            payload,
        }
    }

    #[test]
    fn accepts_any_message_when_no_filter() {
        let mode = PassthroughMode {
            allowed_message_types: vec![],
        };
        let session = make_session();
        let env = make_env("alice", "CustomMessage", b"data".to_vec());
        let result = mode.on_message(&session, &env).unwrap();
        assert!(matches!(result, ModeResponse::PersistState(_)));
    }

    #[test]
    fn rejects_unlisted_message_type() {
        let mode = PassthroughMode {
            allowed_message_types: vec!["Allowed".into()],
        };
        let session = make_session();
        let env = make_env("alice", "NotAllowed", vec![]);
        assert!(mode.on_message(&session, &env).is_err());
    }

    #[test]
    fn commitment_resolves_session() {
        let mode = PassthroughMode {
            allowed_message_types: vec!["Commitment".into()],
        };
        let session = make_session();
        let payload = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "test.done".into(),
            authority_scope: "test".into(),
            reason: "done".into(),
            mode_version: "1.0.0".into(),
            policy_version: String::new(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        let env = make_env("alice", "Commitment", payload);
        let result = mode.on_message(&session, &env).unwrap();
        assert!(matches!(result, ModeResponse::Resolve(_)));
    }

    #[test]
    fn non_initiator_commitment_forbidden() {
        let mode = PassthroughMode {
            allowed_message_types: vec!["Commitment".into()],
        };
        let session = make_session();
        let env = make_env("bob", "Commitment", vec![]);
        assert!(mode.authorize_sender(&session, &env).is_err());
    }
}
