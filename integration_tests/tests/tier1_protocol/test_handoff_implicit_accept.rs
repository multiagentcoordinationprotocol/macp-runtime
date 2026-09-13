//! RFC-MACP-0010 §5.1 — the handoff implicit accept, proven through the real
//! gRPC boundary.
//!
//! The in-process tests (`tests/handoff_implicit_accept_live.rs` in the root
//! crate) pin the deadline arithmetic to the millisecond. These do not: wall
//! clock over a real socket is noisy, so everything here asserts **presence,
//! shape and order** with generous margins. The timeout a test binds is always
//! >= 500 ms and every sleep clears it by a wide factor.
//!
//! What the wire adds over the in-process tests:
//!
//! * the synthetic accept is reachable by ordinary clients — it shows up in
//!   `StreamSession` replay, in order, and it *shifts the accepted ordinals* of
//!   everything after it;
//! * the rev-2 client boundary (`Mode::validate_client_envelope`) is actually
//!   wired into `Send`, not only into the library `step` path;
//! * suspension is excluded from the timeout end-to-end, across the real
//!   `SuspendSession` / `ResumeSession` RPCs.

use std::time::Duration;

use macp_integration_tests::helpers::*;
use macp_integration_tests::server_manager::ServerManager;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::stream_session_response::Response as StreamResp;
use macp_runtime::pb::{
    CommitmentPayload, Envelope, PolicyDescriptor, RegisterPolicyRequest, ResumeSessionRequest,
    SessionStartPayload, StreamSessionRequest, StreamSessionResponse, SuspendSessionRequest,
};
use prost::Message;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Streaming;

use crate::common;

/// `SESSION_STATE_RESOLVED` (macp/v1/envelope.proto).
const STATE_RESOLVED: i32 = 2;
/// `SESSION_STATE_SUSPENDED`.
const STATE_SUSPENDED: i32 = 4;
/// `SESSION_STATE_OPEN`.
const STATE_OPEN: i32 = 1;
/// `SESSION_STATE_EXPIRED`.
const STATE_EXPIRED: i32 = 3;

/// The reserved synthetic `message_id` namespace
/// (`macp_modes::mode::handoff::IMPLICIT_ACCEPT_MESSAGE_ID_PREFIX`). Spelled
/// out rather than imported: this suite is a black-box client, and the wire
/// contract is the literal, not the constant.
const IMPLICIT_ACCEPT_PREFIX: &str = "implicit-accept:";

// ── setup helpers ───────────────────────────────────────────────────────

/// Register a handoff policy carrying `implicit_accept_timeout_ms` and return
/// its (unique) policy id.
async fn register_handoff_policy(
    client: &mut MacpRuntimeServiceClient<Channel>,
    admin: &str,
    timeout_ms: u64,
) -> String {
    let policy_id = format!(
        "policy.handoff.implicit.{}",
        uuid::Uuid::new_v4().as_hyphenated()
    );
    let descriptor = PolicyDescriptor {
        policy_id: policy_id.clone(),
        mode: MODE_HANDOFF.into(),
        description: "implicit accept timeout".into(),
        rules: serde_json::to_string(&serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": timeout_ms },
            "commitment": { "authority": "initiator_only" }
        }))
        .unwrap(),
        schema_version: 1,
        registered_at_unix_ms: 0,
    };
    let resp = client
        .register_policy(with_sender(
            admin,
            RegisterPolicyRequest {
                policy_descriptor: Some(descriptor),
            },
        ))
        .await
        .expect("RegisterPolicy transport")
        .into_inner();
    assert!(resp.ok, "policy registration failed: {}", resp.error);
    policy_id
}

/// The `SessionStartPayload` every handoff session here opens with. `ttl_ms`
/// is a parameter only because the ordering test at the bottom of this file
/// needs a TTL short enough to lapse mid-test; every other caller passes one
/// that cannot interfere.
fn handoff_start_payload_with_ttl(
    owner: &str,
    target: &str,
    policy_version: &str,
    ttl_ms: i64,
) -> Vec<u8> {
    SessionStartPayload {
        intent: "handoff implicit accept".into(),
        participants: vec![owner.into(), target.into()],
        mode_version: MODE_VERSION.into(),
        configuration_version: CONFIG_VERSION.into(),
        policy_version: policy_version.into(),
        ttl_ms,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

/// A `CommitmentPayload` bound to `policy_version` — `commitment_payload()` in
/// the shared helpers hardcodes `policy.default`, which these sessions are not
/// bound to.
fn bound_commitment_payload(commitment_id: &str, policy_version: &str) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: commitment_id.into(),
        action: "handoff.transferred".into(),
        authority_scope: "test".into(),
        reason: "implicitly accepted".into(),
        mode_version: MODE_VERSION.into(),
        policy_version: policy_version.into(),
        configuration_version: CONFIG_VERSION.into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

/// Wall clock in the same units as `Envelope.timestamp_unix_ms`. The runtime
/// under test is a child process on this machine, so the two readings share a
/// clock and can be compared directly.
fn now_unix_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// SessionStart + HandoffOffer, both asserted accepted. Returns the session id.
async fn open_handoff_with_offer(
    client: &mut MacpRuntimeServiceClient<Channel>,
    owner: &str,
    target: &str,
    policy_version: &str,
    handoff_id: &str,
) -> String {
    open_handoff_with_offer_ttl(client, owner, target, policy_version, handoff_id, 120_000).await
}

/// [`open_handoff_with_offer`] with an explicit session TTL.
async fn open_handoff_with_offer_ttl(
    client: &mut MacpRuntimeServiceClient<Channel>,
    owner: &str,
    target: &str,
    policy_version: &str,
    handoff_id: &str,
    ttl_ms: i64,
) -> String {
    let sid = new_session_id();
    let ack = send_as(
        client,
        owner,
        envelope(
            MODE_HANDOFF,
            "SessionStart",
            &new_message_id(),
            &sid,
            owner,
            handoff_start_payload_with_ttl(owner, target, policy_version, ttl_ms),
        ),
    )
    .await
    .expect("SessionStart transport");
    assert!(ack.ok, "SessionStart rejected: {:?}", ack.error);

    let ack = send_as(
        client,
        owner,
        envelope(
            MODE_HANDOFF,
            "HandoffOffer",
            &new_message_id(),
            &sid,
            owner,
            handoff_offer_payload(handoff_id, target, "ownership", "going off shift"),
        ),
    )
    .await
    .expect("HandoffOffer transport");
    assert!(ack.ok, "HandoffOffer rejected: {:?}", ack.error);
    sid
}

// ── StreamSession plumbing (mirrors test_passive_subscribe.rs) ──────────

fn subscribe_frame(session_id: &str, after_sequence: u64) -> StreamSessionRequest {
    StreamSessionRequest {
        subscribe_session_id: session_id.into(),
        after_sequence,
        envelope: None,
    }
}

async fn open_stream(
    client: &mut MacpRuntimeServiceClient<Channel>,
    sender: &str,
) -> (
    mpsc::Sender<StreamSessionRequest>,
    Streaming<StreamSessionResponse>,
) {
    let (tx, rx) = mpsc::channel::<StreamSessionRequest>(8);
    let mut req = tonic::Request::new(ReceiverStream::new(rx));
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {sender}").parse().expect("valid auth"),
    );
    let response = client
        .stream_session(req)
        .await
        .expect("stream_session opened");
    (tx, response.into_inner())
}

async fn next_envelope(stream: &mut Streaming<StreamSessionResponse>) -> Envelope {
    let resp = tokio::time::timeout(Duration::from_secs(5), stream.message())
        .await
        .expect("timed out waiting for envelope")
        .expect("stream returned error")
        .expect("stream ended unexpectedly");
    match resp.response.expect("response variant") {
        StreamResp::Envelope(env) => env,
        StreamResp::Error(err) => panic!("expected envelope, got error: {err:?}"),
    }
}

// ── 1. the accept becomes due and the session resolves ──────────────────

/// RFC-MACP-0010 §5.1(2): once `implicit_accept_timeout_ms` has elapsed on an
/// outstanding offer, the runtime appends the accept itself — so a
/// `Commitment` that arrives with no explicit `HandoffAccept` in sight is
/// nevertheless committable, and the session resolves.
///
/// The control session at the top is load-bearing: without it this test would
/// still pass for a runtime that resolved every handoff commitment regardless
/// of the offer's disposition.
#[tokio::test]
async fn implicit_accept_resolves_after_timeout() {
    let mut client = common::grpc_client().await;
    let owner = "agent://implicit-owner-1";
    let target = "agent://implicit-target-1";

    // Control: a 60 s timeout — no realistically slow runner reaches it
    // between the offer and the commitment two lines below — so the same flow
    // must be *refused*.
    let slow_policy = register_handoff_policy(&mut client, owner, 60_000).await;
    let slow_sid = open_handoff_with_offer(&mut client, owner, target, &slow_policy, "h1").await;
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &slow_sid,
            owner,
            bound_commitment_payload("c1", &slow_policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(
        !ack.ok,
        "commitment before the implicit-accept timeout must be refused"
    );
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("INVALID_ENVELOPE"),
        "unexpected error: {:?}",
        ack.error
    );

    let policy = register_handoff_policy(&mut client, owner, 600).await;
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h1").await;

    // Well clear of the 600 ms timeout: 2 s of margin over scheduler noise.
    tokio::time::sleep(Duration::from_millis(2_000)).await;

    // No explicit HandoffAccept was ever sent. Without the synthesis this
    // commitment fails `commitment_ready` and is rejected INVALID_ENVELOPE.
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(
        ack.ok,
        "commitment after the implicit-accept timeout must be accepted: {:?}",
        ack.error
    );

    let meta = get_session_as(&mut client, owner, &sid)
        .await
        .expect("GetSession transport")
        .metadata
        .expect("metadata present");
    assert_eq!(
        meta.state, STATE_RESOLVED,
        "session must be Resolved after the commitment, got state {}",
        meta.state
    );

    // The synthetic is attributed to the target but is deliberately NOT
    // credited as participant activity: replay records activity for no entry
    // kind, so doing it live would fork `participant_message_counts` from what
    // the log rebuilds — and the target did not in fact send anything. That
    // omission is wire-visible through `SessionMetadata.participant_activity`,
    // so it is pinned here. The target sent nothing all session, so its
    // counter must still be absent-or-zero.
    let target_messages = meta
        .participant_activity
        .iter()
        .find(|a| a.participant_id == target)
        .map_or(0, |a| a.message_count);
    assert_eq!(
        target_messages, 0,
        "the synthetic accept must not credit the target with participant activity"
    );
}

// ── 2. the synthetic envelope is on the wire, in order ──────────────────

/// The synthetic accept is an ordinary accepted (`Incoming`) history entry, so
/// `StreamSession` subscribers see it — positioned between the last
/// pre-deadline message and the triggering `Commitment`, and consuming an
/// accepted ordinal (everything after it shifts by one).
#[tokio::test]
async fn synthetic_envelope_appears_on_stream_in_order() {
    let mut client = common::grpc_client().await;
    let owner = "agent://implicit-owner-2";
    let target = "agent://implicit-target-2";
    let timeout_ms: i64 = 600;
    let policy = register_handoff_policy(&mut client, owner, timeout_ms as u64).await;
    // Read before the offer is sent, so it is a *lower* bound on the server's
    // `offered_at_ms` — hence `before_offer_ms + timeout_ms` is a sound lower
    // bound on the computed deadline D.
    let before_offer_ms = now_unix_ms();
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h2").await;

    // A HandoffContext before the deadline, so the synthetic has a non-trivial
    // predecessor to be ordered after.
    let context_id = new_message_id();
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "HandoffContext",
            &context_id,
            &sid,
            owner,
            handoff_context_payload("h2", "text/plain", b"runbook"),
        ),
    )
    .await
    .expect("HandoffContext transport");
    assert!(ack.ok, "HandoffContext rejected: {:?}", ack.error);

    tokio::time::sleep(Duration::from_millis(2_000)).await;

    // Read *before* the final 300 ms pause, so it is an upper bound on D with
    // a deliberate gap underneath the send. A runtime that stamped the
    // observation time instead of the deadline would land ~300 ms past this
    // reading rather than inside the noise of a single RPC hop.
    let before_commit_ms = now_unix_ms();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let commit_id = new_message_id();
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &commit_id,
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(ack.ok, "Commitment rejected: {:?}", ack.error);

    // Passive subscribe from ordinal 0 replays the whole accepted history.
    let (tx, mut stream) = open_stream(&mut client, target).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");

    let e0 = next_envelope(&mut stream).await;
    assert_eq!(e0.message_type, "SessionStart");
    let e1 = next_envelope(&mut stream).await;
    assert_eq!(e1.message_type, "HandoffOffer");
    let e2 = next_envelope(&mut stream).await;
    assert_eq!(e2.message_type, "HandoffContext");
    assert_eq!(e2.message_id, context_id);

    // The synthetic accept, between the context and the commitment.
    let e3 = next_envelope(&mut stream).await;
    assert_eq!(
        e3.message_type, "HandoffAccept",
        "expected the synthetic accept between HandoffContext and Commitment, got {}",
        e3.message_type
    );
    assert_eq!(
        e3.message_id,
        format!("{IMPLICIT_ACCEPT_PREFIX}h2"),
        "synthetic message_id must be the deterministic id from §5.1(3)"
    );
    assert_eq!(
        e3.sender, target,
        "the synthetic accept is attributed to the offer's target"
    );
    assert_eq!(e3.session_id, sid);
    assert_eq!(e3.mode, MODE_HANDOFF);
    let accept =
        macp_runtime::handoff_pb::HandoffAcceptPayload::decode(&*e3.payload).expect("decodes");
    assert!(
        accept.implicit,
        "the synthetic accept carries implicit=true"
    );
    assert_eq!(accept.handoff_id, "h2");
    assert_eq!(accept.accepted_by, target);

    // RFC-MACP-0010 §5.1(3): the clock on the synthetic is the *computed
    // deadline* D = offered_at + implicit_accept_timeout_ms, never the moment
    // the runtime happened to notice it was due — replay recomputes D from the
    // log and would fork from the live session otherwise.
    //
    // Millisecond equality is not assertable over a real socket, so this is a
    // two-sided bound. The lower bound is exact: D >= before_offer_ms +
    // timeout_ms, because the server stamped `offered_at_ms` after this client
    // read its clock. The upper bound has roughly 1.4 s of slack — D lands
    // ~600 ms after the offer while `before_commit_ms` is read ~2 s after it —
    // so it is not timing-fragile, yet an observation-time stamp (~300 ms past
    // `before_commit_ms`, by the sleep above) still fails it.
    let deadline_ms = e3.timestamp_unix_ms;
    assert!(
        deadline_ms >= before_offer_ms + timeout_ms,
        "synthetic clock {deadline_ms} precedes the earliest possible deadline {}",
        before_offer_ms + timeout_ms
    );
    assert!(
        deadline_ms <= before_commit_ms,
        "synthetic clock {deadline_ms} is past {before_commit_ms} — it looks like the \
         observation time, not the computed deadline"
    );

    let e4 = next_envelope(&mut stream).await;
    assert_eq!(e4.message_type, "Commitment");
    assert_eq!(e4.message_id, commit_id);
    drop(tx);

    // The ordinal-shift check. Accepted ordinals are 1-based and
    // `after_sequence` is exclusive (`LogStore::get_incoming_after` numbers
    // Incoming entries from 1 and returns those with `ordinal >
    // after_sequence`), so this session numbers SessionStart=1, HandoffOffer=2,
    // HandoffContext=3, synthetic=4, Commitment=5. Resuming after 3 therefore
    // yields the synthetic first — exactly the claim that it consumed an
    // ordinal of its own and pushed the Commitment out to 5.
    let (tx, mut stream) = open_stream(&mut client, target).await;
    tx.send(subscribe_frame(&sid, 3))
        .await
        .expect("send subscribe");
    let at4 = next_envelope(&mut stream).await;
    assert_eq!(
        at4.message_id,
        format!("{IMPLICIT_ACCEPT_PREFIX}h2"),
        "the synthetic must occupy accepted ordinal 4"
    );
    let at5 = next_envelope(&mut stream).await;
    assert_eq!(
        at5.message_id, commit_id,
        "the Commitment must have shifted to accepted ordinal 5"
    );
    drop(tx);
}

// ── 2b. the synthetic reaches *live* subscribers ────────────────────────

/// The test above subscribes after the fact and reads replay. This one proves
/// the other half of the freeze-profile claim that synthetic entries "reach
/// `StreamSession` subscribers": a stream opened *before* the triggering
/// Commitment receives the synthetic over the live broadcast, ahead of the
/// Commitment that provoked it.
///
/// Non-flaky by construction: the subscriber is attached and has drained its
/// replay a full 2 s before the Commitment is sent, so there is no race
/// between subscription and publication — only an ordinary live delivery,
/// with the same 5 s receive budget the rest of this file uses.
#[tokio::test]
async fn synthetic_envelope_reaches_live_stream_subscribers() {
    let mut client = common::grpc_client().await;
    let owner = "agent://implicit-owner-2b";
    let target = "agent://implicit-target-2b";
    let policy = register_handoff_policy(&mut client, owner, 600).await;
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h2b").await;

    // Attach *before* anything is due, and drain the replay so the stream is
    // caught up and everything that follows is a live publication.
    let (tx, mut stream) = open_stream(&mut client, target).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");
    assert_eq!(
        next_envelope(&mut stream).await.message_type,
        "SessionStart"
    );
    assert_eq!(
        next_envelope(&mut stream).await.message_type,
        "HandoffOffer"
    );

    tokio::time::sleep(Duration::from_millis(2_000)).await;

    let commit_id = new_message_id();
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &commit_id,
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(ack.ok, "Commitment rejected: {:?}", ack.error);

    // The synthetic must arrive on the already-open stream, before the
    // Commitment. A runtime that appended it to history without broadcasting
    // it would deliver the Commitment here instead.
    let live = next_envelope(&mut stream).await;
    assert_eq!(
        live.message_type, "HandoffAccept",
        "expected the synthetic accept live on the stream, got {} ({})",
        live.message_type, live.message_id
    );
    assert_eq!(live.message_id, format!("{IMPLICIT_ACCEPT_PREFIX}h2b"));
    assert_eq!(live.sender, target);

    let after = next_envelope(&mut stream).await;
    assert_eq!(after.message_type, "Commitment");
    assert_eq!(after.message_id, commit_id);
    drop(tx);
}

// ── 3. the rev-2 client boundary, on the wire ───────────────────────────

/// RFC-MACP-0010 §5.1(3): a client may neither self-declare an implicit accept
/// nor squat the reserved `message_id` namespace. Both are refused by
/// `Mode::validate_client_envelope`, which the runtime calls from its own
/// `Send` path (it bypasses `macp_modes::step::validate_message`).
///
/// The tail of the test is the freeze-profile invariant: a rejected message
/// consumes no dedup slot and corrupts no session — the same `message_id` is
/// re-sent with a legal payload and accepted, and the session then commits.
#[tokio::test]
async fn client_implicit_and_reserved_ids_rejected_on_the_wire() {
    let mut client = common::grpc_client().await;
    let owner = "agent://implicit-owner-3";
    let target = "agent://implicit-target-3";
    // Timeout 0 disables synthesis entirely: every acceptance in this test is
    // explicit, so nothing here can pass by way of the timeout path.
    let policy = register_handoff_policy(&mut client, owner, 0).await;
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h3").await;

    // (a) a client-submitted HandoffAccept with implicit = true, carrying an
    //     ordinary message_id, is refused.
    let reused_id = new_message_id();
    let implicit_payload = macp_runtime::handoff_pb::HandoffAcceptPayload {
        handoff_id: "h3".into(),
        accepted_by: target.into(),
        reason: "I declare this implicit".into(),
        implicit: true,
    }
    .encode_to_vec();
    let ack = send_as(
        &mut client,
        target,
        envelope(
            MODE_HANDOFF,
            "HandoffAccept",
            &reused_id,
            &sid,
            target,
            implicit_payload,
        ),
    )
    .await
    .expect("implicit HandoffAccept transport");
    assert!(
        !ack.ok,
        "a client-submitted implicit HandoffAccept must be rejected"
    );
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("INVALID_ENVELOPE"),
        "unexpected error: {:?}",
        ack.error
    );

    // (b) any message_id in the reserved namespace is refused, whatever the
    //     message type — here a perfectly well-formed HandoffContext.
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "HandoffContext",
            &format!("{IMPLICIT_ACCEPT_PREFIX}h3"),
            &sid,
            owner,
            handoff_context_payload("h3", "text/plain", b"squatting the id"),
        ),
    )
    .await
    .expect("reserved-id HandoffContext transport");
    assert!(
        !ack.ok,
        "a client envelope carrying the reserved synthetic message_id prefix must be rejected"
    );
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("INVALID_ENVELOPE"),
        "unexpected error: {:?}",
        ack.error
    );

    // (c) neither rejection consumed a dedup slot: the *same* message_id from
    //     (a), now with a legal payload, is accepted.
    let ack = send_as(
        &mut client,
        target,
        envelope(
            MODE_HANDOFF,
            "HandoffAccept",
            &reused_id,
            &sid,
            target,
            handoff_accept_payload("h3", target, "explicit accept"),
        ),
    )
    .await
    .expect("explicit HandoffAccept transport");
    assert!(
        ack.ok,
        "a rejected message must not consume its message_id: {:?}",
        ack.error
    );
    // `ok` alone is too weak: a consumed dedup slot answers a re-send with an
    // idempotent `ok` ack too. `duplicate == false` is the discriminator.
    assert!(
        !ack.duplicate,
        "the re-sent message_id must be freshly accepted, not replayed as a duplicate"
    );

    // (d) and the session is otherwise intact — it commits and resolves.
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(ack.ok, "Commitment rejected: {:?}", ack.error);

    let meta = get_session_as(&mut client, owner, &sid)
        .await
        .expect("GetSession transport")
        .metadata
        .expect("metadata present");
    assert_eq!(meta.state, STATE_RESOLVED, "session must be Resolved");
}

// ── 4. suspended time does not tick ─────────────────────────────────────

/// RFC-MACP-0010 §5.1 with RFC-MACP-0003 suspension semantics: the
/// implicit-accept deadline is measured in *unsuspended* time. A session
/// paused across the whole timeout window has not aged into the accept, so a
/// commitment sent immediately on resume is refused — and only becomes
/// acceptable after the remaining unsuspended time actually elapses.
#[tokio::test]
async fn suspended_time_does_not_tick_on_the_wire() {
    let mut client = common::grpc_client().await;
    let owner = "agent://implicit-owner-4";
    let target = "agent://implicit-target-4";
    // Unlike the sleeps elsewhere in this file — upper bounds that a loaded
    // runner can only overshoot in the safe direction — the window between the
    // offer and the `SuspendSession` below is a *deadline*: the RPC has to be
    // processed within `timeout_ms` of unsuspended time or the accept falls due
    // inside the pause and the negative assertion below flips. So it is sized
    // for a badly contended CI runner, not for a laptop.
    let timeout_ms = 3_000u64;
    let policy = register_handoff_policy(&mut client, owner, timeout_ms).await;
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h4").await;

    // Suspend promptly after the offer, so almost no unsuspended time accrues.
    let ack = client
        .suspend_session(with_sender(
            owner,
            SuspendSessionRequest {
                session_id: sid.clone(),
                reason: "pausing across the implicit-accept window".into(),
            },
        ))
        .await
        .expect("SuspendSession transport")
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "suspend must succeed: {:?}", ack.error);
    assert_eq!(ack.session_state, STATE_SUSPENDED);

    // Sleep well past the timeout — but entirely inside the pause. Kept at 2x
    // the timeout, as before.
    tokio::time::sleep(Duration::from_millis(timeout_ms * 2)).await;

    let ack = client
        .resume_session(with_sender(
            owner,
            ResumeSessionRequest {
                session_id: sid.clone(),
                reason: "back".into(),
            },
        ))
        .await
        .expect("ResumeSession transport")
        .into_inner()
        .ack
        .expect("ack present");
    assert!(ack.ok, "resume must succeed: {:?}", ack.error);
    assert_eq!(ack.session_state, STATE_OPEN);

    // Immediately on resume: ~0 ms of unsuspended time has passed since the
    // offer, so nothing is due and the commitment has no accepted offer.
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(
        !ack.ok,
        "the paused interval must not count toward the implicit-accept timeout"
    );
    assert_eq!(
        ack.error.as_ref().map(|e| e.code.as_str()),
        Some("INVALID_ENVELOPE"),
        "unexpected error: {:?}",
        ack.error
    );

    // Control: once the timeout elapses in *unsuspended* time, the same
    // commitment goes through. This is what keeps the assertion above from
    // passing for an unrelated reason (a wedged session, a bad payload).
    tokio::time::sleep(Duration::from_millis(timeout_ms * 2)).await;
    let ack = send_as(
        &mut client,
        owner,
        envelope(
            MODE_HANDOFF,
            "Commitment",
            &new_message_id(),
            &sid,
            owner,
            bound_commitment_payload("c1", &policy),
        ),
    )
    .await
    .expect("Commitment transport");
    assert!(
        ack.ok,
        "the commitment must succeed once unsuspended time clears the timeout: {:?}",
        ack.error
    );
}

// ── 6. the eager sweep, on a timer, through the binary ──────────────────

fn test_binary() -> String {
    std::env::var("MACP_TEST_BINARY").unwrap_or_else(|_| "../target/debug/macp-runtime".into())
}

/// RFC-MACP-0010 §5.1(2)'s eager SHOULD, end to end: the runtime observes the
/// implicit-accept deadline **on its own timer**, with no further
/// session-scoped message of any kind.
///
/// Everything above this section proves the *lazy* MUST — the synthesis fires
/// because a message arrived. This test sends nothing after the `HandoffOffer`.
/// The only client action between the offer and the assertion is opening a
/// `StreamSession` subscription, which is read-only and enters no history, so
/// the accept that shows up on that stream can only have come from
/// `Runtime::sweep_due_synthetic_accepts` being called by the background
/// maintenance loop in `src/main.rs`.
///
/// Its own server, because the shared one runs the default 60 s cleanup
/// interval; `MACP_CLEANUP_INTERVAL_SECS=1` makes the latency bound testable.
/// `ServerManager` picks its own free port and kills only the child it spawned.
///
/// The control session is what stops this from passing for a sweep that
/// accepts every outstanding offer it finds: same server, same ticks, a 60 s
/// timeout, and nothing may be emitted for it.
#[tokio::test]
async fn eager_sweep_settles_the_offer_with_no_further_message() {
    let manager =
        ServerManager::start_with_env(&test_binary(), &[("MACP_CLEANUP_INTERVAL_SECS", "1")])
            .await
            .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    let owner = "agent://sweep-owner";
    let target = "agent://sweep-target";
    let timeout_ms: i64 = 600;

    // Control: 60 s, so no number of 1 s ticks reaches it during this test.
    let slow_policy = register_handoff_policy(&mut client, owner, 60_000).await;
    let slow_sid = open_handoff_with_offer(&mut client, owner, target, &slow_policy, "hs").await;

    let policy = register_handoff_policy(&mut client, owner, timeout_ms as u64).await;
    let offer_sent_at = now_unix_ms();
    let sid = open_handoff_with_offer(&mut client, owner, target, &policy, "h6").await;

    // Attach after the offer and drain the replay, so anything that follows is
    // a live publication rather than history.
    let (tx, mut stream) = open_stream(&mut client, target).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");
    assert_eq!(
        next_envelope(&mut stream).await.message_type,
        "SessionStart"
    );
    assert_eq!(
        next_envelope(&mut stream).await.message_type,
        "HandoffOffer"
    );

    // No Send call is made from here on. `next_envelope` waits up to 5 s, which
    // is >= 4 cleanup ticks past the 600 ms deadline; without the sweep wired
    // into the maintenance loop nothing is ever published and this times out.
    let live = next_envelope(&mut stream).await;
    let observed_at = now_unix_ms();
    assert_eq!(
        live.message_type, "HandoffAccept",
        "expected the swept accept live on the stream, got {} ({})",
        live.message_type, live.message_id
    );
    assert_eq!(live.message_id, format!("{IMPLICIT_ACCEPT_PREFIX}h6"));
    assert_eq!(live.sender, target);

    // The recorded clock is the computed deadline, not the tick that noticed
    // it. Bounded rather than exact — wall clock over a socket is noisy — but
    // the upper bound is the point: a tick-stamped entry would land at or after
    // the observation, which is at least one whole interval later.
    assert!(
        live.timestamp_unix_ms >= offer_sent_at + timeout_ms,
        "D {} must be at or after offer + timeout ({})",
        live.timestamp_unix_ms,
        offer_sent_at + timeout_ms
    );
    assert!(
        live.timestamp_unix_ms < observed_at,
        "D {} must precede the sweep that observed it ({observed_at})",
        live.timestamp_unix_ms
    );

    // Synthesis is not resolution: the session is still Open, waiting for its
    // Commitment.
    let meta = get_session_as(&mut client, owner, &sid)
        .await
        .expect("GetSession transport")
        .metadata
        .expect("metadata present");
    assert_eq!(
        meta.state, STATE_OPEN,
        "the swept accept must not resolve the session"
    );

    // The control offer, on the same server and the same ticks, is untouched.
    let (tx2, mut slow_stream) = open_stream(&mut client, target).await;
    tx2.send(subscribe_frame(&slow_sid, 0))
        .await
        .expect("send subscribe");
    assert_eq!(
        next_envelope(&mut slow_stream).await.message_type,
        "SessionStart"
    );
    assert_eq!(
        next_envelope(&mut slow_stream).await.message_type,
        "HandoffOffer"
    );
    let quiet = tokio::time::timeout(Duration::from_millis(1_500), slow_stream.message()).await;
    assert!(
        quiet.is_err(),
        "an offer inside its window must not be swept: {quiet:?}"
    );

    drop(tx);
    drop(tx2);
}

// ── 7. the maintenance loop's ordering, pinned ──────────────────────────

/// `MACP_CLEANUP_INTERVAL_SECS` for the ordering test. Any value works; this
/// one only has to be short enough to keep the test quick.
const ORDERING_TICK_SECS: i64 = 2;

/// Bound as BOTH the session `ttl_ms` and the policy's
/// `implicit_accept_timeout_ms`, so the two deadlines land within one offer
/// round trip of each other. See the test's own doc comment for why that
/// coincidence is the entire point.
const ORDERING_TTL_MS: i64 = 2_500;

/// The one line in `src/main.rs` that Phase 12's eager/lazy equivalence rests
/// on: `sweep_due_synthetic_accepts` is called **after**
/// `cleanup_expired_sessions`, never before.
///
/// This is a tier-1 test on purpose. `src/main.rs` is the binary's entry point
/// and is not compiled into any in-process test, so the maintenance loop's
/// call order is only observable through a spawned server — which is exactly
/// how the invariant went unpinned.
///
/// # Why the two deadlines must be simultaneous
///
/// The precedence only exists for a session whose TTL **and** implicit-accept
/// deadline have both lapsed unobserved, and it is only decided when one tick
/// finds both due. If the TTL lapsed a whole tick earlier, the session is
/// already `Expired` before the deadline arrives and either call order gives
/// the same answer — a green test proving nothing. So:
///
/// * `implicit_accept_timeout_ms == ttl_ms`, and the offer is accepted
///   strictly after the `SessionStart` envelope's own `timestamp_unix_ms`
///   (which is what `ttl_expiry` is computed from, RFC-MACP-0003 §2). `D` is
///   therefore >= `ttl_expiry` by exactly the offer's round trip — a few
///   milliseconds, asserted below — so no tick can realistically fall between
///   them, and the one that finds either finds both;
/// * `D >= ttl_expiry` rather than the other way round is the safe asymmetry.
///   A tick landing in the gap would see the TTL lapsed and the deadline not,
///   which both call orders resolve to "expired, no accept" — vacuous but
///   green. The inverse gap would red a correct runtime.
///
/// # Expected outcome
///
/// Correct order: `cleanup_expired_sessions` expires the session, the sweep
/// then declines it for being non-`Open`, and accepted history ends at the
/// offer. That is the precedence the lazy path already gives, where
/// `Precheck::Expired` returns before `synthesize_due_accept` is reached.
///
/// Inverted order: the sweep still sees an `Open` session with a lapsed
/// deadline, appends the synthetic `HandoffAccept`, and only then does cleanup
/// expire it — so the accept shows up in the replay below and this test reds.
#[tokio::test]
async fn a_tick_finding_both_deadlines_due_expires_rather_than_accepting() {
    let manager = ServerManager::start_with_env(
        &test_binary(),
        &[(
            "MACP_CLEANUP_INTERVAL_SECS",
            &ORDERING_TICK_SECS.to_string(),
        )],
    )
    .await
    .expect("server must start");
    let mut client = MacpRuntimeServiceClient::connect(manager.endpoint.clone())
        .await
        .expect("connect");

    let owner = "agent://ordering-owner";
    let target = "agent://ordering-target";

    let policy = register_handoff_policy(&mut client, owner, ORDERING_TTL_MS as u64).await;
    let t0 = now_unix_ms();
    let sid =
        open_handoff_with_offer_ttl(&mut client, owner, target, &policy, "h7", ORDERING_TTL_MS)
            .await;
    let offer_acked_at = now_unix_ms();

    // Non-vacuity guard, not a performance assertion: `offer_acked_at - t0` is
    // an upper bound on the gap between the two deadlines. A machine so loaded
    // that this exceeds half a tick could put a tick inside the gap, at which
    // point the test no longer discriminates and should say so out loud rather
    // than pass.
    let gap_bound_ms = offer_acked_at - t0;
    assert!(
        gap_bound_ms < ORDERING_TICK_SECS * 500,
        "setup took {gap_bound_ms} ms, which is too wide a gap between ttl_expiry \
         and the implicit-accept deadline for this test to discriminate"
    );

    // Nothing may touch this session until the sweep has had its tick.
    // `GetSession` and the `StreamSession` subscribe frame both go through
    // `Runtime::get_session_checked`, which expires a lapsed session itself —
    // observing early would settle the race before the maintenance loop ever
    // saw it, and hand back a green test either way round.
    let wake_at = t0 + ORDERING_TTL_MS + ORDERING_TICK_SECS * 1_000 + 2_500;
    tokio::time::sleep(Duration::from_millis(
        (wake_at - now_unix_ms()).max(0) as u64
    ))
    .await;

    // Accepted history, in full. The synthetic accept is an ordinary
    // `Incoming` entry, so if the sweep emitted one it is right here; the
    // `TtlExpired` entry is `Internal` and never appears on this stream.
    let (tx, mut stream) = open_stream(&mut client, owner).await;
    tx.send(subscribe_frame(&sid, 0))
        .await
        .expect("send subscribe");
    let mut history: Vec<String> = Vec::new();
    while let Ok(frame) = tokio::time::timeout(Duration::from_millis(1_500), stream.message()).await
    {
        match frame {
            Ok(Some(resp)) => match resp.response.expect("response variant") {
                StreamResp::Envelope(env) => history.push(env.message_type),
                StreamResp::Error(err) => panic!("stream error: {err:?}"),
            },
            // Stream ended: whatever was replayed is all there is.
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(
        history,
        vec!["SessionStart".to_string(), "HandoffOffer".to_string()],
        "a TTL that lapsed alongside the implicit-accept deadline must leave no \
         synthetic accept in history — the sweep ran before cleanup"
    );

    let meta = get_session_as(&mut client, owner, &sid)
        .await
        .expect("GetSession transport")
        .metadata
        .expect("metadata present");
    assert_eq!(
        meta.state, STATE_EXPIRED,
        "session must be Expired, got state {}",
        meta.state
    );

    drop(tx);
}
