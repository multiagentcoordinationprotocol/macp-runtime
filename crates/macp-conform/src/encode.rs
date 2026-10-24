//! JSON fixture payloads to protobuf wire bytes, and back for assertions.
//!
//! Payloads are named by their fully-qualified protobuf message name
//! (`payload_type`), and within a mode the `message_type` picks the message.
//! Fields a fixture omits take their protobuf default. An unknown
//! `payload_type` or `message_type` is an error, never a silent empty payload.

use macp_pb::pb::{CommitmentPayload, SessionStartPayload};
use macp_pb::{decision_pb, handoff_pb, multi_round_pb, proposal_pb, quorum_pb, task_pb};
use prost::Message;
use serde_json::{json, Value};

fn s(p: &Value, key: &str) -> String {
    p[key].as_str().unwrap_or_default().to_string()
}

/// Encode a fixture message's JSON payload.
pub fn encode_payload(
    payload_type: &str,
    message_type: &str,
    p: &Value,
) -> Result<Vec<u8>, String> {
    let bytes = match payload_type {
        "macp.v1.SessionStartPayload" => SessionStartPayload {
            intent: s(p, "intent"),
            participants: p["participants"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            mode_version: s(p, "mode_version"),
            configuration_version: s(p, "configuration_version"),
            policy_version: s(p, "policy_version"),
            ttl_ms: p["ttl_ms"].as_i64().unwrap_or_default(),
            context_id: String::new(),
            extensions: std::collections::HashMap::new(),
            roots: vec![],
            max_suspend_ms: 0,
        }
        .encode_to_vec(),
        "macp.v1.CommitmentPayload" => CommitmentPayload {
            commitment_id: s(p, "commitment_id"),
            action: s(p, "action"),
            authority_scope: s(p, "authority_scope"),
            reason: s(p, "reason"),
            mode_version: s(p, "mode_version"),
            policy_version: s(p, "policy_version"),
            configuration_version: s(p, "configuration_version"),
            outcome_positive: p["outcome_positive"].as_bool().unwrap_or(true),
            supersedes: None,
        }
        .encode_to_vec(),
        t if t.starts_with("macp.modes.decision.v1.") => decision(message_type, p)?,
        t if t.starts_with("macp.modes.proposal.v1.") => proposal(message_type, p)?,
        t if t.starts_with("macp.modes.task.v1.") => task(message_type, p)?,
        t if t.starts_with("macp.modes.handoff.v1.") => handoff(message_type, p)?,
        t if t.starts_with("macp.modes.quorum.v1.") => quorum(message_type, p)?,
        t if t.starts_with("macp.modes.multi_round.v1.") => multi_round(message_type, p)?,
        other => return Err(format!("unknown payload_type {other}")),
    };
    Ok(bytes)
}
fn decision(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "Proposal" => decision_pb::ProposalPayload {
            proposal_id: s(p, "proposal_id"),
            option: s(p, "option"),
            rationale: s(p, "rationale"),
            supporting_data: vec![],
        }
        .encode_to_vec(),
        "Evaluation" => decision_pb::EvaluationPayload {
            proposal_id: s(p, "proposal_id"),
            recommendation: s(p, "recommendation"),
            confidence: p["confidence"].as_f64().unwrap_or_default(),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "Objection" => decision_pb::ObjectionPayload {
            proposal_id: s(p, "proposal_id"),
            reason: s(p, "reason"),
            severity: s(p, "severity"),
        }
        .encode_to_vec(),
        "Vote" => decision_pb::VotePayload {
            proposal_id: s(p, "proposal_id"),
            vote: s(p, "vote"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled decision message {other}")),
    })
}

fn proposal(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "Proposal" => proposal_pb::ProposalPayload {
            proposal_id: s(p, "proposal_id"),
            title: s(p, "title"),
            summary: s(p, "summary"),
            details: vec![],
            tags: vec![],
        }
        .encode_to_vec(),
        "CounterProposal" => proposal_pb::CounterProposalPayload {
            proposal_id: s(p, "proposal_id"),
            supersedes_proposal_id: p["supersedes_proposal_id"]
                .as_str()
                .unwrap_or_default()
                .into(),
            title: s(p, "title"),
            summary: s(p, "summary"),
            details: vec![],
        }
        .encode_to_vec(),
        "Accept" => proposal_pb::AcceptPayload {
            proposal_id: s(p, "proposal_id"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "Reject" => proposal_pb::RejectPayload {
            proposal_id: s(p, "proposal_id"),
            reason: s(p, "reason"),
            terminal: p["terminal"].as_bool().unwrap_or(false),
        }
        .encode_to_vec(),
        "Withdraw" => proposal_pb::WithdrawPayload {
            proposal_id: s(p, "proposal_id"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled proposal message {other}")),
    })
}

fn task(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "TaskRequest" => task_pb::TaskRequestPayload {
            task_id: s(p, "task_id"),
            title: s(p, "title"),
            instructions: s(p, "instructions"),
            requested_assignee: s(p, "requested_assignee"),
            input: vec![],
            deadline_unix_ms: p["deadline_unix_ms"].as_i64().unwrap_or(0),
        }
        .encode_to_vec(),
        "TaskAccept" => task_pb::TaskAcceptPayload {
            task_id: s(p, "task_id"),
            assignee: s(p, "assignee"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "TaskReject" => task_pb::TaskRejectPayload {
            task_id: s(p, "task_id"),
            assignee: s(p, "assignee"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "TaskComplete" => task_pb::TaskCompletePayload {
            task_id: s(p, "task_id"),
            assignee: s(p, "assignee"),
            output: vec![],
            summary: s(p, "summary"),
        }
        .encode_to_vec(),
        "TaskFail" => task_pb::TaskFailPayload {
            task_id: s(p, "task_id"),
            assignee: s(p, "assignee"),
            error_code: s(p, "error_code"),
            reason: s(p, "reason"),
            retryable: p["retryable"].as_bool().unwrap_or(false),
        }
        .encode_to_vec(),
        "TaskUpdate" => task_pb::TaskUpdatePayload {
            task_id: s(p, "task_id"),
            status: s(p, "status"),
            progress: p["progress"].as_f64().unwrap_or_default(),
            message: s(p, "message"),
            partial_output: vec![],
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled task message {other}")),
    })
}

fn handoff(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "HandoffOffer" => handoff_pb::HandoffOfferPayload {
            handoff_id: s(p, "handoff_id"),
            target_participant: s(p, "target_participant"),
            scope: s(p, "scope"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "HandoffAccept" => handoff_pb::HandoffAcceptPayload {
            handoff_id: s(p, "handoff_id"),
            accepted_by: s(p, "accepted_by"),
            reason: s(p, "reason"),
            implicit: p["implicit"].as_bool().unwrap_or(false),
        }
        .encode_to_vec(),
        "HandoffDecline" => handoff_pb::HandoffDeclinePayload {
            handoff_id: s(p, "handoff_id"),
            declined_by: s(p, "declined_by"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "HandoffContext" => handoff_pb::HandoffContextPayload {
            handoff_id: s(p, "handoff_id"),
            content_type: s(p, "content_type"),
            context: p["context"]
                .as_str()
                .map(|s| s.as_bytes().to_vec())
                .unwrap_or_default(),
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled handoff message {other}")),
    })
}

fn quorum(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "ApprovalRequest" => quorum_pb::ApprovalRequestPayload {
            request_id: s(p, "request_id"),
            action: s(p, "action"),
            summary: s(p, "summary"),
            details: vec![],
            required_approvals: p["required_approvals"].as_u64().unwrap_or(0) as u32,
        }
        .encode_to_vec(),
        "Approve" => quorum_pb::ApprovePayload {
            request_id: s(p, "request_id"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "Reject" => quorum_pb::RejectPayload {
            request_id: s(p, "request_id"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        "Abstain" => quorum_pb::AbstainPayload {
            request_id: s(p, "request_id"),
            reason: s(p, "reason"),
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled quorum message {other}")),
    })
}

fn multi_round(message_type: &str, p: &Value) -> Result<Vec<u8>, String> {
    Ok(match message_type {
        "Contribute" => multi_round_pb::ContributePayload {
            value: s(p, "value"),
        }
        .encode_to_vec(),
        other => return Err(format!("unhandled multi_round message {other}")),
    })
}

/// A `CommitmentPayload`'s wire bytes as the JSON shape fixtures assert on.
pub fn commitment_to_json(raw: &[u8]) -> Option<Value> {
    CommitmentPayload::decode(raw).ok().map(|c| {
        json!({
            "commitment_id": c.commitment_id,
            "action": c.action,
            "authority_scope": c.authority_scope,
            "reason": c.reason,
            "mode_version": c.mode_version,
            "policy_version": c.policy_version,
            "configuration_version": c.configuration_version,
            "outcome_positive": c.outcome_positive,
        })
    })
}

/// Decode an accepted-history payload for `expected_history`'s subset match.
/// Covers the shared terminal and every Handoff message (the only mode whose
/// fixtures assert history payloads today); anything else is an error rather
/// than a vacuous match.
pub fn history_payload_to_json(message_type: &str, raw: &[u8]) -> Result<Value, String> {
    use handoff_pb as h;
    let bad = |e: prost::DecodeError| format!("{message_type} payload does not decode: {e}");
    Ok(match message_type {
        "Commitment" => commitment_to_json(raw).ok_or("Commitment payload does not decode")?,
        "HandoffOffer" => {
            let p = h::HandoffOfferPayload::decode(raw).map_err(bad)?;
            json!({"handoff_id": p.handoff_id, "target_participant": p.target_participant,
                   "scope": p.scope, "reason": p.reason})
        }
        "HandoffAccept" => {
            let p = h::HandoffAcceptPayload::decode(raw).map_err(bad)?;
            json!({"handoff_id": p.handoff_id, "accepted_by": p.accepted_by,
                   "reason": p.reason, "implicit": p.implicit})
        }
        "HandoffDecline" => {
            let p = h::HandoffDeclinePayload::decode(raw).map_err(bad)?;
            json!({"handoff_id": p.handoff_id, "declined_by": p.declined_by, "reason": p.reason})
        }
        "HandoffContext" => {
            let p = h::HandoffContextPayload::decode(raw).map_err(bad)?;
            json!({"handoff_id": p.handoff_id, "content_type": p.content_type,
                   "context": String::from_utf8_lossy(&p.context)})
        }
        other => {
            return Err(format!(
                "expected_history payload decoding not implemented for {other}"
            ))
        }
    })
}

/// Subset match: every key in `expected` is in `actual` with a matching
/// value (recursively); arrays match element-wise on `expected`'s prefix.
/// Returns the first difference.
pub fn json_contains(actual: &Value, expected: &Value) -> Result<(), String> {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            for (key, ev) in e {
                let av = a
                    .get(key)
                    .ok_or_else(|| format!("missing key '{key}' in {actual}"))?;
                json_contains(av, ev).map_err(|m| format!("{key}: {m}"))?;
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(e)) => {
            if a.len() < e.len() {
                return Err(format!(
                    "array shorter than expected: {actual} vs {expected}"
                ));
            }
            for (i, ev) in e.iter().enumerate() {
                json_contains(&a[i], ev).map_err(|m| format!("[{i}]: {m}"))?;
            }
            Ok(())
        }
        _ if actual == expected => Ok(()),
        _ => Err(format!("expected {expected}, got {actual}")),
    }
}
