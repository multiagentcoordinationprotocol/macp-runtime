//! Characterization snapshot of the gRPC refusal surface (plan
//! `plans/http-listener.md` P2 item 0).
//!
//! Drives the refusal matrix over a real tonic server on `127.0.0.1:0` in
//! this process and stores what a client sees as golden files under
//! `tests/golden/grpc_refusals/`:
//!
//! - an in-band refusal: the encoded `Ack` bytes (hex) plus a readable dump;
//! - a request-level refusal: the `Status` code, message and every
//!   `macp-error-*` metadata entry (`-bin` values in base64);
//! - an inline stream refusal: the encoded `StreamSessionResponse` bytes.
//!
//! It was captured before the P2 server-seam refactor (`send_authenticated`,
//! `open_subscription`), which must not change a byte of it.
//!
//! **Normalized** (the only nondeterministic field in the matrix):
//! `Ack.accepted_at_unix_ms` is set to `0` before encoding. Session ids and
//! message ids are fixed constants, so they are kept verbatim. Transport
//! metadata other than `macp-error-*` (content-type, date, grpc-status, ...)
//! is not captured.
//!
//! **Regenerate** only on purpose: `MACP_REGEN_GOLDEN=1 cargo test --test
//! grpc_refusal_snapshot`. Without it a mismatch or a missing golden fails.
//!
//! One `#[tokio::test]` on tokio's current-thread runtime: the token table
//! and limits come from `SecurityLayer::from_env` (the only public way to
//! configure them), and this test binary is its own process, so setting the
//! variables once cannot race another test. The lagged-stream case relies on
//! the current-thread runtime too (see `lagged_stream`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::policy::PolicyDecision;
use macp_runtime::macp_core::session::Session;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    stream_session_response, Ack, CommitmentPayload, Envelope, GetSessionRequest, SendRequest,
    SessionStartPayload, StreamSessionRequest, StreamSessionResponse,
};
use macp_runtime::policy_engine::PolicyEngine;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::{AuthIdentity, SecurityLayer};
use macp_runtime::server::MacpServer;
use prost::Message;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const DECISION: &str = "macp.mode.decision.v1";
const PROPOSAL: &str = "macp.mode.proposal.v1";

const ORCH: &str = "agent://orchestrator";
const PEER: &str = "agent://peer";
const DENIED: &str = "agent://denied";

/// S1: an open decision session (orchestrator + peer).
const S1: &str = "11111111-1111-4111-8111-111111111111";
/// S2: a proposal session (on the file-backed runtime) resolved by
/// Commitment, so terminal compaction leaves a checkpoint whose base is 5.
const S2: &str = "22222222-2222-4222-8222-222222222222";
/// S3: the lagged-stream session.
const S3: &str = "33333333-3333-4333-8333-333333333333";
/// S4: a session `agent://denied` is a participant of (policy-engine reads).
const S4: &str = "44444444-4444-4444-8444-444444444444";
const UNKNOWN: &str = "99999999-9999-4999-8999-999999999999";
const MAXOPEN_A: &str = "55555555-5555-4555-8555-555555555555";
const MAXOPEN_B: &str = "66666666-6666-4666-8666-666666666666";

const MESSAGE_LIMIT: usize = 5;
const START_LIMIT: usize = 5;
const MAX_PAYLOAD: usize = 512;
/// `stream_bus::DEFAULT_SESSION_STREAM_CAPACITY`.
const BUS_CAPACITY: usize = 256;
const LAG_EXTRA: usize = 44;

const TOKENS: &str = r#"[
  {"token":"tok-orch","sender":"agent://orchestrator"},
  {"token":"tok-peer","sender":"agent://peer","can_start_sessions":false},
  {"token":"tok-outsider","sender":"agent://outsider"},
  {"token":"tok-limited","sender":"agent://limited","allowed_modes":["macp.mode.task.v1"]},
  {"token":"tok-rl-msg","sender":"agent://rl-msg"},
  {"token":"tok-rl-start","sender":"agent://rl-start"},
  {"token":"tok-maxopen","sender":"agent://maxopen","max_open_sessions":1},
  {"token":"tok-denied","sender":"agent://denied"}
]"#;

/// Denies every decision point for one sender, allows everyone else.
struct DenyOne;

impl DenyOne {
    fn decide(identity: &AuthIdentity) -> PolicyDecision {
        if identity.sender == DENIED {
            PolicyDecision::Deny {
                reasons: vec!["embargoed by the snapshot engine".into()],
            }
        } else {
            PolicyDecision::Allow { reasons: vec![] }
        }
    }
}

#[async_trait::async_trait]
impl PolicyEngine for DenyOne {
    async fn evaluate_session_start(
        &self,
        identity: &AuthIdentity,
        _mode: &str,
        _env: &Envelope,
    ) -> PolicyDecision {
        Self::decide(identity)
    }
    async fn evaluate_message(
        &self,
        identity: &AuthIdentity,
        _session: &Session,
        _env: &Envelope,
    ) -> PolicyDecision {
        Self::decide(identity)
    }
    async fn evaluate_session_access(
        &self,
        identity: &AuthIdentity,
        _session: &Session,
    ) -> PolicyDecision {
        Self::decide(identity)
    }
}

// ── golden files ─────────────────────────────────────────────────────────

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/grpc_refusals")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare `actual` with `<case>.txt`, or write it under `MACP_REGEN_GOLDEN=1`.
/// Mismatches are collected so one run reports every changed case.
fn check(case: &str, actual: String, failures: &mut Vec<String>) {
    let path = golden_dir().join(format!("{case}.txt"));
    if std::env::var("MACP_REGEN_GOLDEN").as_deref() == Ok("1") {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    match std::fs::read_to_string(&path) {
        Ok(expected) if expected == actual => {}
        Ok(expected) => failures.push(format!(
            "{case}: output changed\n--- golden {}\n{expected}\n--- actual\n{actual}",
            path.display()
        )),
        Err(e) => failures.push(format!(
            "{case}: no golden at {} ({e}); regenerate with MACP_REGEN_GOLDEN=1",
            path.display()
        )),
    }
}

fn render_ack(mut ack: Ack) -> String {
    // Normalized: wall-clock time of the refusal.
    ack.accepted_at_unix_ms = 0;
    let mut out = String::from("kind: ack\n");
    out += &format!("encoded_hex: {}\n", hex(&ack.encode_to_vec()));
    out += &format!("ok: {}\n", ack.ok);
    out += &format!("duplicate: {}\n", ack.duplicate);
    out += &format!("message_id: {}\n", ack.message_id);
    out += &format!("session_id: {}\n", ack.session_id);
    out += &format!(
        "accepted_at_unix_ms: {} (normalized)\n",
        ack.accepted_at_unix_ms
    );
    out += &format!("session_state: {}\n", ack.session_state);
    match &ack.error {
        Some(e) => {
            out += &format!("error.code: {}\n", e.code);
            out += &format!("error.message: {}\n", e.message);
            out += &format!("error.session_id: {}\n", e.session_id);
            out += &format!("error.message_id: {}\n", e.message_id);
            out += &format!("error.details: {}\n", String::from_utf8_lossy(&e.details));
        }
        None => out += "error: none\n",
    }
    out
}

fn render_status(status: &tonic::Status) -> String {
    let mut out = String::from("kind: status\n");
    out += &format!("code: {} ({:?})\n", status.code() as i32, status.code());
    out += &format!("message: {}\n", status.message());
    let mut entries: Vec<String> = Vec::new();
    for kv in status.metadata().iter() {
        match kv {
            tonic::metadata::KeyAndValueRef::Ascii(k, v)
                if k.as_str().starts_with("macp-error-") =>
            {
                entries.push(format!(
                    "{}: {}",
                    k.as_str(),
                    v.to_str().unwrap_or("<non-ascii>")
                ));
            }
            tonic::metadata::KeyAndValueRef::Binary(k, v)
                if k.as_str().starts_with("macp-error-") =>
            {
                let bytes = v.to_bytes().unwrap();
                entries.push(format!(
                    "{}: {} (utf8: {})",
                    k.as_str(),
                    base64::engine::general_purpose::STANDARD.encode(&bytes),
                    String::from_utf8_lossy(&bytes)
                ));
            }
            _ => {}
        }
    }
    entries.sort();
    for e in entries {
        out += &format!("metadata {e}\n");
    }
    out += &format!("details_hex: {}\n", hex(status.details()));
    out
}

fn render_frame(frame: &StreamSessionResponse) -> String {
    let mut out = String::from("kind: stream_frame\n");
    out += &format!("encoded_hex: {}\n", hex(&frame.encode_to_vec()));
    match &frame.response {
        Some(stream_session_response::Response::Error(e)) => {
            out += &format!("error.code: {}\n", e.code);
            out += &format!("error.message: {}\n", e.message);
            out += &format!("error.session_id: {}\n", e.session_id);
            out += &format!("error.message_id: {}\n", e.message_id);
            out += &format!("error.details: {}\n", String::from_utf8_lossy(&e.details));
        }
        Some(stream_session_response::Response::Envelope(e)) => {
            out += &format!("envelope: {} {}\n", e.message_type, e.message_id);
        }
        None => out += "response: none\n",
    }
    out
}

// ── fixtures ─────────────────────────────────────────────────────────────

fn start_payload(participants: &[&str]) -> Vec<u8> {
    SessionStartPayload {
        intent: "snapshot".into(),
        participants: participants.iter().map(|p| p.to_string()).collect(),
        mode_version: "1.0.0".into(),
        configuration_version: "config.default".into(),
        policy_version: "policy.default".into(),
        ttl_ms: 600_000,
        context_id: String::new(),
        extensions: HashMap::new(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

fn decision_proposal(proposal_id: &str) -> Vec<u8> {
    macp_runtime::decision_pb::ProposalPayload {
        proposal_id: proposal_id.into(),
        option: "opt".into(),
        rationale: "r".into(),
        supporting_data: vec![],
    }
    .encode_to_vec()
}

fn env(mode: &str, message_type: &str, message_id: &str, sid: &str, payload: Vec<u8>) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: mode.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: sid.into(),
        sender: String::new(),
        timestamp_unix_ms: 1_800_000_000_000,
        payload,
    }
}

fn with_token<T>(token: Option<&str>, body: T) -> tonic::Request<T> {
    let mut req = tonic::Request::new(body);
    if let Some(token) = token {
        req.metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    req
}

async fn send(
    client: &mut MacpRuntimeServiceClient<Channel>,
    token: Option<&str>,
    e: Envelope,
) -> Ack {
    client
        .send(with_token(token, SendRequest { envelope: Some(e) }))
        .await
        .expect("Send answers in band")
        .into_inner()
        .ack
        .expect("an ack")
}

async fn send_ok(client: &mut MacpRuntimeServiceClient<Channel>, token: &str, e: Envelope) {
    let what = format!("{} {}", e.message_type, e.message_id);
    let ack = send(client, Some(token), e).await;
    assert!(ack.ok, "{what}: {:?}", ack.error);
}

async fn get_session_err(
    client: &mut MacpRuntimeServiceClient<Channel>,
    token: Option<&str>,
    sid: &str,
) -> tonic::Status {
    client
        .get_session(with_token(
            token,
            GetSessionRequest {
                session_id: sid.into(),
            },
        ))
        .await
        .expect_err("GetSession must refuse")
}

fn subscribe(sid: &str, after_sequence: u64) -> StreamSessionRequest {
    StreamSessionRequest {
        subscribe_session_id: sid.into(),
        after_sequence,
        envelope: None,
    }
}

/// Open a StreamSession as `token`, send one subscribe frame, keep the
/// inbound side open (the sender is returned so the stream is not ended).
async fn open_subscribe(
    client: &mut MacpRuntimeServiceClient<Channel>,
    token: &str,
    sid: &str,
    after: u64,
) -> (
    mpsc::Sender<StreamSessionRequest>,
    tonic::Streaming<StreamSessionResponse>,
) {
    let (tx, rx) = mpsc::channel(4);
    tx.send(subscribe(sid, after)).await.unwrap();
    let stream = client
        .stream_session(with_token(Some(token), ReceiverStream::new(rx)))
        .await
        .expect("stream opens")
        .into_inner();
    (tx, stream)
}

async fn next_frame(
    stream: &mut tonic::Streaming<StreamSessionResponse>,
) -> Result<StreamSessionResponse, tonic::Status> {
    tokio::time::timeout(std::time::Duration::from_secs(5), stream.message())
        .await
        .expect("timed out waiting for a stream item")
        .map(|m| m.expect("stream ended without a frame or status"))
}

async fn serve(server: MacpServer) -> MacpRuntimeServiceClient<Channel> {
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(MacpRuntimeServiceServer::new(server))
            .serve_with_incoming(incoming),
    );
    MacpRuntimeServiceClient::connect(format!("http://{addr}"))
        .await
        .unwrap()
}

#[tokio::test]
async fn grpc_refusal_outputs_match_the_goldens() {
    // Edition 2021: `set_var` is safe; this binary holds one test.
    std::env::set_var("MACP_AUTH_TOKENS_JSON", TOKENS);
    std::env::set_var("MACP_MESSAGE_LIMIT_PER_MINUTE", MESSAGE_LIMIT.to_string());
    std::env::set_var(
        "MACP_SESSION_START_LIMIT_PER_MINUTE",
        START_LIMIT.to_string(),
    );
    std::env::set_var("MACP_MAX_PAYLOAD_BYTES", MAX_PAYLOAD.to_string());
    let security = SecurityLayer::from_env().expect("security from env");

    let runtime = Arc::new(Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));
    let mut client = serve(MacpServer::new(runtime.clone(), security.clone())).await;
    // Same runtime and SecurityLayer (shared rate buckets), plus an engine.
    let mut engine_client = serve(
        MacpServer::new(runtime.clone(), security.clone()).with_policy_engine(Arc::new(DenyOne)),
    )
    .await;
    // Terminal compaction needs a backend that supports it (the in-memory
    // one does not), so the compacted-resume case gets a file-backed runtime.
    // Kept apart from `runtime`: file I/O yields, which the lag case must not.
    let data_dir = tempfile::tempdir().unwrap();
    let file_runtime = Arc::new(Runtime::new(
        Arc::new(macp_runtime::storage::FileBackend::new(data_dir.path().to_path_buf()).unwrap()),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));
    let mut file_client = serve(MacpServer::new(file_runtime, security)).await;

    let mut failures = Vec::new();

    // Fixtures.
    send_ok(
        &mut client,
        "tok-orch",
        env(
            DECISION,
            "SessionStart",
            "s1-start",
            S1,
            start_payload(&[ORCH, PEER]),
        ),
    )
    .await;
    send_ok(
        &mut client,
        "tok-orch",
        env(
            DECISION,
            "SessionStart",
            "s4-start",
            S4,
            start_payload(&[ORCH, DENIED]),
        ),
    )
    .await;

    // ── authentication ──
    let ack = send(
        &mut client,
        None,
        env(
            DECISION,
            "Proposal",
            "m-no-token",
            S1,
            decision_proposal("p1"),
        ),
    )
    .await;
    check("send_no_token", render_ack(ack), &mut failures);
    let ack = send(
        &mut client,
        Some("tok-bogus"),
        env(
            DECISION,
            "Proposal",
            "m-bad-token",
            S1,
            decision_proposal("p1"),
        ),
    )
    .await;
    check("send_invalid_token", render_ack(ack), &mut failures);
    let status = get_session_err(&mut client, None, S1).await;
    check(
        "get_session_no_token",
        render_status(&status),
        &mut failures,
    );
    let status = get_session_err(&mut client, Some("tok-bogus"), S1).await;
    check(
        "get_session_invalid_token",
        render_status(&status),
        &mut failures,
    );
    let status = client
        .stream_session(with_token(
            None,
            ReceiverStream::new(mpsc::channel::<StreamSessionRequest>(1).1),
        ))
        .await
        .expect_err("an unauthenticated stream is refused");
    check("stream_no_token", render_status(&status), &mut failures);

    // ── spoofed sender ──
    let mut spoofed = env(
        DECISION,
        "Proposal",
        "m-spoofed",
        S1,
        decision_proposal("p1"),
    );
    spoofed.sender = ORCH.into();
    let ack = send(&mut client, Some("tok-peer"), spoofed).await;
    check("send_spoofed_sender", render_ack(ack), &mut failures);

    // ── forbidden mode ──
    let ack = send(
        &mut client,
        Some("tok-limited"),
        env(
            DECISION,
            "SessionStart",
            "m-forbidden-mode",
            UNKNOWN,
            start_payload(&["agent://limited"]),
        ),
    )
    .await;
    check("send_forbidden_mode", render_ack(ack), &mut failures);

    // ── message rate limit: MESSAGE_LIMIT kernel refusals fill the bucket ──
    for i in 0..MESSAGE_LIMIT {
        let ack = send(
            &mut client,
            Some("tok-rl-msg"),
            env(
                DECISION,
                "Proposal",
                &format!("m-rl-{i}"),
                UNKNOWN,
                decision_proposal("p1"),
            ),
        )
        .await;
        assert!(!ack.ok);
    }
    let ack = send(
        &mut client,
        Some("tok-rl-msg"),
        env(
            DECISION,
            "Proposal",
            "m-rl-over",
            UNKNOWN,
            decision_proposal("p1"),
        ),
    )
    .await;
    check("send_message_rate_limited", render_ack(ack), &mut failures);

    // ── SessionStart rate limit ──
    for i in 0..START_LIMIT {
        let ack = send(
            &mut client,
            Some("tok-rl-start"),
            env(
                DECISION,
                "SessionStart",
                &format!("m-rls-{i}"),
                UNKNOWN,
                vec![],
            ),
        )
        .await;
        assert!(!ack.ok);
    }
    let ack = send(
        &mut client,
        Some("tok-rl-start"),
        env(DECISION, "SessionStart", "m-rls-over", UNKNOWN, vec![]),
    )
    .await;
    check(
        "send_session_start_rate_limited",
        render_ack(ack),
        &mut failures,
    );

    // ── max_open_sessions ──
    send_ok(
        &mut client,
        "tok-maxopen",
        env(
            DECISION,
            "SessionStart",
            "mo-a",
            MAXOPEN_A,
            start_payload(&["agent://maxopen"]),
        ),
    )
    .await;
    let ack = send(
        &mut client,
        Some("tok-maxopen"),
        env(
            DECISION,
            "SessionStart",
            "mo-b",
            MAXOPEN_B,
            start_payload(&["agent://maxopen"]),
        ),
    )
    .await;
    check("send_max_open_sessions", render_ack(ack), &mut failures);

    // ── payload too large ──
    let ack = send(
        &mut client,
        Some("tok-peer"),
        env(
            DECISION,
            "Proposal",
            "m-too-large",
            S1,
            vec![7u8; MAX_PAYLOAD + 1],
        ),
    )
    .await;
    check("send_payload_too_large", render_ack(ack), &mut failures);
    // Unauthenticated and oversized: the shape check runs first on gRPC.
    let ack = send(
        &mut client,
        None,
        env(
            DECISION,
            "Proposal",
            "m-too-large-anon",
            S1,
            vec![7u8; MAX_PAYLOAD + 1],
        ),
    )
    .await;
    check(
        "send_payload_too_large_unauthenticated",
        render_ack(ack),
        &mut failures,
    );

    // ── missing envelope ──
    let status = client
        .send(with_token(Some("tok-peer"), SendRequest { envelope: None }))
        .await
        .expect_err("an envelope-less Send is a status");
    check(
        "send_missing_envelope",
        render_status(&status),
        &mut failures,
    );

    // ── policy-engine denial ──
    let ack = send(
        &mut engine_client,
        Some("tok-denied"),
        env(
            DECISION,
            "SessionStart",
            "m-engine-start",
            UNKNOWN,
            start_payload(&[DENIED]),
        ),
    )
    .await;
    check("send_policy_engine_denied", render_ack(ack), &mut failures);
    let status = get_session_err(&mut engine_client, Some("tok-denied"), S4).await;
    check(
        "get_session_policy_engine_denied",
        render_status(&status),
        &mut failures,
    );
    let (_tx, mut stream) = open_subscribe(&mut engine_client, "tok-denied", S4, 0).await;
    let frame = next_frame(&mut stream).await.expect("an inline frame");
    check(
        "subscribe_policy_engine_denied",
        render_frame(&frame),
        &mut failures,
    );

    // ── unknown session ──
    let ack = send(
        &mut client,
        Some("tok-peer"),
        env(
            DECISION,
            "Proposal",
            "m-unknown",
            UNKNOWN,
            decision_proposal("p1"),
        ),
    )
    .await;
    check("send_unknown_session", render_ack(ack), &mut failures);
    let status = get_session_err(&mut client, Some("tok-peer"), UNKNOWN).await;
    check(
        "get_session_unknown_session",
        render_status(&status),
        &mut failures,
    );
    let (_tx, mut stream) = open_subscribe(&mut client, "tok-peer", UNKNOWN, 0).await;
    let status = next_frame(&mut stream).await.expect_err("terminal status");
    check(
        "subscribe_unknown_session",
        render_status(&status),
        &mut failures,
    );

    // ── forbidden subscriber (inline, stream stays open) ──
    let (_tx, mut stream) = open_subscribe(&mut client, "tok-outsider", S1, 0).await;
    let frame = next_frame(&mut stream).await.expect("an inline frame");
    check("subscribe_forbidden", render_frame(&frame), &mut failures);
    let status = get_session_err(&mut client, Some("tok-outsider"), S1).await;
    check(
        "get_session_forbidden",
        render_status(&status),
        &mut failures,
    );

    // ── compacted resume: SessionStart(1) .. Commitment(5) compacts to base 5 ──
    send_ok(
        &mut file_client,
        "tok-orch",
        env(
            PROPOSAL,
            "SessionStart",
            "s2-start",
            S2,
            start_payload(&[ORCH, PEER]),
        ),
    )
    .await;
    let proposal = macp_runtime::proposal_pb::ProposalPayload {
        proposal_id: "p1".into(),
        title: "offer".into(),
        summary: "terms".into(),
        details: vec![],
        tags: vec![],
    }
    .encode_to_vec();
    let accept = macp_runtime::proposal_pb::AcceptPayload {
        proposal_id: "p1".into(),
        reason: "ok".into(),
    }
    .encode_to_vec();
    send_ok(
        &mut file_client,
        "tok-peer",
        env(PROPOSAL, "Proposal", "s2-proposal", S2, proposal),
    )
    .await;
    send_ok(
        &mut file_client,
        "tok-peer",
        env(PROPOSAL, "Accept", "s2-accept-peer", S2, accept.clone()),
    )
    .await;
    send_ok(
        &mut file_client,
        "tok-orch",
        env(PROPOSAL, "Accept", "s2-accept-orch", S2, accept),
    )
    .await;
    let commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "proposal.accepted".into(),
        authority_scope: "commercial".into(),
        reason: "bound".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy.default".into(),
        configuration_version: "config.default".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();
    send_ok(
        &mut file_client,
        "tok-orch",
        env(PROPOSAL, "Commitment", "s2-commit", S2, commitment),
    )
    .await;
    let (_tx, mut stream) = open_subscribe(&mut file_client, "tok-orch", S2, 0).await;
    let status = next_frame(&mut stream).await.expect_err("terminal status");
    check(
        "subscribe_compacted_resume",
        render_status(&status),
        &mut failures,
    );

    // ── lagged stream ──
    lagged_stream(&mut client, &runtime, &mut failures).await;

    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Deterministic lag: the subscriber's receiver exists (the SessionStart
/// replay frame has arrived), then `BUS_CAPACITY + LAG_EXTRA` envelopes are
/// accepted inside `tokio::task::unconstrained` on this current-thread
/// runtime. With an in-memory backend and no lock contention, `process`
/// never yields, so the server's stream task cannot run until all of them are
/// published: the receiver is exactly `LAG_EXTRA` behind, whichever of the
/// stream's two receive paths observes it.
async fn lagged_stream(
    client: &mut MacpRuntimeServiceClient<Channel>,
    runtime: &Arc<Runtime>,
    failures: &mut Vec<String>,
) {
    send_ok(
        client,
        "tok-orch",
        env(
            DECISION,
            "SessionStart",
            "s3-start",
            S3,
            start_payload(&[ORCH, PEER]),
        ),
    )
    .await;
    let (_tx, mut stream) = open_subscribe(client, "tok-orch", S3, 0).await;
    let first = next_frame(&mut stream)
        .await
        .expect("the SessionStart replay");
    assert!(matches!(
        first.response,
        Some(stream_session_response::Response::Envelope(ref e)) if e.message_id == "s3-start"
    ));
    let runtime = runtime.clone();
    tokio::task::unconstrained(async move {
        for i in 0..BUS_CAPACITY + LAG_EXTRA {
            let mut e = env(
                DECISION,
                "Proposal",
                &format!("lag-{i}"),
                S3,
                decision_proposal(&format!("p{i}")),
            );
            e.sender = PEER.into();
            runtime.process(&e, None).await.expect("accepted");
        }
    })
    .await;
    let status = next_frame(&mut stream).await.expect_err("STREAM_LAGGED");
    check("stream_lagged", render_status(&status), failures);
}
