use crate::common;
use macp_integration_tests::helpers::*;

#[tokio::test]
async fn concurrent_sessions_across_modes() {
    let mut client = common::grpc_client().await;

    // Start 5 sessions in different modes simultaneously
    let modes = [
        MODE_DECISION,
        MODE_PROPOSAL,
        MODE_TASK,
        MODE_HANDOFF,
        MODE_QUORUM,
    ];

    let mut session_ids = Vec::new();
    for mode in &modes {
        let sid = new_session_id();
        let agent = "agent://concurrent-test";
        let partner = "agent://partner";

        let ack = send_as(
            &mut client,
            agent,
            envelope(
                mode,
                "SessionStart",
                &new_message_id(),
                &sid,
                agent,
                session_start_payload("concurrent", &[agent, partner], 30_000),
            ),
        )
        .await
        .unwrap();
        assert!(ack.ok, "Failed to start session for mode {mode}");
        session_ids.push((sid, *mode));
    }

    // Verify all sessions are open
    for (sid, mode) in &session_ids {
        let resp = get_session_as(&mut client, "agent://concurrent-test", sid)
            .await
            .unwrap();
        let meta = resp.metadata.expect("metadata present");
        assert_eq!(meta.state, 1, "Session for {mode} should be OPEN"); // OPEN
        assert_eq!(meta.mode, *mode);
    }
}

#[tokio::test]
async fn parallel_decision_sessions_are_independent() {
    let mut client = common::grpc_client().await;
    let coord = "agent://coord";
    let voter = "agent://voter";

    let sid1 = new_session_id();
    let sid2 = new_session_id();

    // Start two independent decision sessions
    for sid in [&sid1, &sid2] {
        send_as(
            &mut client,
            coord,
            envelope(
                MODE_DECISION,
                "SessionStart",
                &new_message_id(),
                sid,
                coord,
                session_start_payload("parallel", &[coord, voter], 30_000),
            ),
        )
        .await
        .unwrap();
    }

    // Resolve only session 1
    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Proposal",
            &new_message_id(),
            &sid1,
            coord,
            proposal_payload("p1", "option-A", "test"),
        ),
    )
    .await
    .unwrap();

    send_as(
        &mut client,
        voter,
        envelope(
            MODE_DECISION,
            "Vote",
            &new_message_id(),
            &sid1,
            voter,
            vote_payload("p1", "yes", "ok"),
        ),
    )
    .await
    .unwrap();

    send_as(
        &mut client,
        coord,
        envelope(
            MODE_DECISION,
            "Commitment",
            &new_message_id(),
            &sid1,
            coord,
            commitment_payload("c1", "option-A", "team", "done", true),
        ),
    )
    .await
    .unwrap();

    // Verify: session 1 resolved, session 2 still open
    let resp1 = get_session_as(&mut client, coord, &sid1).await.unwrap();
    assert_eq!(resp1.metadata.unwrap().state, 2); // RESOLVED

    let resp2 = get_session_as(&mut client, coord, &sid2).await.unwrap();
    assert_eq!(resp2.metadata.unwrap().state, 1); // OPEN
}

/// True concurrency: multiple clients on independent connections race sends
/// into one session. The kernel must serialize acceptance — every distinct
/// message is accepted exactly once and the session stays consistent. (The
/// two tests above cover session *independence*; this one covers write
/// contention, which sequential loops never exercised.)
#[tokio::test]
async fn concurrent_senders_race_into_one_session() {
    let mut client = common::grpc_client().await;
    let sid = new_session_id();
    let initiator = "agent://race-initiator";
    let senders = [
        "agent://racer-1",
        "agent://racer-2",
        "agent://racer-3",
        "agent://racer-4",
    ];
    let mut participants = vec![initiator];
    participants.extend(senders);

    let ack = send_as(
        &mut client,
        initiator,
        envelope(
            MODE_DECISION,
            "SessionStart",
            &new_message_id(),
            &sid,
            initiator,
            session_start_payload("write contention", &participants, 60_000),
        ),
    )
    .await
    .unwrap();
    assert!(ack.ok);

    const PROPOSALS_PER_SENDER: usize = 5;
    let mut handles = Vec::new();
    for (i, sender) in senders.iter().enumerate() {
        let sid = sid.clone();
        let sender = sender.to_string();
        handles.push(tokio::spawn(async move {
            // Each task gets its own channel so requests genuinely race.
            let mut client = common::grpc_client().await;
            for j in 0..PROPOSALS_PER_SENDER {
                let ack = send_as(
                    &mut client,
                    &sender,
                    envelope(
                        MODE_DECISION,
                        "Proposal",
                        &new_message_id(),
                        &sid,
                        &sender,
                        proposal_payload(&format!("p-{i}-{j}"), "racing", "concurrent write"),
                    ),
                )
                .await
                .expect("send must not transport-fail");
                assert!(
                    ack.ok,
                    "concurrent proposal {i}/{j} rejected: {:?}",
                    ack.error
                );
                assert!(!ack.duplicate, "distinct message flagged duplicate");
            }
        }));
    }
    for handle in handles {
        handle.await.expect("sender task must not panic");
    }

    // The session survived the contention in a consistent state.
    let resp = get_session_as(&mut client, initiator, &sid).await.unwrap();
    assert_eq!(resp.metadata.expect("metadata").state, 1); // OPEN
}
