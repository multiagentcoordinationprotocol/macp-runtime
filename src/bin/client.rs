#[path = "support/common.rs"]
mod common;

use common::{
    canonical_commitment_payload, canonical_start_payload, envelope, get_session_as, initialize,
    list_modes, print_ack, send_as,
};
use macp_runtime::decision_pb::{EvaluationPayload, ProposalPayload, VotePayload};
use prost::Message;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = common::connect_client().await?;
    let session_id = common::new_session_id();

    let init = initialize(&mut client).await?;
    println!(
        "Initialize: version={} runtime={}",
        init.selected_protocol_version,
        init.runtime_info
            .as_ref()
            .map(|info| info.name.as_str())
            .unwrap_or("?")
    );

    let modes = list_modes(&mut client).await?;
    println!(
        "ListModes: {:?}",
        modes
            .modes
            .iter()
            .map(|m| m.mode.as_str())
            .collect::<Vec<_>>()
    );

    let start = envelope(
        "macp.mode.decision.v1",
        "SessionStart",
        "m1",
        &session_id,
        "coordinator",
        canonical_start_payload(
            "select the deployment plan",
            &["coordinator", "alice", "bob"],
            60_000,
        ),
    );
    let ack = send_as(&mut client, "coordinator", start).await?;
    print_ack("session_start", &ack);

    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy v2.1 to production".into(),
        rationale: "integration tests and canary checks passed".into(),
        supporting_data: vec![],
    };
    let ack = send_as(
        &mut client,
        "coordinator",
        envelope(
            "macp.mode.decision.v1",
            "Proposal",
            "m2",
            &session_id,
            "coordinator",
            proposal.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("proposal", &ack);

    let evaluation = EvaluationPayload {
        proposal_id: "p1".into(),
        recommendation: "APPROVE".into(),
        confidence: 0.94,
        reason: "operational risk is low".into(),
    };
    let ack = send_as(
        &mut client,
        "alice",
        envelope(
            "macp.mode.decision.v1",
            "Evaluation",
            "m3",
            &session_id,
            "alice",
            evaluation.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("evaluation", &ack);

    let vote = VotePayload {
        proposal_id: "p1".into(),
        vote: "APPROVE".into(),
        reason: "approved for release".into(),
    };
    let ack = send_as(
        &mut client,
        "bob",
        envelope(
            "macp.mode.decision.v1",
            "Vote",
            "m4",
            &session_id,
            "bob",
            vote.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("vote", &ack);

    let ack = send_as(
        &mut client,
        "coordinator",
        envelope(
            "macp.mode.decision.v1",
            "Commitment",
            "m5",
            &session_id,
            "coordinator",
            canonical_commitment_payload(
                "c1",
                "deployment.approved",
                "release-management",
                "decision session reached commitment",
            ),
        ),
    )
    .await?;
    print_ack("commitment", &ack);

    let session = get_session_as(&mut client, "alice", &session_id).await?;
    let meta = session.metadata.expect("metadata");
    println!("GetSession: state={} mode={}", meta.state, meta.mode);

    Ok(())
}
