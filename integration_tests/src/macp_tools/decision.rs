use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

use super::SharedClient;
use crate::helpers;

// ── ProposeTool ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ProposeTool {
    pub client: SharedClient,
    pub agent_id: String,
}

#[derive(Deserialize)]
pub struct ProposeArgs {
    pub session_id: String,
    pub proposal_id: String,
    pub option: String,
    pub rationale: String,
}

#[derive(Serialize)]
pub struct ToolResult {
    pub ok: bool,
    pub session_state: i32,
}

#[derive(Debug, thiserror::Error)]
#[error("MACP tool error: {0}")]
pub struct MacpToolError(pub String);

impl Tool for ProposeTool {
    const NAME: &'static str = "macp_propose";
    type Error = MacpToolError;
    type Args = ProposeArgs;
    type Output = ToolResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "macp_propose",
            "description": "Submit a proposal in a MACP Decision session",
            "parameters": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "proposal_id": { "type": "string" },
                    "option": { "type": "string" },
                    "rationale": { "type": "string" }
                },
                "required": ["session_id", "proposal_id", "option", "rationale"]
            }
        }))
        .unwrap()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let payload = helpers::proposal_payload(&args.proposal_id, &args.option, &args.rationale);
        let env = helpers::envelope(
            helpers::MODE_DECISION,
            "Proposal",
            &helpers::new_message_id(),
            &args.session_id,
            &self.agent_id,
            payload,
        );
        let mut client = self.client.lock().await;
        let ack = helpers::send_as(&mut client, &self.agent_id, env)
            .await
            .map_err(|e| MacpToolError(e.to_string()))?;
        Ok(ToolResult {
            ok: ack.ok,
            session_state: ack.session_state,
        })
    }
}

// ── EvaluateTool ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct EvaluateTool {
    pub client: SharedClient,
    pub agent_id: String,
}

#[derive(Deserialize)]
pub struct EvaluateArgs {
    pub session_id: String,
    pub proposal_id: String,
    pub recommendation: String,
    pub confidence: f64,
    pub reason: String,
}

impl Tool for EvaluateTool {
    const NAME: &'static str = "macp_evaluate";
    type Error = MacpToolError;
    type Args = EvaluateArgs;
    type Output = ToolResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "macp_evaluate",
            "description": "Submit an evaluation of a proposal in a MACP Decision session",
            "parameters": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "proposal_id": { "type": "string" },
                    "recommendation": { "type": "string", "enum": ["APPROVE", "REVIEW", "BLOCK", "REJECT"], "description": "UPPER_CASE, exactly (RFC-MACP-0007 §4)" },
                    "confidence": { "type": "number" },
                    "reason": { "type": "string" }
                },
                "required": ["session_id", "proposal_id", "recommendation", "confidence", "reason"]
            }
        }))
        .unwrap()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let payload = helpers::evaluation_payload(
            &args.proposal_id,
            &args.recommendation,
            args.confidence,
            &args.reason,
        );
        let env = helpers::envelope(
            helpers::MODE_DECISION,
            "Evaluation",
            &helpers::new_message_id(),
            &args.session_id,
            &self.agent_id,
            payload,
        );
        let mut client = self.client.lock().await;
        let ack = helpers::send_as(&mut client, &self.agent_id, env)
            .await
            .map_err(|e| MacpToolError(e.to_string()))?;
        Ok(ToolResult {
            ok: ack.ok,
            session_state: ack.session_state,
        })
    }
}

// ── VoteTool ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct VoteTool {
    pub client: SharedClient,
    pub agent_id: String,
}

#[derive(Deserialize)]
pub struct VoteArgs {
    pub session_id: String,
    pub proposal_id: String,
    pub vote: String,
    pub reason: String,
}

impl Tool for VoteTool {
    const NAME: &'static str = "macp_vote";
    type Error = MacpToolError;
    type Args = VoteArgs;
    type Output = ToolResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(serde_json::json!({
            "name": "macp_vote",
            "description": "Cast a vote on a proposal in a MACP Decision session",
            "parameters": {
                "type": "object",
                "properties": {
                    "session_id": { "type": "string" },
                    "proposal_id": { "type": "string" },
                    "vote": { "type": "string", "enum": ["APPROVE", "REJECT", "ABSTAIN"], "description": "UPPER_CASE, exactly (RFC-MACP-0007 §4)" },
                    "reason": { "type": "string" }
                },
                "required": ["session_id", "proposal_id", "vote", "reason"]
            }
        }))
        .unwrap()
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let payload = helpers::vote_payload(&args.proposal_id, &args.vote, &args.reason);
        let env = helpers::envelope(
            helpers::MODE_DECISION,
            "Vote",
            &helpers::new_message_id(),
            &args.session_id,
            &self.agent_id,
            payload,
        );
        let mut client = self.client.lock().await;
        let ack = helpers::send_as(&mut client, &self.agent_id, env)
            .await
            .map_err(|e| MacpToolError(e.to_string()))?;
        Ok(ToolResult {
            ok: ack.ok,
            session_state: ack.session_state,
        })
    }
}
