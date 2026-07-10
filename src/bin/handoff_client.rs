#[path = "support/common.rs"]
mod common;

use common::{
    canonical_commitment_payload, canonical_start_payload, envelope, get_session_as, print_ack,
    send_as,
};
use macp_runtime::handoff_pb::{HandoffAcceptPayload, HandoffContextPayload, HandoffOfferPayload};
use prost::Message;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = common::connect_client().await?;
    let session_id = common::new_session_id();

    println!("=== Handoff Mode Demo ===\n");

    let ack = send_as(
        &mut client,
        "owner",
        envelope(
            "macp.mode.handoff.v1",
            "SessionStart",
            "m0",
            &session_id,
            "owner",
            canonical_start_payload("escalate support ticket", &["owner", "target"], 60_000),
        ),
    )
    .await?;
    print_ack("session_start", &ack);

    let offer = HandoffOfferPayload {
        handoff_id: "h1".into(),
        target_participant: "target".into(),
        scope: "customer-support".into(),
        reason: "specialist attention required".into(),
    };
    let ack = send_as(
        &mut client,
        "owner",
        envelope(
            "macp.mode.handoff.v1",
            "HandoffOffer",
            "m1",
            &session_id,
            "owner",
            offer.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("handoff_offer", &ack);

    let context = HandoffContextPayload {
        handoff_id: "h1".into(),
        content_type: "text/plain".into(),
        context: b"customer issue: invoice mismatch".to_vec(),
    };
    let ack = send_as(
        &mut client,
        "owner",
        envelope(
            "macp.mode.handoff.v1",
            "HandoffContext",
            "m2",
            &session_id,
            "owner",
            context.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("handoff_context", &ack);

    let accept = HandoffAcceptPayload {
        handoff_id: "h1".into(),
        accepted_by: "target".into(),
        reason: "taking ownership".into(),
        // Explicit client accept — implicit=true is reserved for the runtime's
        // synthetic timeout accept (RFC-MACP-0010 §5.1); clients MUST send false.
        implicit: false,
    };
    let ack = send_as(
        &mut client,
        "target",
        envelope(
            "macp.mode.handoff.v1",
            "HandoffAccept",
            "m3",
            &session_id,
            "target",
            accept.encode_to_vec(),
        ),
    )
    .await?;
    print_ack("handoff_accept", &ack);

    let ack = send_as(
        &mut client,
        "owner",
        envelope(
            "macp.mode.handoff.v1",
            "Commitment",
            "m4",
            &session_id,
            "owner",
            canonical_commitment_payload(
                "c1",
                "handoff.accepted",
                "workflow",
                "target accepted handoff h1",
            ),
        ),
    )
    .await?;
    print_ack("commitment", &ack);

    let session = get_session_as(&mut client, "target", &session_id).await?;
    let meta = session.metadata.expect("metadata");
    println!("[get_session] state={} mode={}", meta.state, meta.mode);

    Ok(())
}
