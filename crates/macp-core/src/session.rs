use crate::error::MacpError;
use crate::mode::ModeResponse;
use crate::policy::PolicyDefinition;
use macp_pb::pb::SessionStartPayload;
use prost::Message;
use std::collections::{HashMap, HashSet};

pub const MAX_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Hard cap on the cumulative time a session may spend `Suspended` before it is
/// force-expired (RFC-MACP-0001 §7.5). Bounds indefinite human-in-the-loop holds.
pub const MAX_SUSPEND_MS: i64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionState {
    Open,
    /// Non-terminal pause of an `Open` session (RFC-MACP-0001 §7.5). TTL is
    /// banked while suspended; only `Open`<->`Suspended` and `Suspended`->
    /// `Expired`/`Cancelled` transitions are permitted.
    Suspended,
    Resolved,
    Expired,
    /// Terminal: ended by an accepted `CancelSession` (distinct from `Expired`).
    Cancelled,
}

impl SessionState {
    /// Terminal states accept no further transitions.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            SessionState::Resolved | SessionState::Expired | SessionState::Cancelled
        )
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub session_id: String,
    pub state: SessionState,
    pub ttl_expiry: i64,
    pub ttl_ms: i64,
    pub started_at_unix_ms: i64,
    pub resolution: Option<Vec<u8>>,
    pub mode: String,
    pub mode_state: Vec<u8>,
    pub participants: Vec<String>,
    pub seen_message_ids: HashSet<String>,
    pub intent: String,
    pub mode_version: String,
    pub configuration_version: String,
    pub policy_version: String,
    pub context_id: String,
    pub extensions: HashMap<String, Vec<u8>>,
    pub roots: Vec<macp_pb::pb::Root>,
    pub initiator_sender: String,
    pub participant_message_counts: HashMap<String, u32>,
    pub participant_last_seen: HashMap<String, i64>,
    pub policy_definition: Option<PolicyDefinition>,
    /// Wall-clock (session-timeline) ms at which the session was suspended, or
    /// `None` when not suspended. Used to bank TTL across a suspension (§7.5).
    pub suspended_at_ms: Option<i64>,
    /// Cumulative ms the session has spent suspended across all suspend/resume
    /// cycles. Drives the `MAX_SUSPEND_MS` cap.
    pub accumulated_suspended_ms: i64,
}

impl Session {
    pub fn record_participant_activity(&mut self, sender: &str, timestamp_ms: i64) {
        *self
            .participant_message_counts
            .entry(sender.to_string())
            .or_insert(0) += 1;
        self.participant_last_seen
            .insert(sender.to_string(), timestamp_ms);
    }

    /// Suspend an `Open` session (RFC-MACP-0001 §7.5). Records the suspend time
    /// so TTL can be banked on resume. Pure: no clock, no I/O — the caller
    /// injects `now_ms`.
    pub fn suspend(&mut self, now_ms: i64) -> Result<(), MacpError> {
        if self.state != SessionState::Open {
            return Err(MacpError::SessionNotOpen);
        }
        self.state = SessionState::Suspended;
        self.suspended_at_ms = Some(now_ms);
        Ok(())
    }

    /// Resume a `Suspended` session, banking the suspended duration into the
    /// TTL deadline (`ttl_expiry += now - suspended_at`). If the cumulative
    /// suspended time would exceed `MAX_SUSPEND_MS`, the session is force-expired
    /// instead and `MacpError::TtlExpired` is returned.
    pub fn resume(&mut self, now_ms: i64) -> Result<(), MacpError> {
        if self.state != SessionState::Suspended {
            return Err(MacpError::SessionNotOpen);
        }
        let suspended_at = self.suspended_at_ms.unwrap_or(now_ms);
        let banked = (now_ms - suspended_at).max(0);
        self.accumulated_suspended_ms = self.accumulated_suspended_ms.saturating_add(banked);
        self.suspended_at_ms = None;
        if self.accumulated_suspended_ms > MAX_SUSPEND_MS {
            self.state = SessionState::Expired;
            return Err(MacpError::TtlExpired);
        }
        self.ttl_expiry = self.ttl_expiry.saturating_add(banked);
        self.state = SessionState::Open;
        Ok(())
    }

    /// Cancel an `Open` or `Suspended` session into the terminal `Cancelled`
    /// state (RFC-MACP-0001 §7.3). Returns an error if already terminal.
    pub fn cancel(&mut self) -> Result<(), MacpError> {
        if self.state.is_terminal() {
            return Err(MacpError::SessionNotOpen);
        }
        self.state = SessionState::Cancelled;
        self.suspended_at_ms = None;
        Ok(())
    }

    /// Whether a currently-`Suspended` session has exceeded `MAX_SUSPEND_MS` as
    /// of `now_ms` (cumulative banked plus the in-progress suspension).
    pub fn suspend_cap_exceeded(&self, now_ms: i64) -> bool {
        match self.suspended_at_ms {
            Some(at) => {
                self.accumulated_suspended_ms
                    .saturating_add((now_ms - at).max(0))
                    > MAX_SUSPEND_MS
            }
            None => self.accumulated_suspended_ms > MAX_SUSPEND_MS,
        }
    }

    pub fn apply_mode_response(&mut self, response: ModeResponse) {
        match response {
            ModeResponse::NoOp => {}
            ModeResponse::PersistState(state) => self.mode_state = state,
            ModeResponse::Resolve(resolution) => {
                self.state = SessionState::Resolved;
                self.resolution = Some(resolution);
            }
            ModeResponse::PersistAndResolve { state, resolution } => {
                self.mode_state = state;
                self.state = SessionState::Resolved;
                self.resolution = Some(resolution);
            }
        }
    }
}

pub fn requires_strict_session_start(mode: &str) -> bool {
    matches!(
        mode,
        "macp.mode.decision.v1"
            | "macp.mode.proposal.v1"
            | "macp.mode.task.v1"
            | "macp.mode.handoff.v1"
            | "macp.mode.quorum.v1"
            | "ext.multi_round.v1"
    )
}

/// Parse a protobuf-encoded SessionStartPayload from raw bytes.
pub fn parse_session_start_payload(payload: &[u8]) -> Result<SessionStartPayload, MacpError> {
    if payload.is_empty() {
        return Err(MacpError::InvalidPayload);
    }
    SessionStartPayload::decode(payload).map_err(|_| MacpError::InvalidPayload)
}

/// Extract and validate TTL from a parsed SessionStartPayload.
pub fn extract_ttl_ms(payload: &SessionStartPayload) -> Result<i64, MacpError> {
    if !(1..=MAX_TTL_MS).contains(&payload.ttl_ms) {
        return Err(MacpError::InvalidTtl);
    }
    Ok(payload.ttl_ms)
}

/// Validate the complete canonical SessionStart binding contract.
pub fn validate_canonical_session_start_payload(
    payload: &SessionStartPayload,
) -> Result<(), MacpError> {
    extract_ttl_ms(payload)?;

    if payload.mode_version.trim().is_empty() || payload.configuration_version.trim().is_empty() {
        return Err(MacpError::InvalidPayload);
    }

    if payload.participants.is_empty() {
        return Err(MacpError::InvalidPayload);
    }

    // Safety limit: prevent resource exhaustion from excessively large participant lists.
    const MAX_PARTICIPANTS: usize = 1000;
    if payload.participants.len() > MAX_PARTICIPANTS {
        return Err(MacpError::InvalidPayload);
    }

    let mut seen = HashSet::new();
    for participant in &payload.participants {
        let participant = participant.trim();
        if participant.is_empty() || !seen.insert(participant.to_string()) {
            return Err(MacpError::InvalidPayload);
        }
    }

    Ok(())
}

/// Enforce the strict SessionStart binding contract for standards-track and qualifying extension modes.
pub fn validate_strict_session_start_payload(
    mode: &str,
    payload: &SessionStartPayload,
) -> Result<(), MacpError> {
    if !requires_strict_session_start(mode) {
        return Ok(());
    }

    validate_canonical_session_start_payload(payload)
}

/// Validate that a session ID meets the acceptance policy.
///
/// Accepts:
/// - UUID v4/v7 in hyphenated lowercase canonical form (36 chars)
/// - base64url tokens of 22+ chars (`[A-Za-z0-9_-]`)
///
/// Rejects everything else (empty, short human-readable, uppercase UUID, etc.).
pub fn validate_session_id_for_acceptance(session_id: &str) -> Result<(), MacpError> {
    if session_id.is_empty() {
        return Err(MacpError::InvalidSessionId);
    }

    // Try UUID parse: must be valid UUID v4 or v7, canonical lowercase hyphenated form
    if session_id.len() == 36 && session_id.contains('-') {
        if let Ok(parsed) = uuid::Uuid::parse_str(session_id) {
            // Verify it's the canonical lowercase hyphenated representation
            if parsed.as_hyphenated().to_string() == session_id {
                match parsed.get_version() {
                    Some(uuid::Version::Random) | Some(uuid::Version::SortRand) => {
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
        return Err(MacpError::InvalidSessionId);
    }

    // Try base64url: at least 22 chars, only [A-Za-z0-9_-]
    if session_id.len() >= 22
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Ok(());
    }

    Err(MacpError::InvalidSessionId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn encode_payload(ttl_ms: i64, participants: Vec<String>) -> Vec<u8> {
        let payload = SessionStartPayload {
            intent: String::new(),
            participants,
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            policy_version: String::new(),
            ttl_ms,
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
        };
        payload.encode_to_vec()
    }

    #[test]
    fn parse_empty_payload_is_invalid() {
        let err = parse_session_start_payload(b"").unwrap_err();
        assert_eq!(err.to_string(), "InvalidPayload");
    }

    #[test]
    fn parse_valid_protobuf_payload() {
        let bytes = encode_payload(5000, vec!["alice".into(), "bob".into()]);
        let result = parse_session_start_payload(&bytes).unwrap();
        assert_eq!(result.ttl_ms, 5000);
        assert_eq!(result.participants, vec!["alice", "bob"]);
    }

    #[test]
    fn extract_ttl_requires_explicit_positive_value() {
        let payload = SessionStartPayload::default();
        assert_eq!(
            extract_ttl_ms(&payload).unwrap_err().to_string(),
            "InvalidTtl"
        );

        let payload = SessionStartPayload {
            ttl_ms: 5000,
            ..Default::default()
        };
        assert_eq!(extract_ttl_ms(&payload).unwrap(), 5000);
    }

    #[test]
    fn standard_mode_requires_explicit_versions_and_participants() {
        let payload = SessionStartPayload {
            participants: vec!["alice".into()],
            mode_version: String::new(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );

        let payload = SessionStartPayload {
            participants: vec![],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.decision.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    fn open_session(ttl_expiry: i64) -> Session {
        Session {
            session_id: "s1".into(),
            state: SessionState::Open,
            ttl_expiry,
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
    fn suspend_then_resume_banks_ttl() {
        let mut s = open_session(10_000);
        s.suspend(2_000).unwrap();
        assert_eq!(s.state, SessionState::Suspended);
        assert_eq!(s.suspended_at_ms, Some(2_000));
        // Resume 3_000ms later: banked 3_000 is added to the deadline.
        s.resume(5_000).unwrap();
        assert_eq!(s.state, SessionState::Open);
        assert_eq!(s.ttl_expiry, 13_000);
        assert_eq!(s.accumulated_suspended_ms, 3_000);
        assert_eq!(s.suspended_at_ms, None);
    }

    #[test]
    fn suspend_requires_open_and_resume_requires_suspended() {
        let mut s = open_session(10_000);
        // resume on an Open session is rejected
        assert!(matches!(
            s.resume(1).unwrap_err(),
            MacpError::SessionNotOpen
        ));
        s.suspend(1).unwrap();
        // double-suspend rejected
        assert!(matches!(
            s.suspend(2).unwrap_err(),
            MacpError::SessionNotOpen
        ));
    }

    #[test]
    fn resume_exceeding_max_suspend_expires() {
        let mut s = open_session(10_000);
        s.suspend(0).unwrap();
        // Resume after more than MAX_SUSPEND_MS: force-expired.
        let err = s.resume(MAX_SUSPEND_MS + 1).unwrap_err();
        assert!(matches!(err, MacpError::TtlExpired));
        assert_eq!(s.state, SessionState::Expired);
    }

    #[test]
    fn cancel_from_open_or_suspended_then_terminal_is_rejected() {
        let mut s = open_session(10_000);
        s.suspend(1).unwrap();
        s.cancel().unwrap();
        assert_eq!(s.state, SessionState::Cancelled);
        assert_eq!(s.suspended_at_ms, None);
        // Already terminal: further cancel is rejected.
        assert!(matches!(s.cancel().unwrap_err(), MacpError::SessionNotOpen));

        let mut open = open_session(10_000);
        open.cancel().unwrap();
        assert_eq!(open.state, SessionState::Cancelled);
    }

    #[test]
    fn standard_mode_rejects_duplicate_participants() {
        let payload = SessionStartPayload {
            participants: vec!["alice".into(), "alice".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 1000,
            ..Default::default()
        };
        assert_eq!(
            validate_strict_session_start_payload("macp.mode.proposal.v1", &payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn multi_round_requires_strict_session_start() {
        let payload = SessionStartPayload::default();
        assert!(validate_strict_session_start_payload("ext.multi_round.v1", &payload).is_err());
    }

    #[test]
    fn valid_uuid_v4_accepted() {
        let id = uuid::Uuid::new_v4().as_hyphenated().to_string();
        validate_session_id_for_acceptance(&id).unwrap();
    }

    #[test]
    fn valid_base64url_accepted() {
        // 22-char base64url token
        validate_session_id_for_acceptance("abcdefghijklmnopqrstuv").unwrap();
        // longer base64url with underscore and hyphen
        validate_session_id_for_acceptance("abc-def_ghi-jkl_mno-pqr").unwrap();
    }

    #[test]
    fn empty_id_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn short_weak_id_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("s1")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
        assert_eq!(
            validate_session_id_for_acceptance("decision-demo-1")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn uppercase_uuid_rejected() {
        let id = uuid::Uuid::new_v4()
            .as_hyphenated()
            .to_string()
            .to_uppercase();
        assert_eq!(
            validate_session_id_for_acceptance(&id)
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn base64url_too_short_rejected() {
        assert_eq!(
            validate_session_id_for_acceptance("abcdefghij")
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn valid_uuid_v7_accepted() {
        // Construct a v7 UUID by patching the version nibble of a v4 UUID
        let v4 = uuid::Uuid::new_v4();
        let mut bytes = *v4.as_bytes();
        // Set version nibble (bits 48-51) to 0b0111 (v7)
        bytes[6] = (bytes[6] & 0x0F) | 0x70;
        // Keep variant bits valid (RFC 4122: 0b10xx)
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v7_id = uuid::Uuid::from_bytes(bytes).as_hyphenated().to_string();
        assert!(validate_session_id_for_acceptance(&v7_id).is_ok());
    }

    #[test]
    fn uuid_v1_rejected() {
        // Construct a v1 UUID by patching the version nibble of a v4 UUID
        let v4 = uuid::Uuid::new_v4();
        let mut bytes = *v4.as_bytes();
        // Set version nibble (bits 48-51) to 0b0001 (v1)
        bytes[6] = (bytes[6] & 0x0F) | 0x10;
        // Keep variant bits valid (RFC 4122: 0b10xx)
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        let v1_id = uuid::Uuid::from_bytes(bytes).as_hyphenated().to_string();
        assert_eq!(
            validate_session_id_for_acceptance(&v1_id)
                .unwrap_err()
                .to_string(),
            "InvalidSessionId"
        );
    }

    #[test]
    fn too_many_participants_rejected() {
        let participants: Vec<String> = (0..1001).map(|i| format!("agent://p{i}")).collect();
        let bytes = encode_payload(5000, participants);
        let payload = parse_session_start_payload(&bytes).unwrap();
        assert_eq!(
            validate_canonical_session_start_payload(&payload)
                .unwrap_err()
                .to_string(),
            "InvalidPayload"
        );
    }

    #[test]
    fn max_participants_accepted() {
        let participants: Vec<String> = (0..1000).map(|i| format!("agent://p{i}")).collect();
        let bytes = encode_payload(5000, participants);
        let payload = parse_session_start_payload(&bytes).unwrap();
        validate_canonical_session_start_payload(&payload).unwrap();
    }
}
