//! Replay is identical with telemetry on and off (R2 P4, AC 5).
//!
//! The same scripted session runs twice through a real tonic server, with a
//! `ManualClock` and a `FileBackend` in its own temporary directory: once on
//! a runtime with `Telemetry::disabled()`, once with active telemetry
//! exporting to an in-memory span exporter (so the run really records
//! `process` spans, an anchor, links and transition spans). The durable
//! artifacts and their replay must not differ by a byte: the anchor and
//! everything else trace-related stays out of the log, the snapshot and the
//! session state.

#![cfg(feature = "otel")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};

use macp_runtime::decision_pb::{ProposalPayload, VotePayload};
use macp_runtime::log_store::{LogEntry, LogStore};
use macp_runtime::macp_core::ManualClock;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    CommitmentPayload, Envelope, ResumeSessionRequest, SendRequest, SessionStartPayload,
    SuspendSessionRequest,
};
use macp_runtime::registry::{PersistedSession, SessionRegistry};
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::storage::{FileBackend, StorageBackend};
use macp_runtime::telemetry::{grpc, init, Telemetry};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use prost::Message;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const DECISION: &str = "macp.mode.decision.v1";
const SID: &str = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f60";
const NOW: i64 = 1_800_000_000_000;
const INITIATOR: &str = "agent://a";
const PEER: &str = "agent://b";
const OUTSIDER: &str = "agent://c";

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

fn envelope(message_type: &str, message_id: &str, payload: Vec<u8>) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: DECISION.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: SID.into(),
        sender: String::new(),
        timestamp_unix_ms: NOW,
        payload,
    }
}

fn authed<T>(msg: T, sender: &str) -> tonic::Request<T> {
    let mut req = tonic::Request::new(msg);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {sender}").parse().unwrap());
    req
}

async fn send(channel: &Channel, sender: &str, env: Envelope) {
    MacpRuntimeServiceClient::new(channel.clone())
        .send(authed(
            SendRequest {
                envelope: Some(env),
            },
            sender,
        ))
        .await
        .unwrap();
}

/// The scripted session: start, proposal, a duplicate, a rejected vote,
/// a vote, suspend and resume, then a Commitment that resolves it.
async fn script(channel: &Channel, clock: &ManualClock) {
    let start = SessionStartPayload {
        intent: "intent".into(),
        participants: vec![INITIATOR.into(), PEER.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: 60_000,
        extensions: HashMap::new(),
        ..Default::default()
    }
    .encode_to_vec();
    send(channel, INITIATOR, envelope("SessionStart", "m1", start)).await;
    clock.advance(100);
    let proposal = ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "why".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    send(
        channel,
        INITIATOR,
        envelope("Proposal", "m2", proposal.clone()),
    )
    .await;
    send(channel, INITIATOR, envelope("Proposal", "m2", proposal)).await;
    let vote = VotePayload {
        proposal_id: "p1".into(),
        vote: "APPROVE".into(),
        reason: "ok".into(),
    }
    .encode_to_vec();
    send(channel, OUTSIDER, envelope("Vote", "m3", vote.clone())).await;
    clock.advance(100);
    send(channel, PEER, envelope("Vote", "m4", vote)).await;
    clock.advance(100);
    let mut client = MacpRuntimeServiceClient::new(channel.clone());
    client
        .suspend_session(authed(
            SuspendSessionRequest {
                session_id: SID.into(),
                reason: "pause".into(),
            },
            INITIATOR,
        ))
        .await
        .unwrap();
    clock.advance(1_000);
    client
        .resume_session(authed(
            ResumeSessionRequest {
                session_id: SID.into(),
                reason: "go".into(),
            },
            INITIATOR,
        ))
        .await
        .unwrap();
    clock.advance(100);
    let commitment = CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "test".into(),
        reason: "done".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy.default".into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec();
    send(channel, INITIATOR, envelope("Commitment", "m5", commitment)).await;
}

/// Run the script on a fresh runtime over `dir` and return the runtime.
async fn run(dir: &Path, telemetry: Telemetry, compact: bool) -> Arc<Runtime> {
    let clock = Arc::new(ManualClock::new(NOW));
    let storage: Arc<dyn StorageBackend> = Arc::new(FileBackend::new(dir.to_path_buf()).unwrap());
    let runtime = Arc::new(
        Runtime::new(
            storage,
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
        )
        .with_clock(Arc::clone(&clock) as _)
        .with_compact_terminal_logs(compact)
        .with_telemetry(telemetry),
    );
    let svc = MacpServer::new(Arc::clone(&runtime), SecurityLayer::dev_mode());
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    let router = Server::builder()
        .layer(grpc::server_layer())
        .add_service(MacpRuntimeServiceServer::new(svc));
    tokio::spawn(router.serve_with_incoming(incoming));
    let channel = tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap();
    script(&channel, &clock).await;
    runtime
}

fn session_dir(base: &Path) -> PathBuf {
    base.join("sessions").join(SID)
}

fn load_log(dir: &Path) -> Vec<LogEntry> {
    std::fs::read_to_string(dir.join("log.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `PersistedSession` with its one unordered field put in a fixed order:
/// `seen_message_ids` is collected from a `HashSet`, whose iteration order
/// follows a per-instance random hash seed. Two runs with telemetry **off**
/// already differ there, so it is the one thing not compared byte for byte.
fn canonical(session: &macp_runtime::session::Session) -> Vec<u8> {
    let mut p = PersistedSession::from(session);
    p.seen_message_ids.sort();
    serde_json::to_vec(&p).unwrap()
}

/// A serialized `PersistedSession` as its raw JSON value, with only the
/// top-level `seen_message_ids` array sorted. No struct round-trip: every
/// key present (an unknown extra one included) and every value is compared
/// as written.
fn canonical_snapshot(bytes: &[u8]) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let map = v.as_object_mut().expect("a JSON object");
    if let Some(serde_json::Value::Array(ids)) = map.get_mut("seen_message_ids") {
        ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    }
    v
}

/// [`canonical_snapshot`] of both sides, plus their raw key sets.
fn assert_snapshots_identical(off: &[u8], on: &[u8], what: &str) {
    let (off, on) = (canonical_snapshot(off), canonical_snapshot(on));
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(keys(&off), keys(&on), "{what}: key sets differ");
    assert_eq!(off, on, "{what}: content differs");
}

/// Compare two `log.jsonl` files: every line byte-identical, except a
/// checkpoint's payload (a serialized `PersistedSession`), compared through
/// [`canonical_snapshot`] with every other field of the line byte-identical.
///
/// With `compacted`, the checkpoint is the one terminal compaction wrote,
/// which `macp-storage`'s `compaction.rs` stamps with the wall clock rather
/// than the runtime's injected clock (pre-existing, unrelated to telemetry),
/// so its two timestamps are left out of the comparison.
fn assert_logs_identical(off: &[u8], on: &[u8], compacted: bool) {
    let (off, on) = (
        std::str::from_utf8(off).unwrap(),
        std::str::from_utf8(on).unwrap(),
    );
    let (off, on): (Vec<&str>, Vec<&str>) = (off.lines().collect(), on.lines().collect());
    assert_eq!(off.len(), on.len(), "same number of entries");
    for (i, (a, b)) in off.iter().zip(&on).enumerate() {
        if a == b {
            continue;
        }
        let (mut ea, mut eb): (LogEntry, LogEntry) = (
            serde_json::from_str(a).unwrap(),
            serde_json::from_str(b).unwrap(),
        );
        assert_eq!(
            format!("{:?}", ea.entry_kind),
            "Checkpoint",
            "entry {i} differs and is not a checkpoint:\n{a}\n{b}"
        );
        assert_snapshots_identical(&ea.raw_payload, &eb.raw_payload, &format!("checkpoint {i}"));
        ea.raw_payload.clear();
        eb.raw_payload.clear();
        if compacted {
            for e in [&mut ea, &mut eb] {
                e.received_at_ms = 0;
                e.timestamp_unix_ms = 0;
            }
        }
        assert_eq!(
            serde_json::to_string(&ea).unwrap(),
            serde_json::to_string(&eb).unwrap(),
            "checkpoint {i} envelope differs"
        );
    }
}

/// Every file under `dir`, relative, sorted.
fn files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Compaction off: the terminal checkpoint is appended on the runtime's own
/// clock, so the whole log compares strictly.
#[tokio::test]
async fn replay_is_identical_with_telemetry_on_and_off() {
    check_equivalence(false).await;
}

/// Compaction on (the default): the log is replaced by one checkpoint.
#[tokio::test]
async fn replay_is_identical_with_telemetry_on_and_off_after_compaction() {
    check_equivalence(true).await;
}

async fn check_equivalence(compact: bool) {
    install_propagator();
    let off_dir = tempfile::tempdir().unwrap();
    let on_dir = tempfile::tempdir().unwrap();

    let off = run(off_dir.path(), Telemetry::disabled(), compact).await;

    let spans = InMemorySpanExporter::default();
    let tracer = SdkTracerProvider::builder()
        .with_simple_exporter(spans.clone())
        .build();
    let (dispatch, telemetry) = init::active_for_tests(&tracer, None);
    let on = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        run(on_dir.path(), telemetry, compact).await
    };
    // The active run really traced: an anchor, links and a resolve span.
    tracer.force_flush().unwrap();
    let exported = spans.get_finished_spans().unwrap();
    assert!(exported.iter().any(|s| s.name.starts_with("resolve ")));
    assert!(exported.iter().any(|s| !s.links.is_empty()));
    assert_eq!(on.trace_anchor_count(), 1);
    assert_eq!(off.trace_anchor_count(), 0);

    let (off_dir, on_dir) = (session_dir(off_dir.path()), session_dir(on_dir.path()));
    // Same files, and nothing trace-related among them.
    assert_eq!(files(&off_dir), files(&on_dir));
    for name in files(&on_dir) {
        assert!(
            !name.contains("trace") && !name.contains("anchor"),
            "{name}"
        );
        let bytes = std::fs::read(on_dir.join(&name)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("trace"), "{name} mentions trace: {text}");
    }

    // log.jsonl: every entry byte-identical, a checkpoint up to the set
    // order of its embedded snapshot.
    let (off_log, on_log) = (
        std::fs::read(off_dir.join("log.jsonl")).unwrap(),
        std::fs::read(on_dir.join("log.jsonl")).unwrap(),
    );
    assert!(!off_log.is_empty());
    assert_logs_identical(&off_log, &on_log, compact);
    // session.json: identical up to the same set order.
    let (off_snap, on_snap) = (
        std::fs::read(off_dir.join("session.json")).unwrap(),
        std::fs::read(on_dir.join("session.json")).unwrap(),
    );
    assert_snapshots_identical(&off_snap, &on_snap, "session.json");

    // Replaying each log gives the same PersistedSession bytes.
    let modes = ModeRegistry::build_default(Arc::new(macp_runtime::policy::DefaultPolicyEvaluator));
    let off_replayed =
        macp_runtime::replay::replay_session(SID, &load_log(&off_dir), &modes, None).unwrap();
    let on_replayed =
        macp_runtime::replay::replay_session(SID, &load_log(&on_dir), &modes, None).unwrap();
    assert_eq!(canonical(&off_replayed), canonical(&on_replayed));
    assert_eq!(
        off_replayed.state,
        macp_runtime::session::SessionState::Resolved
    );
}
