use crate::common;
use macp_integration_tests::helpers::*;
use macp_integration_tests::macp_tools::{
    self, commit::*, decision::*, query::*, session_start::*,
};
use rig::tool::ToolSet;

/// Two Rig tool-equipped agents drive a full decision lifecycle.
/// The tools are invoked directly via ToolSet::call (simulating what an LLM agent would do).
#[tokio::test]
async fn rig_tools_drive_decision_lifecycle() {
    let ep = common::endpoint().await;
    let coord_client = macp_tools::shared_client(ep).await;
    let voter_client = macp_tools::shared_client(ep).await;
    let sid = new_session_id();

    // Build coordinator's toolset
    let mut coord_tools = ToolSet::default();
    coord_tools.add_tool(StartSessionTool {
        client: coord_client.clone(),
        agent_id: "agent://coord".into(),
    });
    coord_tools.add_tool(ProposeTool {
        client: coord_client.clone(),
        agent_id: "agent://coord".into(),
    });
    coord_tools.add_tool(CommitTool {
        client: coord_client.clone(),
        agent_id: "agent://coord".into(),
        mode: MODE_DECISION.into(),
    });
    coord_tools.add_tool(GetSessionTool {
        client: coord_client.clone(),
        agent_id: "agent://coord".into(),
    });

    // Build voter's toolset
    let mut voter_tools = ToolSet::default();
    voter_tools.add_tool(VoteTool {
        client: voter_client.clone(),
        agent_id: "agent://voter".into(),
    });

    // Verify tool definitions are valid
    let defs = coord_tools.get_tool_definitions().await.unwrap();
    assert_eq!(defs.len(), 4);
    assert!(defs.iter().any(|d| d.name == "macp_start_session"));
    assert!(defs.iter().any(|d| d.name == "macp_propose"));
    assert!(defs.iter().any(|d| d.name == "macp_commit"));
    assert!(defs.iter().any(|d| d.name == "macp_get_session"));

    // Step 1: Coordinator starts session (simulating LLM tool call)
    let result = coord_tools
        .call(
            "macp_start_session",
            serde_json::json!({
                "mode": MODE_DECISION,
                "session_id": &sid,
                "intent": "choose deployment strategy",
                "participants": ["agent://coord", "agent://voter"],
                "ttl_ms": 30000
            })
            .to_string(),
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);

    // Step 2: Coordinator proposes
    let result = coord_tools
        .call(
            "macp_propose",
            serde_json::json!({
                "session_id": &sid,
                "proposal_id": "p1",
                "option": "deploy-v2",
                "rationale": "better performance and reliability"
            })
            .to_string(),
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);

    // Step 3: Voter votes
    let result = voter_tools
        .call(
            "macp_vote",
            serde_json::json!({
                "session_id": &sid,
                "proposal_id": "p1",
                "vote": "APPROVE",
                "reason": "looks good to me"
            })
            .to_string(),
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);

    // Step 4: Coordinator commits
    let result = coord_tools
        .call(
            "macp_commit",
            serde_json::json!({
                "session_id": &sid,
                "action": "deploy-v2",
                "authority_scope": "team",
                "reason": "voter approved"
            })
            .to_string(),
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["session_state"], 2); // RESOLVED

    // Step 5: Coordinator checks session state
    let result = coord_tools
        .call(
            "macp_get_session",
            serde_json::json!({
                "session_id": &sid
            })
            .to_string(),
        )
        .await
        .unwrap();
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["state"], 2); // RESOLVED
}
