use crate::mode::util::validate_commitment_payload_for_session;
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Internal state tracked across rounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiRoundState {
    pub round: u64,
    pub participants: Vec<String>,
    pub contributions: BTreeMap<String, String>,
    #[serde(default)]
    pub convergence_type: String,
    #[serde(default)]
    pub converged: bool,
}

/// Legacy JSON shape for Contribute messages (pre-proto wire format).
#[derive(Debug, Clone, Deserialize)]
struct ContributeJson {
    value: String,
}

/// Parse a Contribute payload: canonical protobuf
/// (`macp.modes.multi_round.v1.ContributePayload`) or the legacy JSON
/// `{"value": "..."}`.
///
/// JSON is tried FIRST, permanently. Every payload accepted before the proto
/// encoding existed was JSON, and replay must parse those bytes identically
/// forever (RFC-MACP-0003 §1). Trying proto first would let pathological JSON
/// bytes decode as a *valid* proto message with a different value (e.g. `{`
/// opens a proto group that a later `|` byte closes) and silently change a
/// replayed contribution. A proto payload never parses as a JSON object, so
/// this order is deterministic and costs proto senders one failed JSON parse.
fn parse_contribute_value(payload: &[u8]) -> Result<String, MacpError> {
    // Empty payloads were always rejected in the JSON era (and canonical
    // proto3 encoding cannot produce a non-empty encoding for value "");
    // keep rejecting them rather than accepting an empty contribution.
    if payload.is_empty() {
        return Err(MacpError::InvalidPayload);
    }
    if let Ok(text) = std::str::from_utf8(payload) {
        if let Ok(c) = serde_json::from_str::<ContributeJson>(text) {
            return Ok(c.value);
        }
    }
    <macp_pb::multi_round_pb::ContributePayload as prost::Message>::decode(payload)
        .map(|c| c.value)
        .map_err(|_| MacpError::InvalidPayload)
}

/// Resolution payload emitted on convergence.
#[derive(Debug, Serialize)]
struct ResolutionPayload {
    converged_value: String,
    round: u64,
    #[serde(rename = "final")]
    final_values: BTreeMap<String, String>,
}

pub struct MultiRoundMode;

impl MultiRoundMode {
    fn encode_state(state: &MultiRoundState) -> Vec<u8> {
        crate::mode::util::encode_mode_state(state)
    }

    fn decode_state(data: &[u8]) -> Result<MultiRoundState, MacpError> {
        crate::mode::util::decode_mode_state(data)
    }

    fn check_convergence(state: &MultiRoundState) -> bool {
        let all_contributed = state
            .participants
            .iter()
            .all(|p| state.contributions.contains_key(p));

        if !all_contributed {
            return false;
        }

        let values: Vec<&String> = state.contributions.values().collect();
        values.windows(2).all(|w| w[0] == w[1])
    }
}

impl Mode for MultiRoundMode {
    fn on_session_start(
        &self,
        session: &Session,
        _env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        let participants = session.participants.clone();

        if participants.is_empty() {
            return Err(MacpError::InvalidPayload);
        }

        let state = MultiRoundState {
            round: 0,
            participants,
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };

        Ok(ModeResponse::PersistState(Self::encode_state(&state)))
    }

    fn on_message(&self, session: &Session, env: &Envelope) -> Result<ModeResponse, MacpError> {
        match env.message_type.as_str() {
            "Contribute" => self.handle_contribute(session, env),
            "Commitment" => self.handle_commitment(session, env),
            _ => Err(MacpError::InvalidPayload),
        }
    }

    fn authorize_sender(&self, session: &Session, env: &Envelope) -> Result<(), MacpError> {
        if env.message_type == "Commitment" {
            // Only the initiator can emit Commitment
            if env.sender != session.initiator_sender {
                return Err(MacpError::Forbidden);
            }
            return Ok(());
        }
        // Default: must be a declared participant
        if !session.participants.is_empty() && !session.participants.contains(&env.sender) {
            return Err(MacpError::Forbidden);
        }
        Ok(())
    }
}

impl MultiRoundMode {
    fn handle_contribute(
        &self,
        session: &Session,
        env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        let mut state = Self::decode_state(&session.mode_state)?;

        if state.converged {
            return Err(MacpError::InvalidPayload);
        }

        let value = parse_contribute_value(&env.payload)?;

        let previous = state.contributions.get(&env.sender);
        let value_changed = previous.is_none_or(|prev| *prev != value);

        if value_changed {
            state.round += 1;
            state.contributions.insert(env.sender.clone(), value);
        }

        if Self::check_convergence(&state) {
            state.converged = true;
        }

        Ok(ModeResponse::PersistState(Self::encode_state(&state)))
    }

    fn handle_commitment(
        &self,
        session: &Session,
        env: &Envelope,
    ) -> Result<ModeResponse, MacpError> {
        let state = Self::decode_state(&session.mode_state)?;

        if !state.converged {
            return Err(MacpError::InvalidPayload);
        }

        validate_commitment_payload_for_session(session, &env.payload)?;

        let converged_value = state
            .contributions
            .values()
            .next()
            .cloned()
            .unwrap_or_default();
        let resolution = ResolutionPayload {
            converged_value,
            round: state.round,
            final_values: state.contributions.clone(),
        };
        let resolution_bytes =
            serde_json::to_vec(&resolution).expect("ResolutionPayload is always serializable");

        Ok(ModeResponse::PersistAndResolve {
            state: Self::encode_state(&state),
            resolution: resolution_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use macp_pb::pb::CommitmentPayload;
    use prost::Message;

    fn base_session() -> Session {
        Session::builder("s1", "ext.multi_round.v1", "coordinator")
            .ttl_ms(60_000)
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .build()
    }

    fn session_start_env() -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "SessionStart".into(),
            message_id: "m0".into(),
            session_id: "s1".into(),
            sender: "coordinator".into(),
            timestamp_unix_ms: 1_700_000_000_000,
            payload: vec![],
        }
    }

    fn contribute_env_with_payload(sender: &str, payload: Vec<u8>) -> Envelope {
        Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "Contribute".into(),
            message_id: format!("m_{}", sender),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// Canonical proto encoding — the primary wire format.
    fn contribute_env(sender: &str, value: &str) -> Envelope {
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: value.into(),
        }
        .encode_to_vec();
        contribute_env_with_payload(sender, payload)
    }

    /// Legacy JSON encoding — kept accepted for replay compatibility.
    fn contribute_env_json(sender: &str, value: &str) -> Envelope {
        let payload = serde_json::json!({"value": value}).to_string();
        contribute_env_with_payload(sender, payload.into_bytes())
    }

    fn commitment_env(sender: &str) -> Envelope {
        let payload = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "multi_round.converged".into(),
            authority_scope: "test".into(),
            reason: "converged".into(),
            mode_version: "1.0.0".into(),
            policy_version: String::new(),
            configuration_version: "cfg-1".into(),
            outcome_positive: true,
            supersedes: None,
        }
        .encode_to_vec();
        Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "Commitment".into(),
            message_id: "m_commit".into(),
            session_id: "s1".into(),
            sender: sender.into(),
            timestamp_unix_ms: 1_700_000_000_000,
            payload,
        }
    }

    fn session_with_state(state: &MultiRoundState) -> Session {
        let mut s = base_session();
        s.mode_state = MultiRoundMode::encode_state(state);
        s.participants = state.participants.clone();
        s
    }

    #[test]
    fn session_start_parses_valid_config() {
        let mode = MultiRoundMode;
        let mut session = base_session();
        session.participants = vec!["alice".into(), "bob".into()];
        let env = session_start_env();

        let result = mode.on_session_start(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.round, 0);
                assert_eq!(state.participants, vec!["alice", "bob"]);
                assert!(state.contributions.is_empty());
                assert!(!state.converged);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn session_start_rejects_empty_participants() {
        let mode = MultiRoundMode;
        let session = base_session();
        let env = session_start_env();

        let err = mode.on_session_start(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn contribute_first_value_increments_round() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("alice", "option_a");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(new_state.round, 1);
                assert_eq!(new_state.contributions.get("alice").unwrap(), "option_a");
                assert!(!new_state.converged);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn resubmit_same_value_does_not_increment_round() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 1,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("alice", "option_a");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(new_state.round, 1);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn revise_value_increments_round() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 1,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("alice", "option_b");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(new_state.round, 2);
                assert_eq!(new_state.contributions.get("alice").unwrap(), "option_b");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn convergence_sets_converged_flag() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 1,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("bob", "option_a");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(new_state.round, 2);
                assert!(new_state.converged);
            }
            _ => panic!("Expected PersistState (convergence tracked, not auto-resolved)"),
        }
    }

    #[test]
    fn commitment_after_convergence_resolves() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        contributions.insert("bob".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 2,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: true,
        };
        let session = session_with_state(&state);
        let env = commitment_env("coordinator");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistAndResolve { resolution, .. } => {
                let res: serde_json::Value = serde_json::from_slice(&resolution).unwrap();
                assert_eq!(res["converged_value"], "option_a");
                assert_eq!(res["round"], 2);
            }
            _ => panic!("Expected PersistAndResolve"),
        }
    }

    #[test]
    fn commitment_before_convergence_rejected() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = commitment_env("coordinator");

        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn contribute_after_convergence_rejected() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        contributions.insert("bob".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 2,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: true,
        };
        let session = session_with_state(&state);
        let env = contribute_env("alice", "option_b");

        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn non_initiator_commitment_rejected() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        contributions.insert("bob".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 2,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: true,
        };
        let session = session_with_state(&state);
        let env = commitment_env("alice"); // not the initiator

        let err = mode.authorize_sender(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "Forbidden");
    }

    #[test]
    fn no_convergence_when_values_differ() {
        let mode = MultiRoundMode;
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 1,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("bob", "option_b");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert!(!new_state.converged);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    #[test]
    fn no_convergence_when_not_all_contributed() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into(), "carol".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("alice", "option_a");

        let result = mode.on_message(&session, &env).unwrap();
        assert!(matches!(result, ModeResponse::PersistState(_)));
    }

    #[test]
    fn non_contribute_message_rejected() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "Message".into(),
            message_id: "m1".into(),
            session_id: "s1".into(),
            sender: "alice".into(),
            timestamp_unix_ms: 1_700_000_000_000,
            payload: b"hello".to_vec(),
        };

        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.error_code(), "INVALID_ENVELOPE");
    }

    #[test]
    fn contribute_invalid_payload_returns_error() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "Contribute".into(),
            message_id: "m1".into(),
            session_id: "s1".into(),
            sender: "alice".into(),
            timestamp_unix_ms: 1_700_000_000_000,
            payload: b"not json".to_vec(),
        };

        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    /// Replay compatibility: pre-proto histories carry JSON Contribute
    /// payloads, and they must keep parsing to the identical value forever
    /// (RFC-MACP-0003 §1).
    #[test]
    fn contribute_json_fallback_still_accepted() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);

        let result = mode
            .on_message(&session, &contribute_env_json("alice", "option_a"))
            .unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.contributions["alice"], "option_a");
                assert_eq!(state.round, 1);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    /// The two encodings must be interchangeable mid-session: a JSON
    /// contribution revised via proto (same value) counts as unchanged.
    #[test]
    fn proto_and_json_contributions_are_equivalent() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);

        let after_json = match mode
            .on_message(&session, &contribute_env_json("alice", "option_a"))
            .unwrap()
        {
            ModeResponse::PersistState(data) => data,
            _ => panic!("Expected PersistState"),
        };
        let session = {
            let state: MultiRoundState = serde_json::from_slice(&after_json).unwrap();
            session_with_state(&state)
        };

        // Same value re-sent as proto: no round advance (value unchanged).
        match mode
            .on_message(&session, &contribute_env("alice", "option_a"))
            .unwrap()
        {
            ModeResponse::PersistState(data) => {
                let state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.round, 1, "unchanged value must not advance the round");
                assert_eq!(state.contributions["alice"], "option_a");
            }
            _ => panic!("Expected PersistState"),
        }
    }

    /// Empty payloads were always rejected in the JSON era; the proto path
    /// must not turn them into an accepted empty contribution.
    #[test]
    fn contribute_empty_payload_rejected() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env_with_payload("alice", vec![]);

        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn encode_decode_round_trip() {
        let mut contributions = BTreeMap::new();
        contributions.insert("alice".into(), "value_a".into());
        let original = MultiRoundState {
            round: 5,
            participants: vec!["alice".into(), "bob".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: true,
        };

        let encoded = MultiRoundMode::encode_state(&original);
        let decoded = MultiRoundMode::decode_state(&encoded).unwrap();

        assert_eq!(decoded.round, original.round);
        assert_eq!(decoded.participants, original.participants);
        assert_eq!(decoded.contributions, original.contributions);
        assert_eq!(decoded.converged, original.converged);
    }

    #[test]
    fn decode_invalid_state_returns_error() {
        let err = MultiRoundMode::decode_state(b"garbage").unwrap_err();
        assert_eq!(err.to_string(), "InvalidModeState");
    }

    #[test]
    fn three_participant_convergence() {
        let mode = MultiRoundMode;

        let mut contributions = BTreeMap::new();
        contributions.insert("alice".to_string(), "option_a".to_string());
        contributions.insert("bob".to_string(), "option_a".to_string());
        let state = MultiRoundState {
            round: 2,
            participants: vec!["alice".into(), "bob".into(), "carol".into()],
            contributions,
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = contribute_env("carol", "option_a");

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let new_state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert!(new_state.converged);
            }
            _ => panic!("Expected PersistState with converged=true"),
        }
    }

    #[test]
    fn unknown_message_type_rejected() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into(), "bob".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        let env = Envelope {
            macp_version: "1.0".into(),
            mode: "ext.multi_round.v1".into(),
            message_type: "UnknownType".into(),
            message_id: "msg-unknown".into(),
            session_id: "s1".into(),
            sender: "alice".into(),
            timestamp_unix_ms: 0,
            payload: vec![],
        };
        let err = mode.on_message(&session, &env).unwrap_err();
        assert_eq!(err.error_code(), "INVALID_ENVELOPE");
    }
}
