use crate::common;
use macp_integration_tests::helpers::*;

#[tokio::test]
async fn session_expires_after_ttl() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://ttl-test";
    let partner = "agent://partner";

    // Start session with very short TTL (100ms)
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent,
            session_start_payload("ttl test", &[agent, partner], 100),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Wait for TTL to expire
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // Try sending — should fail because session expired
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid,
            agent,
            proposal_payload("p1", "late", "expired"),
        ),
    )
    .await
    .unwrap();
    assert!(!ack.ok);
}

#[tokio::test]
async fn get_session_returns_open_state() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let agent = "agent://lifecycle-test";
    let partner = "agent://partner";

    // Start session
    send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            agent,
            session_start_payload("lifecycle test", &[agent, partner], 30_000),
        ),
    )
    .await
    .unwrap();

    // GetSession should show OPEN
    let resp = get_session_as(&mut client, agent, &sid).await.unwrap();
    let meta = resp.metadata.expect("metadata present");
    assert_eq!(meta.state, 1); // OPEN
    assert_eq!(meta.mode, MODE_DECISION);
    assert_eq!(meta.session_id, sid);
}

#[tokio::test]
async fn watch_sessions_emits_created_exactly_once_per_session() {
    use macp_runtime::pb::WatchSessionsRequest;

    let mut client = common::grpc_client().await;
    let agent = "agent://watch-once";
    let partner = "agent://partner";

    // One session created BEFORE subscribing (arrives via initial sync)...
    let sid_before = new_session_id();
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid_before,
            agent,
            session_start_payload("watch dedup before", &[agent, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    let mut request = tonic::Request::new(WatchSessionsRequest {});
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {agent}").parse().expect("valid header"),
    );
    let mut stream = client.watch_sessions(request).await.unwrap().into_inner();

    // ...and one created AFTER subscribing (arrives as a live event).
    let sid_after = new_session_id();
    let ack = send_as(
        &mut client,
        agent,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid_after,
            agent,
            session_start_payload("watch dedup after", &[agent, partner], 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    // Collect Created events until both sessions have been seen (bounded).
    let mut created_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let next = tokio::time::timeout_at(deadline, stream.message()).await;
        let Ok(Ok(Some(resp))) = next else { break };
        if let Some(event) = resp.event {
            // EventType::Created == 1 in the proto enum.
            if event.event_type == 1 {
                if let Some(session) = event.session {
                    *created_counts.entry(session.session_id).or_insert(0) += 1;
                }
            }
        }
        if created_counts.get(&sid_before).copied().unwrap_or(0) >= 1
            && created_counts.get(&sid_after).copied().unwrap_or(0) >= 1
        {
            // Drain briefly for any straggling duplicate before asserting.
            let grace =
                tokio::time::timeout(std::time::Duration::from_millis(300), stream.message()).await;
            if let Ok(Ok(Some(resp))) = grace {
                if let Some(event) = resp.event {
                    if event.event_type == 1 {
                        if let Some(session) = event.session {
                            *created_counts.entry(session.session_id).or_insert(0) += 1;
                        }
                    }
                }
            }
            break;
        }
    }

    assert_eq!(
        created_counts.get(&sid_before).copied().unwrap_or(0),
        1,
        "initial-sync session must appear exactly once"
    );
    assert_eq!(
        created_counts.get(&sid_after).copied().unwrap_or(0),
        1,
        "live-created session must appear exactly once"
    );
}
