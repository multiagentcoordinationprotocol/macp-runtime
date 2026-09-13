use chrono::Utc;
use macp_runtime::log_store::LogStore;
use macp_runtime::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::SessionState;
use macp_runtime::storage::MemoryBackend;
use prost::Message;
use std::sync::Arc;

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn make_runtime() -> Runtime {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    Runtime::new(storage, registry, log_store)
}

fn session_start(participants: Vec<String>) -> Vec<u8> {
    SessionStartPayload {
        intent: "integration-test".into(),
        participants,
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: String::new(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

fn envelope(
    mode: &str,
    message_type: &str,
    message_id: &str,
    session_id: &str,
    sender: &str,
    payload: Vec<u8>,
) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: mode.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: session_id.into(),
        sender: sender.into(),
        timestamp_unix_ms: Utc::now().timestamp_millis(),
        payload,
    }
}

fn commitment(action: &str) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: "c1".into(),
        action: action.into(),
        authority_scope: "test".into(),
        reason: "done".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy.default".into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

#[tokio::test]
async fn decision_full_lifecycle_through_runtime() {
    use macp_runtime::decision_pb::{ProposalPayload, VotePayload};

    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.decision.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://orchestrator",
            session_start(vec![
                "agent://orchestrator".into(),
                "agent://a".into(),
                "agent://b".into(),
            ]),
        ),
        None,
    )
    .await
    .unwrap();

    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    rt.process(
        &envelope(
            mode,
            "Proposal",
            "m2",
            &sid,
            "agent://orchestrator",
            proposal,
        ),
        None,
    )
    .await
    .unwrap();

    let vote = VotePayload {
        proposal_id: "p1".into(),
        vote: "approve".into(),
        reason: "good".into(),
    }
    .encode_to_vec();
    rt.process(&envelope(mode, "Vote", "m3", &sid, "agent://a", vote), None)
        .await
        .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m4",
                &sid,
                "agent://orchestrator",
                commitment("decision.selected"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

#[tokio::test]
async fn proposal_full_lifecycle_through_runtime() {
    use macp_runtime::proposal_pb::{AcceptPayload, ProposalPayload};

    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.proposal.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://buyer",
            session_start(vec!["agent://buyer".into(), "agent://seller".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        title: "offer".into(),
        summary: "terms".into(),
        details: vec![],
        tags: vec![],
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "Proposal", "m2", &sid, "agent://seller", proposal),
        None,
    )
    .await
    .unwrap();

    let accept = AcceptPayload {
        proposal_id: "p1".into(),
        reason: String::new(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "Accept", "m3", &sid, "agent://buyer", accept.clone()),
        None,
    )
    .await
    .unwrap();
    rt.process(
        &envelope(mode, "Accept", "m4", &sid, "agent://seller", accept),
        None,
    )
    .await
    .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m5",
                &sid,
                "agent://buyer",
                commitment("proposal.accepted"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

#[tokio::test]
async fn task_full_lifecycle_through_runtime() {
    use macp_runtime::task_pb::{TaskAcceptPayload, TaskCompletePayload, TaskRequestPayload};

    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.task.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://planner",
            session_start(vec!["agent://planner".into(), "agent://worker".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    let request = TaskRequestPayload {
        task_id: "t1".into(),
        title: "Build widget".into(),
        instructions: "Do it".into(),
        requested_assignee: "agent://worker".into(),
        input: vec![],
        deadline_unix_ms: 0,
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "TaskRequest", "m2", &sid, "agent://planner", request),
        None,
    )
    .await
    .unwrap();

    let accept = TaskAcceptPayload {
        task_id: "t1".into(),
        assignee: "agent://worker".into(),
        reason: "ready".into(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "TaskAccept", "m3", &sid, "agent://worker", accept),
        None,
    )
    .await
    .unwrap();

    let complete = TaskCompletePayload {
        task_id: "t1".into(),
        assignee: "agent://worker".into(),
        output: b"result".to_vec(),
        summary: "done".into(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "TaskComplete", "m4", &sid, "agent://worker", complete),
        None,
    )
    .await
    .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m5",
                &sid,
                "agent://planner",
                commitment("task.completed"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

#[tokio::test]
async fn handoff_full_lifecycle_through_runtime() {
    use macp_runtime::handoff_pb::{HandoffAcceptPayload, HandoffOfferPayload};

    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.handoff.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://owner",
            session_start(vec!["agent://owner".into(), "agent://target".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    let offer = HandoffOfferPayload {
        handoff_id: "h1".into(),
        target_participant: "agent://target".into(),
        scope: "support".into(),
        reason: "escalate".into(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "HandoffOffer", "m2", &sid, "agent://owner", offer),
        None,
    )
    .await
    .unwrap();

    let accept = HandoffAcceptPayload {
        handoff_id: "h1".into(),
        accepted_by: "agent://target".into(),
        reason: "ready".into(),
        implicit: false,
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "HandoffAccept", "m3", &sid, "agent://target", accept),
        None,
    )
    .await
    .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m4",
                &sid,
                "agent://owner",
                commitment("handoff.accepted"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

#[tokio::test]
async fn quorum_full_lifecycle_through_runtime() {
    use macp_runtime::quorum_pb::{ApprovalRequestPayload, ApprovePayload};

    let rt = make_runtime();
    let sid = new_sid();
    let mode = "macp.mode.quorum.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://coordinator",
            session_start(vec![
                "agent://alice".into(),
                "agent://bob".into(),
                "agent://carol".into(),
            ]),
        ),
        None,
    )
    .await
    .unwrap();

    let request = ApprovalRequestPayload {
        request_id: "r1".into(),
        action: "deploy.production".into(),
        summary: "Deploy v2".into(),
        details: vec![],
        required_approvals: 2,
    }
    .encode_to_vec();
    rt.process(
        &envelope(
            mode,
            "ApprovalRequest",
            "m2",
            &sid,
            "agent://coordinator",
            request,
        ),
        None,
    )
    .await
    .unwrap();

    let approve1 = ApprovePayload {
        request_id: "r1".into(),
        reason: "lgtm".into(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "Approve", "m3", &sid, "agent://alice", approve1),
        None,
    )
    .await
    .unwrap();

    let approve2 = ApprovePayload {
        request_id: "r1".into(),
        reason: "ship it".into(),
    }
    .encode_to_vec();
    rt.process(
        &envelope(mode, "Approve", "m4", &sid, "agent://bob", approve2),
        None,
    )
    .await
    .unwrap();

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m5",
                &sid,
                "agent://coordinator",
                commitment("quorum.approved"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

#[tokio::test]
async fn multi_round_full_lifecycle_through_runtime() {
    let rt = make_runtime();
    let sid = new_sid();
    let mode = "ext.multi_round.v1";

    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://coordinator",
            session_start(vec!["agent://alice".into(), "agent://bob".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    rt.process(
        &envelope(
            mode,
            "Contribute",
            "m2",
            &sid,
            "agent://alice",
            br#"{"value":"option_a"}"#.to_vec(),
        ),
        None,
    )
    .await
    .unwrap();

    rt.process(
        &envelope(
            mode,
            "Contribute",
            "m3",
            &sid,
            "agent://bob",
            br#"{"value":"option_b"}"#.to_vec(),
        ),
        None,
    )
    .await
    .unwrap();

    rt.process(
        &envelope(
            mode,
            "Contribute",
            "m4",
            &sid,
            "agent://bob",
            br#"{"value":"option_a"}"#.to_vec(),
        ),
        None,
    )
    .await
    .unwrap();

    // Session still Open — convergence tracked, requires Commitment
    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(session.state, SessionState::Open);

    let result = rt
        .process(
            &envelope(
                mode,
                "Commitment",
                "m5",
                &sid,
                "agent://coordinator",
                commitment("multi_round.converged"),
            ),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
}

/// One clock per internal log entry (Phase 11a / follow-on 10).
///
/// `suspend_session` and `resume_session` each read the wall clock exactly
/// once and hand that same instant to both the `Session` mutation and
/// `make_internal_entry`. That makes the live `accumulated_suspended_ms` and
/// the value replay re-derives from the two entry timestamps identical *by
/// construction* — which matters because since Phase 10 that value gates an
/// accept/reject decision, so a sub-millisecond divergence could make a
/// live-`Resolved` session fail replay outright.
///
/// Honest note on what this test can and cannot do: with the clock injected
/// the equality holds deterministically, and before the fix it would only have
/// broken when a millisecond tick happened to land between the two `Utc::now()`
/// reads. So this pins the invariant rather than differentially proving the old
/// bug; the injected-clock signature is the real guarantee.
#[tokio::test]
async fn suspend_resume_entries_share_the_session_mutation_clock() {
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> = Arc::new(MemoryBackend);
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let rt = Runtime::new(storage, registry, Arc::clone(&log_store));

    let mode = "macp.mode.decision.v1";
    let sid = new_sid();
    rt.process(
        &envelope(
            mode,
            "SessionStart",
            "m1",
            &sid,
            "agent://coordinator",
            session_start(vec!["agent://coordinator".into(), "agent://alice".into()]),
        ),
        None,
    )
    .await
    .unwrap();

    rt.suspend_session(&sid, "pause", "agent://coordinator")
        .await
        .unwrap();
    // Let a millisecond boundary pass so the banked duration is non-trivial;
    // the assertion below is an exact equality either way.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    rt.resume_session(&sid, "carry on", "agent://coordinator")
        .await
        .unwrap();

    let entries = log_store.get_log(&sid).await.unwrap();
    let stamp_of = |message_type: &str| -> i64 {
        let matching: Vec<&macp_runtime::log_store::LogEntry> = entries
            .iter()
            .filter(|e| e.message_type == message_type)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one {message_type} entry, got {}",
            matching.len()
        );
        matching[0].received_at_ms
    };
    let suspended_at = stamp_of("SessionSuspend");
    let resumed_at = stamp_of("SessionResume");

    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(session.state, SessionState::Open);
    assert_eq!(
        session.accumulated_suspended_ms,
        resumed_at - suspended_at,
        "live accumulated_suspended_ms must equal the span between the two \
         internal log entries, or replay reconstructs a different value"
    );
    assert!(
        session.accumulated_suspended_ms > 0,
        "the suspension must have measured something, else the equality is vacuous"
    );

    // Phase 11b acceptance criterion 4 — live/replay agreement on the new
    // `suspension_intervals` vec. The live session records the pair in
    // `Session::resume`; replay reconstructs it by driving the same method
    // from the two internal entries' recorded timestamps, so the two must be
    // identical, not merely consistent.
    assert_eq!(
        session.suspension_intervals,
        vec![(suspended_at, resumed_at)],
        "the live session's completed-pause record must match the internal \
         log entries it was derived from"
    );
    let replay_registry = macp_runtime::mode_registry::ModeRegistry::build_default(Arc::new(
        macp_runtime::policy::DefaultPolicyEvaluator,
    ));
    let replayed = macp_runtime::replay::replay_session(&sid, &entries, &replay_registry, None)
        .expect("replay must succeed");
    assert_eq!(
        replayed.suspension_intervals, session.suspension_intervals,
        "replay must reconstruct the identical suspension intervals"
    );
    assert_eq!(
        replayed.accumulated_suspended_ms,
        session.accumulated_suspended_ms
    );
}
