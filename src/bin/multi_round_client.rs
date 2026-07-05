#[path = "support/common.rs"]
mod common;

use common::{
    canonical_commitment_payload, canonical_start_payload, envelope, get_session_as, print_ack,
    send_as,
};
use macp_runtime::multi_round_pb::ContributePayload;
use prost::Message;

fn contribute(value: &str) -> Vec<u8> {
    ContributePayload {
        value: value.into(),
    }
    .encode_to_vec()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = common::connect_client().await?;
    let session_id = common::new_session_id();

    println!("=== Multi-Round Convergence Demo ===\n");

    // Standards-track SessionStart with required fields
    let ack = send_as(
        &mut client,
        "coordinator",
        envelope(
            "ext.multi_round.v1",
            "SessionStart",
            "m0",
            &session_id,
            "coordinator",
            canonical_start_payload("convergence test", &["alice", "bob"], 60_000),
        ),
    )
    .await?;
    print_ack("session_start", &ack);

    // Alice contributes
    let ack = send_as(
        &mut client,
        "alice",
        envelope(
            "ext.multi_round.v1",
            "Contribute",
            "m1",
            &session_id,
            "alice",
            contribute("option_a"),
        ),
    )
    .await?;
    print_ack("alice_contributes", &ack);

    // Bob contributes differently
    let ack = send_as(
        &mut client,
        "bob",
        envelope(
            "ext.multi_round.v1",
            "Contribute",
            "m2",
            &session_id,
            "bob",
            contribute("option_b"),
        ),
    )
    .await?;
    print_ack("bob_contributes_b", &ack);

    let session = get_session_as(&mut client, "alice", &session_id).await?;
    let meta = session.metadata.expect("metadata");
    println!("[get_session] state={} mode={}", meta.state, meta.mode);

    // Bob revises to match Alice → convergence
    let ack = send_as(
        &mut client,
        "bob",
        envelope(
            "ext.multi_round.v1",
            "Contribute",
            "m3",
            &session_id,
            "bob",
            contribute("option_a"),
        ),
    )
    .await?;
    print_ack("bob_revises", &ack);

    let session = get_session_as(&mut client, "alice", &session_id).await?;
    let meta = session.metadata.expect("metadata");
    println!(
        "[get_session] state={} mode={} (converged, awaiting commitment)",
        meta.state, meta.mode
    );

    // Coordinator commits after convergence
    let ack = send_as(
        &mut client,
        "coordinator",
        envelope(
            "ext.multi_round.v1",
            "Commitment",
            "m4",
            &session_id,
            "coordinator",
            canonical_commitment_payload(
                "c1",
                "multi_round.converged",
                "convergence",
                "all participants agreed",
            ),
        ),
    )
    .await?;
    print_ack("commitment", &ack);

    let session = get_session_as(&mut client, "alice", &session_id).await?;
    let meta = session.metadata.expect("metadata");
    println!("[get_session] state={} mode={}", meta.state, meta.mode);

    Ok(())
}
