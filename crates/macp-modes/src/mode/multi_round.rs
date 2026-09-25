use crate::mode::util::validate_commitment_payload_for_session;
use crate::mode::{Mode, ModeResponse};
use macp_core::error::MacpError;
use macp_core::session::Session;
use macp_pb::pb::Envelope;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Internal state tracked across rounds.
///
/// **`#[non_exhaustive]`** (0.8.0, `DECISIONS.md` D7), for the same reason as
/// the handoff, quorum, proposal and task records: this is runtime-produced
/// coordination state, deserialized from accepted envelopes, that grows a
/// field whenever the mode learns something new —
/// [`convergence_type`](Self::convergence_type) and
/// [`converged`](Self::converged) both carry `#[serde(default)]` because they
/// were added after the fact, and each would be a
/// `constructible_struct_adds_field` major today. `release-plz.toml`'s
/// `semver_check = true` turns that into a blocked release PR across all seven
/// lockstep crates. 0.8.0 is already being taken for the handoff field, so
/// sealing the rest of the class here costs nothing extra and makes every
/// future field additive.
///
/// Fields stay `pub` and readable; only construction by struct literal from
/// another crate is refused, and nothing outside `macp-modes` constructs one —
/// the mode mints it from accepted envelopes, which is why no constructor is
/// offered in its place.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
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
/// replayed contribution.
///
/// A canonical proto payload CAN also parse as a JSON object: at value
/// byte-lengths 13, 32, and 123, the field-1 tag byte and/or the following
/// length-varint byte are themselves insignificant JSON whitespace (or, at
/// 123, the literal `{`), so `serde_json` skips them and misparses the
/// value's own bytes as a `{"value":"..."}`-shaped object (issue #192). At
/// `semantics_rev >= 3`, a successful JSON parse is therefore trusted only
/// when the same bytes do NOT also round-trip byte-identically through the
/// canonical proto encoding — see [`macp_core::session::CURRENT_SEMANTICS_REV`]
/// revision 3. Revisions 0-2 keep the unconditional (and, at those three
/// lengths, wrong) JSON-first reading, so already-persisted histories replay
/// to the exact outcome they were originally accepted with.
///
/// `#[doc(hidden)] pub` solely so `tests/parity_contract.rs` (in the root
/// `macp-runtime` crate) can assert this predicate directly, against
/// `schemas/parity/contract.json`'s `contribute_payload`/`contribute_acceptance`
/// vectors, instead of reimplementing it. Not a stability promise.
#[doc(hidden)]
pub fn parse_contribute_value(payload: &[u8], semantics_rev: u32) -> Result<String, MacpError> {
    // Empty payloads were always rejected in the JSON era (and canonical
    // proto3 encoding cannot produce a non-empty encoding for value "");
    // keep rejecting them rather than accepting an empty contribution.
    if payload.is_empty() {
        return Err(MacpError::InvalidPayload);
    }
    let proto_decoded =
        <macp_pb::multi_round_pb::ContributePayload as Message>::decode(payload).ok();
    if let Ok(text) = std::str::from_utf8(payload) {
        if let Ok(c) = serde_json::from_str::<ContributeJson>(text) {
            let trust_proto = semantics_rev >= 3
                && proto_decoded
                    .as_ref()
                    .is_some_and(|m| m.encode_to_vec() == payload);
            if !trust_proto {
                return Ok(c.value);
            }
            tracing::debug!(
                value_len = payload.len(),
                "Contribute payload is both valid JSON and canonical proto; preferring proto (issue #192 tie-break)"
            );
        }
    }
    proto_decoded
        .map(|c| c.value)
        .ok_or(MacpError::InvalidPayload)
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

        let value = parse_contribute_value(&env.payload, session.semantics_rev)?;

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

    use macp_core::session::CURRENT_SEMANTICS_REV;
    use macp_pb::pb::CommitmentPayload;

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

    // --- issue #192: canonical-proto/JSON collision tie-break ---------------

    /// The 123-byte, no-leading-brace value shape that collides at rev2 via
    /// the length-byte-is-`{` mechanism (acceptance criterion 3). Shared
    /// with `reverse_direction_residual_is_a_documented_trade_off_at_rev3`,
    /// which reads the identical 125-byte proto encoding from the opposite
    /// direction -- see that test's doc comment.
    fn length_123_no_leading_brace_value() -> String {
        format!(r#""value":"{}"}}"#, "a".repeat(112))
    }

    #[test]
    fn canonical_proto_length_13_preserved_wrong_at_rev2() {
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: r#"{"value":"x"}"#.into(),
        }
        .encode_to_vec();
        assert_eq!(payload.len(), 15);

        let decoded = parse_contribute_value(&payload, 2).unwrap();
        assert_eq!(decoded, "x");
    }

    #[test]
    fn canonical_proto_length_13_corrected_at_rev3() {
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: r#"{"value":"x"}"#.into(),
        }
        .encode_to_vec();

        let decoded = parse_contribute_value(&payload, 3).unwrap();
        assert_eq!(decoded, r#"{"value":"x"}"#);
    }

    #[test]
    fn canonical_proto_length_32_preserved_wrong_at_rev2() {
        let filler = "a".repeat(20);
        let value = format!(r#"{{"value":"{}"}}"#, filler);
        assert_eq!(value.len(), 32);
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: value.clone(),
        }
        .encode_to_vec();

        let decoded = parse_contribute_value(&payload, 2).unwrap();
        assert_eq!(decoded, filler);
        assert_ne!(decoded, value);
    }

    #[test]
    fn canonical_proto_length_32_corrected_at_rev3() {
        let filler = "a".repeat(20);
        let value = format!(r#"{{"value":"{}"}}"#, filler);
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: value.clone(),
        }
        .encode_to_vec();

        let decoded = parse_contribute_value(&payload, 3).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn canonical_proto_length_123_preserved_wrong_at_rev2() {
        let value = length_123_no_leading_brace_value();
        assert_eq!(value.len(), 123);
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: value.clone(),
        }
        .encode_to_vec();

        let decoded = parse_contribute_value(&payload, 2).unwrap();
        assert_eq!(decoded, "a".repeat(112));
        assert_ne!(decoded, value);
    }

    #[test]
    fn canonical_proto_length_123_corrected_at_rev3() {
        let value = length_123_no_leading_brace_value();
        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: value.clone(),
        }
        .encode_to_vec();

        let decoded = parse_contribute_value(&payload, 3).unwrap();
        assert_eq!(decoded, value);
    }

    /// Acceptance criterion 5: this is the IDENTICAL 125-byte payload as
    /// `canonical_proto_length_123_*` (`length_123_no_leading_brace_value`),
    /// read from the opposite direction. `[0x0a]` prepended to the JSON
    /// object `{"value":"<112 a's>"}` is simultaneously: (a) valid legacy
    /// JSON with an incidental leading-whitespace byte, meaning "112 a's",
    /// and (b) the canonical proto encoding of the 123-byte string
    /// `"value":"<112 a's>"}`. Both readings are valid; no further tie-break
    /// can distinguish them from the bytes alone. The tie-break's choice to
    /// prefer proto is what makes the sibling `length_123` test a genuine
    /// *fix* and makes this test's case a documented, accepted residual --
    /// the identical trade-off `macp-sdk-python`'s PR #77 already accepted
    /// for the same reason (see this plan's Context section).
    #[test]
    fn reverse_direction_residual_is_a_documented_trade_off_at_rev3() {
        let content = "a".repeat(112);
        let legacy_json_reading = format!(r#"{{"value":"{}"}}"#, content);
        assert_eq!(legacy_json_reading.len(), 124);

        let mut payload = vec![0x0Au8];
        payload.extend_from_slice(legacy_json_reading.as_bytes());
        assert_eq!(payload.len(), 125);

        let decoded = parse_contribute_value(&payload, 3).expect("must decode");
        assert_eq!(
            decoded,
            length_123_no_leading_brace_value(),
            "proto reading must win -- the identical bytes as canonical_proto_length_123_*, \
             read from the opposite direction"
        );
        assert_ne!(
            decoded, content,
            "the legacy-JSON reading (112 a's) is a real, accepted, documented residual, \
             not what this returns"
        );
    }

    /// Regression pin for the round-trip canonicality check itself: a
    /// payload that decodes via prost -- because field 4 is unrecognized by
    /// `ContributePayload` and silently skipped, per `crates/macp-pb/build.rs`'s
    /// plain `tonic_prost_build::configure()` with no unknown-field
    /// retention -- but is NOT the canonical encoding (re-encoding drops the
    /// unknown field the original bytes carried) must still be read as
    /// JSON. Confirms the round-trip check discriminates "parses as *some*
    /// protobuf message" from "*is* the canonical encoding of this
    /// message," which is what makes the tie-break safe without Python's
    /// extra `DiscardUnknownFields()` step.
    #[test]
    fn whitespace_prefixed_legacy_json_with_unknown_proto_field_shape_still_decodes_as_json() {
        // tag(field1,LEN)=0x0a, len=0x0d(13), the 13-byte legacy JSON value,
        // then an unknown field 4 (varint, value 10) whose tag/value bytes
        // (0x20, 0x0a) are also valid trailing JSON whitespace.
        let mut payload = vec![0x0Au8, 0x0Du8];
        payload.extend_from_slice(br#"{"value":"x"}"#);
        payload.extend_from_slice(&[0x20u8, 0x0Au8]);
        assert_eq!(payload.len(), 17);

        // Sanity: this really does decode as *some* protobuf message (the
        // unknown field 4 is silently skipped), just not canonically.
        let proto_decoded =
            <macp_pb::multi_round_pb::ContributePayload as Message>::decode(payload.as_slice())
                .expect("prost must silently skip the unrecognized field 4 and decode field 1");
        assert_eq!(proto_decoded.value, "{\"value\":\"x\"}");
        assert_ne!(
            proto_decoded.encode_to_vec(),
            payload,
            "re-encoding must drop the unknown field 4, proving this payload is not canonical"
        );

        let decoded = parse_contribute_value(&payload, 3).expect("must decode");
        assert_eq!(
            decoded, "x",
            "non-canonical proto must not defeat the tie-break; JSON is correctly preferred"
        );
    }

    /// A property-style, differential sweep across five value shapes and
    /// every length 1..=300 (acceptance criterion 4): proves the rev-3 fix
    /// is universally correct (not just at the three named reproducer
    /// lengths) and that the pre-fix rev-2 bug is *exactly* characterized --
    /// it mis-decodes at length 13 and 32 for every leading-brace-template
    /// shape and at length 123 for the no-leading-brace shape, and nowhere
    /// else in the swept range.
    #[test]
    fn canonical_proto_exhaustively_round_trips_at_rev3() {
        fn cycle_fill(pattern: &[u8], len: usize) -> String {
            (0..len)
                .map(|i| pattern[i % pattern.len()] as char)
                .collect()
        }

        // Leading-brace template: `{"value":"<filler>"}`. Skeleton (empty
        // filler) is 12 bytes; below that, an arbitrary filler is used
        // that cannot collide at any length (too short to be a valid
        // object).
        fn leading_brace(pattern: &[u8], len: usize) -> String {
            const SKELETON: usize = 12;
            if len < SKELETON {
                return cycle_fill(pattern, len);
            }
            format!(r#"{{"value":"{}"}}"#, cycle_fill(pattern, len - SKELETON))
        }

        // No-leading-brace template: `"value":"<filler>"}`. Skeleton is 11
        // bytes; exercises the length-byte-is-`{` mechanism, only at
        // len=123.
        fn no_leading_brace(pattern: &[u8], len: usize) -> String {
            const SKELETON: usize = 11;
            if len < SKELETON {
                return cycle_fill(pattern, len);
            }
            format!(r#""value":"{}"}}"#, cycle_fill(pattern, len - SKELETON))
        }

        let shapes: [(&str, fn(usize) -> String); 5] = [
            ("plain_ascii", |len| leading_brace(b"a", len)),
            ("all_digit", |len| leading_brace(b"7", len)),
            ("quoted_json_string_shaped", |len| {
                leading_brace(b"1a:", len)
            }),
            ("object_shaped", |len| leading_brace(b"{}", len)),
            ("no_leading_brace", |len| no_leading_brace(b"a", len)),
        ];

        for (shape_name, make_value) in shapes {
            let expected_rev2_mismatches: &[usize] = if shape_name == "no_leading_brace" {
                &[123]
            } else {
                &[13, 32]
            };

            for len in 1..=300usize {
                let value = make_value(len);
                assert_eq!(
                    value.len(),
                    len,
                    "shape {shape_name} generator produced the wrong length"
                );
                let payload = macp_pb::multi_round_pb::ContributePayload {
                    value: value.clone(),
                }
                .encode_to_vec();

                let rev3 = parse_contribute_value(&payload, 3).unwrap_or_else(|e| {
                    panic!("shape {shape_name} len {len}: rev3 decode failed: {e:?}")
                });
                assert_eq!(
                    rev3, value,
                    "shape {shape_name} len {len}: rev3 must always recover the true value"
                );

                let rev2 = parse_contribute_value(&payload, 2).unwrap_or_else(|e| {
                    panic!("shape {shape_name} len {len}: rev2 decode failed: {e:?}")
                });
                if expected_rev2_mismatches.contains(&len) {
                    assert_ne!(
                        rev2, value,
                        "shape {shape_name} len {len}: expected a known rev2 collision, \
                         but it decoded correctly"
                    );
                } else {
                    assert_eq!(
                        rev2, value,
                        "shape {shape_name} len {len}: unexpected rev2 mismatch outside \
                         the known collision set"
                    );
                }
            }
        }
    }

    /// End-to-end (issue #192): a fresh session (current `semantics_rev`)
    /// must resolve the length-13 collision payload to its true value, not
    /// the substring `serde_json` would misread, proving the fix is wired
    /// into the live `on_message` dispatch path, not just the free
    /// function.
    #[test]
    fn handle_contribute_end_to_end_applies_the_rev3_fix() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let session = session_with_state(&state);
        assert_eq!(
            session.semantics_rev, CURRENT_SEMANTICS_REV,
            "a freshly-built session must bind the current revision"
        );

        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: r#"{"value":"x"}"#.into(),
        }
        .encode_to_vec();
        let env = contribute_env_with_payload("alice", payload);

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.contributions["alice"], r#"{"value":"x"}"#);
            }
            _ => panic!("Expected PersistState"),
        }
    }

    /// Sibling of the test above: an in-flight session bound to
    /// `semantics_rev` 2 (accepted before this fix shipped) must keep
    /// resolving the SAME payload to the historically-wrong value for the
    /// rest of its lifetime -- `semantics_rev` is fixed at `SessionStart`
    /// and never changes mid-session.
    #[test]
    fn handle_contribute_end_to_end_preserves_the_collision_at_rev2() {
        let mode = MultiRoundMode;
        let state = MultiRoundState {
            round: 0,
            participants: vec!["alice".into()],
            contributions: BTreeMap::new(),
            convergence_type: "all_equal".into(),
            converged: false,
        };
        let mut session = session_with_state(&state);
        session.semantics_rev = 2;

        let payload = macp_pb::multi_round_pb::ContributePayload {
            value: r#"{"value":"x"}"#.into(),
        }
        .encode_to_vec();
        let env = contribute_env_with_payload("alice", payload);

        let result = mode.on_message(&session, &env).unwrap();
        match result {
            ModeResponse::PersistState(data) => {
                let state: MultiRoundState = serde_json::from_slice(&data).unwrap();
                assert_eq!(state.contributions["alice"], "x");
            }
            _ => panic!("Expected PersistState"),
        }
    }
}
