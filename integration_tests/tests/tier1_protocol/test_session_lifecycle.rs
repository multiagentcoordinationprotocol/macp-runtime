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

    // Poll until the TTL lapses and a send is rejected, instead of a single
    // fixed sleep — a loaded machine can delay either the test or the
    // runtime's expiry sweep past any one hardcoded pause.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
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
        if !ack.ok {
            break; // rejected: session expired as required
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session with a 100ms TTL still accepted messages after 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
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

/// Phase 8's regression: the initial sync's observable contract over a
/// registry large enough to have exercised the old whole-registry deep clone,
/// on a runtime of this test's own.
///
/// Deliberately **not** the shared server from `tests/common`. That one
/// accumulates sessions from every other test in this binary, so the only
/// assertion possible against it is "each of mine appears once" — it cannot
/// assert that the sync emits *nothing else*, which is half of "exactly N
/// Created events, once each". A private runtime starts with an empty registry
/// and makes the whole set assertable. (This replaces an earlier 12-session
/// variant that ran against the shared server for exactly that reason.)
#[tokio::test]
async fn watch_sessions_initial_sync_emits_every_session_once_on_a_private_runtime() {
    use macp_integration_tests::server_manager::ServerManager;
    use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
    use macp_runtime::pb::WatchSessionsRequest;

    const SESSIONS: usize = 60;

    let binary =
        std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into());
    // 60 starts from one sender blows through the 60/minute default, which
    // would surface as a bogus lifecycle failure. Pin it above the fixture.
    let manager = ServerManager::start_with_env(
        &binary,
        &[("MACP_SESSION_START_LIMIT_PER_MINUTE", "1000")],
    )
    .await
    .expect("private runtime must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect to the private runtime");

    let agent = "agent://watch-sync-many";
    let partner = "agent://partner";
    let mut expected: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..SESSIONS {
        let sid = new_session_id();
        let ack = send_as(
            &mut client,
            agent,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                &sid,
                agent,
                session_start_payload(&format!("watch sync {i}"), &[agent, partner], 60_000),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "SessionStart {i} failed: {:?}", ack.error);
        expected.insert(sid);
    }

    let mut request = tonic::Request::new(WatchSessionsRequest {});
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {agent}").parse().expect("valid header"),
    );
    let mut stream = client.watch_sessions(request).await.unwrap().into_inner();

    // Every Created event, counted — including any for a session this test did
    // not create, which on a private runtime would be a bug rather than noise.
    let mut created_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while created_counts.len() < SESSIONS {
        let next = tokio::time::timeout_at(deadline, stream.message()).await;
        let Ok(Ok(Some(resp))) = next else {
            panic!(
                "initial sync stopped after {} of {SESSIONS} sessions",
                created_counts.len()
            )
        };
        let event = resp.event.expect("lifecycle event present");
        // EventType::Created == 1 in the proto enum.
        if event.event_type == 1 {
            let session = event.session.expect("Created carries metadata");
            *created_counts.entry(session.session_id).or_insert(0) += 1;
        }
    }

    // Drain briefly: a duplicate or a stray extra would arrive right after.
    while let Ok(Ok(Some(resp))) =
        tokio::time::timeout(std::time::Duration::from_millis(500), stream.message()).await
    {
        if let Some(event) = resp.event {
            if event.event_type == 1 {
                if let Some(session) = event.session {
                    *created_counts.entry(session.session_id).or_insert(0) += 1;
                }
            }
        }
    }

    assert_eq!(
        created_counts.len(),
        SESSIONS,
        "the sync emitted Created for a session this runtime never created"
    );
    for sid in &expected {
        assert_eq!(
            created_counts.get(sid).copied().unwrap_or(0),
            1,
            "session {sid} must appear exactly once in the initial sync"
        );
    }
}
