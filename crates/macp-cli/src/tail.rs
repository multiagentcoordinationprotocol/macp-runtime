//! `macp session tail`: print a session's accepted envelopes as they arrive.
//!
//! A passive `StreamSession` subscription (RFC-MACP-0006 §3.2): the runtime
//! replays accepted history after `after_sequence`, then switches to live
//! traffic. Each envelope is printed with its accepted ordinal, 1-based, so
//! `--after N` resumes after the N-th; `WhatIfReplay`'s `history_index` is
//! that ordinal minus one.
//!
//! The stream has no end-of-history marker and never closes on its own. When
//! it has been quiet for a moment, `tail` asks `GetSession` for the state, and
//! stops once the session has ended (or at once, with `follow` off).

use std::io::Write;
use std::time::Duration;

use base64::Engine as _;
use macp_conform::client::{self, MacpRuntimeServiceClient};
use macp_pb::pb::{
    stream_session_response, Envelope, GetSessionRequest, SessionState, StreamSessionRequest,
};
use tonic::metadata::MetadataValue;
use tonic::transport::Channel;

/// What to tail, and how to print it.
#[derive(Debug, Clone)]
pub struct TailOptions {
    pub session_id: String,
    /// Skip the first `after` accepted envelopes.
    pub after: u64,
    /// Keep reading live traffic until the session ends.
    pub follow: bool,
    /// One JSON object per line instead of a table.
    pub json: bool,
    /// How long a quiet stream waits before checking the session's state.
    pub idle: Duration,
}

impl TailOptions {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            after: 0,
            follow: true,
            json: false,
            idle: Duration::from_millis(500),
        }
    }
}

/// How a tail ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TailEnd {
    /// The session is no longer open: `Resolved`, `Expired` or `Cancelled`.
    Ended { state: String, printed: u64 },
    /// History was read and `follow` was off; the session may still be open.
    Caught { state: String, printed: u64 },
}

/// Connect to `target` and tail with bearer `token`.
pub async fn run(
    target: &str,
    token: &str,
    opts: &TailOptions,
    out: &mut impl Write,
) -> Result<TailEnd, String> {
    let mut client = client::connect(target).await?;
    tail(&mut client, token, opts, out).await
}

/// Tail on an existing client. See the module docs.
pub async fn tail(
    client: &mut MacpRuntimeServiceClient<Channel>,
    token: &str,
    opts: &TailOptions,
    out: &mut impl Write,
) -> Result<TailEnd, String> {
    let bearer = MetadataValue::try_from(format!("Bearer {token}"))
        .map_err(|_| "the token is not a valid bearer value".to_string())?;
    // Fail on an unknown or forbidden session before opening the stream,
    // which reports either only inline.
    session_state(client, &bearer, &opts.session_id).await?;

    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(StreamSessionRequest {
        envelope: None,
        subscribe_session_id: opts.session_id.clone(),
        after_sequence: opts.after,
    })
    .await
    .map_err(|_| "stream request channel closed".to_string())?;
    let mut req = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(rx));
    req.metadata_mut().insert("authorization", bearer.clone());
    let mut stream = client
        .stream_session(req)
        .await
        .map_err(|s| format!("StreamSession: {}", s.message()))?
        .into_inner();

    let mut printed = 0u64;
    loop {
        match tokio::time::timeout(opts.idle, stream.message()).await {
            Ok(Ok(Some(resp))) => match resp.response {
                Some(stream_session_response::Response::Envelope(env)) => {
                    printed += 1;
                    let line = if opts.json {
                        json_line(opts.after + printed, &env)
                    } else {
                        text_line(opts.after + printed, &env)
                    };
                    writeln!(out, "{line}").map_err(|e| e.to_string())?;
                }
                Some(stream_session_response::Response::Error(e)) => {
                    return Err(format!("{}: {}", e.code, e.message));
                }
                None => {}
            },
            Ok(Ok(None)) => return Err("the runtime closed the stream".into()),
            Ok(Err(s)) => return Err(format!("StreamSession: {}", s.message())),
            Err(_quiet) => {
                let state = session_state(client, &bearer, &opts.session_id).await?;
                if !matches!(state.as_str(), "Open" | "Suspended") {
                    return Ok(TailEnd::Ended { state, printed });
                }
                if !opts.follow {
                    return Ok(TailEnd::Caught { state, printed });
                }
            }
        }
    }
}

async fn session_state(
    client: &mut MacpRuntimeServiceClient<Channel>,
    bearer: &MetadataValue<tonic::metadata::Ascii>,
    session_id: &str,
) -> Result<String, String> {
    let mut req = tonic::Request::new(GetSessionRequest {
        session_id: session_id.to_string(),
    });
    req.metadata_mut().insert("authorization", bearer.clone());
    req.set_timeout(Duration::from_secs(30));
    let meta = client
        .get_session(req)
        .await
        .map_err(|s| status_message(&s))?
        .into_inner()
        .metadata
        .unwrap_or_default();
    Ok(state_name(meta.state))
}

/// `CODE: message`, with the MACP error code when the status carries one.
pub fn status_message(s: &tonic::Status) -> String {
    match s
        .metadata()
        .get("macp-error-code")
        .and_then(|v| v.to_str().ok())
    {
        Some(code) if !s.message().starts_with(code) => format!("{code}: {}", s.message()),
        _ => s.message().to_string(),
    }
}

/// A `SessionState` as the runtime's own `GetSession` names it.
pub fn state_name(state: i32) -> String {
    match SessionState::try_from(state) {
        Ok(SessionState::Open) => "Open".into(),
        Ok(SessionState::Resolved) => "Resolved".into(),
        Ok(SessionState::Expired) => "Expired".into(),
        Ok(SessionState::Suspended) => "Suspended".into(),
        Ok(SessionState::Cancelled) => "Cancelled".into(),
        _ => format!("unknown({state})"),
    }
}

fn text_line(seq: u64, env: &Envelope) -> String {
    let mut line = format!(
        "#{seq:<4} {}  {:<16} {:<16} {}",
        rfc3339_ms(env.timestamp_unix_ms),
        env.sender,
        env.message_type,
        env.message_id
    );
    if let Some(p) = describe_payload(&env.mode, &env.message_type, &env.payload) {
        line.push_str("  ");
        line.push_str(&p);
    }
    line
}

fn json_line(seq: u64, env: &Envelope) -> String {
    serde_json::json!({
        "sequence": seq,
        "message_id": env.message_id,
        "message_type": env.message_type,
        "sender": env.sender,
        "mode": env.mode,
        "session_id": env.session_id,
        "timestamp_unix_ms": env.timestamp_unix_ms,
        "payload_base64": base64::engine::general_purpose::STANDARD.encode(&env.payload),
    })
    .to_string()
}

/// The payload decoded for display, when `mode` and `message_type` name a
/// message this release knows. Debug form: for reading, not parsing.
pub fn describe_payload(mode: &str, message_type: &str, raw: &[u8]) -> Option<String> {
    use macp_pb::{decision_pb as d, handoff_pb as h, multi_round_pb as m, pb};
    use macp_pb::{proposal_pb as p, quorum_pb as q, task_pb as t};
    fn show<M: prost::Message + Default + std::fmt::Debug>(raw: &[u8]) -> Option<String> {
        M::decode(raw).ok().map(|m| format!("{m:?}"))
    }
    match message_type {
        "SessionStart" => return show::<pb::SessionStartPayload>(raw),
        "Commitment" => return show::<pb::CommitmentPayload>(raw),
        _ => {}
    }
    match (mode, message_type) {
        ("macp.mode.decision.v1", "Proposal") => show::<d::ProposalPayload>(raw),
        ("macp.mode.decision.v1", "Evaluation") => show::<d::EvaluationPayload>(raw),
        ("macp.mode.decision.v1", "Objection") => show::<d::ObjectionPayload>(raw),
        ("macp.mode.decision.v1", "Vote") => show::<d::VotePayload>(raw),
        ("macp.mode.proposal.v1", "Proposal") => show::<p::ProposalPayload>(raw),
        ("macp.mode.proposal.v1", "CounterProposal") => show::<p::CounterProposalPayload>(raw),
        ("macp.mode.proposal.v1", "Accept") => show::<p::AcceptPayload>(raw),
        ("macp.mode.proposal.v1", "Reject") => show::<p::RejectPayload>(raw),
        ("macp.mode.proposal.v1", "Withdraw") => show::<p::WithdrawPayload>(raw),
        ("macp.mode.task.v1", "TaskRequest") => show::<t::TaskRequestPayload>(raw),
        ("macp.mode.task.v1", "TaskAccept") => show::<t::TaskAcceptPayload>(raw),
        ("macp.mode.task.v1", "TaskReject") => show::<t::TaskRejectPayload>(raw),
        ("macp.mode.task.v1", "TaskUpdate") => show::<t::TaskUpdatePayload>(raw),
        ("macp.mode.task.v1", "TaskComplete") => show::<t::TaskCompletePayload>(raw),
        ("macp.mode.task.v1", "TaskFail") => show::<t::TaskFailPayload>(raw),
        ("macp.mode.handoff.v1", "HandoffOffer") => show::<h::HandoffOfferPayload>(raw),
        ("macp.mode.handoff.v1", "HandoffContext") => show::<h::HandoffContextPayload>(raw),
        ("macp.mode.handoff.v1", "HandoffAccept") => show::<h::HandoffAcceptPayload>(raw),
        ("macp.mode.handoff.v1", "HandoffDecline") => show::<h::HandoffDeclinePayload>(raw),
        ("macp.mode.quorum.v1", "ApprovalRequest") => show::<q::ApprovalRequestPayload>(raw),
        ("macp.mode.quorum.v1", "Approve") => show::<q::ApprovePayload>(raw),
        ("macp.mode.quorum.v1", "Reject") => show::<q::RejectPayload>(raw),
        ("macp.mode.quorum.v1", "Abstain") => show::<q::AbstainPayload>(raw),
        ("ext.multi_round.v1", "Contribute") => show::<m::ContributePayload>(raw),
        _ => None,
    }
}

/// `unix_ms` as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn rfc3339_ms(unix_ms: i64) -> String {
    let (days, ms) = (
        unix_ms.div_euclid(86_400_000),
        unix_ms.rem_euclid(86_400_000),
    );
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message as _;

    #[test]
    fn rfc3339_ms_formats_utc() {
        assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_ms(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(rfc3339_ms(1_792_886_400_000), "2026-10-25T00:00:00.000Z");
        assert_eq!(rfc3339_ms(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn describe_payload_decodes_known_messages_only() {
        let vote = macp_pb::decision_pb::VotePayload {
            proposal_id: "p1".into(),
            vote: "APPROVE".into(),
            ..Default::default()
        }
        .encode_to_vec();
        let shown = describe_payload("macp.mode.decision.v1", "Vote", &vote).unwrap();
        assert!(shown.contains("APPROVE"), "{shown}");
        assert_eq!(describe_payload("macp.mode.task.v1", "Vote", &vote), None);
        assert_eq!(describe_payload("ext.custom.v1", "Ping", &[]), None);
    }

    #[test]
    fn lines_carry_the_accepted_ordinal() {
        let env = Envelope {
            message_id: "m1".into(),
            message_type: "Signal".into(),
            sender: "alice".into(),
            timestamp_unix_ms: 0,
            payload: vec![1, 2],
            ..Default::default()
        };
        assert!(text_line(3, &env).starts_with("#3    1970-01-01T00:00:00.000Z  alice"));
        let v: serde_json::Value = serde_json::from_str(&json_line(3, &env)).unwrap();
        assert_eq!(v["sequence"], 3);
        assert_eq!(v["payload_base64"], "AQI=");
    }
}
