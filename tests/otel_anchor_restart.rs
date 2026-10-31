//! The durable session anchor (R2 P5): the anchor survives a restart through
//! the storage backend's opaque trace-anchor record, so spans after recovery
//! (later `process` spans and the root `expire`) still link to the
//! pre-restart SessionStart span.
//!
//! Each runtime runs behind a real tonic server built as `src/main.rs` builds
//! it, over a `FileBackend` in a temporary directory, with a `ManualClock`.
//! The "restart" drops the first runtime and recovers a second one from the
//! same directory with `recovery::recover_sessions`, as `main` does.

#![cfg(feature = "otel")]

#[path = "support/counting_backend.rs"]
mod counting_backend;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use counting_backend::Counting;
use macp_runtime::decision_pb::ProposalPayload;
use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::ManualClock;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    Ack, CommitmentPayload, Envelope, SendRequest, SessionStartPayload, StreamSessionRequest,
};
use macp_runtime::policy::registry::PolicyRegistry;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::storage::{FileBackend, MemoryBackend, StorageBackend};
use macp_runtime::telemetry::{grpc, init, Telemetry};
use opentelemetry::trace::{SpanId, TraceId};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use prost::Message;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const DECISION: &str = "macp.mode.decision.v1";
const NOW: i64 = 1_800_000_000_000;
const TTL_MS: i64 = 60_000;
const INITIATOR: &str = "agent://a";
const PEER: &str = "agent://b";

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// One runtime generation: its spans, runtime, clock and channel.
struct Node {
    spans: InMemorySpanExporter,
    tracer: SdkTracerProvider,
    runtime: Arc<Runtime>,
    clock: Arc<ManualClock>,
    channel: Channel,
    _guard: Option<tracing::dispatcher::DefaultGuard>,
}

impl Node {
    /// Recover whatever `storage` holds (as `main` does), then serve it.
    async fn boot(storage: Arc<dyn StorageBackend>, active: bool, now: i64) -> Self {
        install_propagator();
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let (telemetry, guard) = if active {
            let (dispatch, telemetry) = init::active_for_tests(&tracer, None);
            (telemetry, Some(tracing::dispatcher::set_default(&dispatch)))
        } else {
            (Telemetry::disabled(), None)
        };
        let registry = Arc::new(SessionRegistry::new());
        let log_store = Arc::new(LogStore::new());
        let modes = Arc::new(ModeRegistry::build_default(Arc::new(
            macp_runtime::policy::DefaultPolicyEvaluator,
        )));
        let policies = Arc::new(PolicyRegistry::new());
        macp_runtime::recovery::recover_sessions(
            storage.as_ref(),
            &registry,
            &log_store,
            &modes,
            &policies,
            true,
        )
        .await
        .unwrap();
        let clock = Arc::new(ManualClock::new(now));
        let runtime = Arc::new(
            Runtime::with_registries(storage, registry, log_store, modes, policies)
                .with_clock(Arc::clone(&clock) as _)
                .with_telemetry(telemetry),
        );
        let channel = serve(Arc::clone(&runtime)).await;
        Self {
            spans,
            tracer,
            runtime,
            clock,
            channel,
            _guard: guard,
        }
    }

    fn all(&self) -> Vec<SpanData> {
        self.tracer.force_flush().unwrap();
        self.spans.get_finished_spans().unwrap()
    }

    async fn send(&self, env: Envelope) -> Ack {
        let mut req = tonic::Request::new(SendRequest {
            envelope: Some(env),
        });
        req.metadata_mut().insert(
            "authorization",
            format!("Bearer {INITIATOR}").parse().unwrap(),
        );
        MacpRuntimeServiceClient::new(self.channel.clone())
            .send(req)
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap()
    }

    /// The span id of `sid`'s accepted SessionStart `process` span.
    fn anchor_of(&self, sid: &str) -> SpanId {
        self.all()
            .iter()
            .find(|s| {
                s.name.starts_with("process")
                    && attr(s, "macp.session.id").as_deref() == Some(sid)
                    && attr(s, "macp.message.type").as_deref() == Some("SessionStart")
                    && attr(s, "macp.envelope.outcome").as_deref() == Some("accepted")
            })
            .unwrap_or_else(|| panic!("no accepted SessionStart span for {sid}"))
            .span_context
            .span_id()
    }

    /// The `expire` span of `sid`.
    fn expire_of(&self, sid: &str) -> SpanData {
        self.all()
            .into_iter()
            .find(|s| {
                s.name.starts_with("expire ") && attr(s, "macp.session.id").as_deref() == Some(sid)
            })
            .unwrap_or_else(|| panic!("no expire span for {sid}"))
    }

    /// The `process` span of the envelope `message_id`.
    fn process_of(&self, message_id: &str) -> SpanData {
        self.all()
            .into_iter()
            .find(|s| {
                s.name.starts_with("process")
                    && attr(s, "macp.message.id").as_deref() == Some(message_id)
            })
            .unwrap_or_else(|| panic!("no process span for {message_id}"))
    }
}

async fn serve(runtime: Arc<Runtime>) -> Channel {
    let svc = MacpServer::new(runtime, SecurityLayer::dev_mode());
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    let router = Server::builder()
        .layer(grpc::server_layer())
        .add_service(MacpRuntimeServiceServer::new(svc));
    tokio::spawn(router.serve_with_incoming(incoming));
    tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

fn attr(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.as_str().into_owned())
}

fn sid(n: u32) -> String {
    format!("0b7c1a3e-2f4d-4c6b-9a8e-{n:012x}")
}

fn envelope(message_type: &str, message_id: &str, sid: &str, payload: Vec<u8>) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: DECISION.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: sid.into(),
        sender: String::new(),
        timestamp_unix_ms: NOW,
        payload,
    }
}

fn start(sid: &str) -> Envelope {
    let payload = SessionStartPayload {
        intent: "intent".into(),
        participants: vec![INITIATOR.into(), PEER.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: TTL_MS,
        extensions: HashMap::new(),
        ..Default::default()
    }
    .encode_to_vec();
    envelope("SessionStart", &format!("{sid}-start"), sid, payload)
}

fn proposal(message_id: &str, sid: &str) -> Envelope {
    let payload = ProposalPayload {
        proposal_id: message_id.into(),
        option: "deploy".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    envelope("Proposal", message_id, sid, payload)
}

fn commitment(message_id: &str, sid: &str) -> Envelope {
    let payload = CommitmentPayload {
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
    envelope("Commitment", message_id, sid, payload)
}

/// Poll the backend until the spawned anchor write for `sid` landed.
async fn wait_for_stored_anchor(storage: &dyn StorageBackend, sid: &str) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(bytes) = storage.load_trace_anchor(sid).await.unwrap() {
            return bytes;
        }
        assert!(
            Instant::now() < deadline,
            "the anchor of {sid} was never stored"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn session_files(base: &Path, sid: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(base.join("sessions").join(sid))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

// The anchor written before a restart is loaded after it: a later `process`
// span and the TTL `expire` root both link to the pre-restart SessionStart.
#[tokio::test]
async fn links_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let (live, expiring) = (sid(1), sid(2));

    let before = Node::boot(Arc::clone(&storage), true, NOW).await;
    for s in [&live, &expiring] {
        assert!(before.send(start(s)).await.ok);
    }
    let (live_anchor, expiring_anchor) = (before.anchor_of(&live), before.anchor_of(&expiring));
    for (s, anchor) in [(&live, live_anchor), (&expiring, expiring_anchor)] {
        let record = wait_for_stored_anchor(storage.as_ref(), s).await;
        let text = String::from_utf8(record).unwrap();
        assert!(text.starts_with(r#"{"v":1,"traceparent":"00-"#), "{text}");
        assert!(text.contains(&anchor.to_string()), "{text}");
        assert!(!text.contains("tracestate"));
    }
    let pre_trace = before
        .all()
        .iter()
        .find(|s| s.span_context.span_id() == live_anchor)
        .unwrap()
        .span_context
        .trace_id();
    drop(before);

    // Restart: a new runtime over the same directory, its cache empty.
    let counting = Counting::new(Arc::clone(&storage));
    let after = Node::boot(counting.clone(), true, NOW + 1_000).await;
    assert_eq!(after.runtime.trace_anchor_count(), 0, "loaded lazily");

    // A later message of the live session links to the pre-restart anchor.
    after.send(proposal("p-after", &live)).await;
    after.send(proposal("p-after-2", &live)).await;
    for message_id in ["p-after", "p-after-2"] {
        let process = after.process_of(message_id);
        assert_eq!(process.links.len(), 1, "{message_id}");
        assert_eq!(process.links[0].span_context.span_id(), live_anchor);
        assert_eq!(process.links[0].span_context.trace_id(), pre_trace);
    }

    // The TTL lapses: the sweep's root `expire` links to the pre-restart
    // anchor of the expiring session.
    after.clock.set(NOW + TTL_MS + 2_500);
    after.runtime.cleanup_expired_sessions().await;
    let expire = after.expire_of(&expiring);
    assert_eq!(expire.parent_span_id, SpanId::INVALID, "expire is a root");
    assert_eq!(expire.links.len(), 1);
    assert_eq!(expire.links[0].span_context.span_id(), expiring_anchor);

    // One storage read per resident session, then the cache serves it.
    assert_eq!(counting.loads(), 2);
    assert_eq!(after.runtime.trace_anchor_count(), 2);
}

// Whole-feature seam (P2 + P3 + P4 + P5): after a restart, a Commitment sent
// on a StreamSession carrying the client's `traceparent` produces the full
// chain client -> SERVER (stream open) -> `process` -> `resolve`, and both the
// `process` and the `resolve` span link to the pre-restart SessionStart anchor
// loaded from storage. Each phase's own tests cover one link of this chain on
// one path (unary, or no restart); none covers them together.
#[tokio::test]
async fn stream_commitment_after_a_restart_chains_and_links_the_stored_anchor() {
    const CLIENT_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const CLIENT_SPAN: &str = "00f067aa0ba902b7";
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let s = sid(7);

    let before = Node::boot(Arc::clone(&storage), true, NOW).await;
    assert!(before.send(start(&s)).await.ok);
    assert!(before.send(proposal("p-before", &s)).await.ok);
    let anchor = before.anchor_of(&s);
    wait_for_stored_anchor(storage.as_ref(), &s).await;
    drop(before);

    let after = Node::boot(Arc::clone(&storage), true, NOW + 1_000).await;
    assert_eq!(after.runtime.trace_anchor_count(), 0, "loaded lazily");

    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut req = tonic::Request::new(tokio_stream::wrappers::ReceiverStream::new(rx));
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {INITIATOR}").parse().unwrap(),
    );
    req.metadata_mut().insert(
        "traceparent",
        format!("00-{CLIENT_TRACE}-{CLIENT_SPAN}-01")
            .parse()
            .unwrap(),
    );
    let mut inbound = MacpRuntimeServiceClient::new(after.channel.clone())
        .stream_session(req)
        .await
        .unwrap()
        .into_inner();
    tx.send(StreamSessionRequest {
        envelope: Some(commitment("c-after", &s)),
        ..Default::default()
    })
    .await
    .unwrap();
    use macp_runtime::pb::stream_session_response::Response;
    match inbound.message().await.unwrap().unwrap().response {
        Some(Response::Envelope(e)) => assert_eq!(e.message_id, "c-after"),
        other => panic!("expected the echoed Commitment, got {other:?}"),
    }
    drop(tx);
    while let Ok(Some(_)) = inbound.message().await {}

    // The SERVER span ends with the stream; wait for it to be exported.
    let deadline = Instant::now() + Duration::from_secs(5);
    let server = loop {
        if let Some(span) = after
            .all()
            .into_iter()
            .find(|s| s.name == "macp.v1.MACPRuntimeService/StreamSession")
        {
            break span;
        }
        assert!(Instant::now() < deadline, "no StreamSession SERVER span");
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let client_trace = TraceId::from_hex(CLIENT_TRACE).unwrap();
    assert_eq!(server.span_context.trace_id(), client_trace);
    assert_eq!(
        server.parent_span_id,
        SpanId::from_hex(CLIENT_SPAN).unwrap()
    );

    let process = after.process_of("c-after");
    assert_eq!(
        attr(&process, "macp.envelope.outcome").as_deref(),
        Some("accepted")
    );
    assert_eq!(process.parent_span_id, server.span_context.span_id());
    assert_eq!(process.span_context.trace_id(), client_trace);
    assert_eq!(process.links.len(), 1);
    assert_eq!(process.links[0].span_context.span_id(), anchor);

    let resolve = after
        .all()
        .into_iter()
        .find(|sp| {
            sp.name.starts_with("resolve ")
                && attr(sp, "macp.session.id").as_deref() == Some(s.as_str())
        })
        .expect("a resolve span");
    assert_eq!(resolve.parent_span_id, process.span_context.span_id());
    assert_eq!(resolve.span_context.trace_id(), client_trace);
    assert_eq!(resolve.links.len(), 1);
    assert_eq!(resolve.links[0].span_context.span_id(), anchor);
    assert_eq!(
        attr(&resolve, "macp.session.state").as_deref(),
        Some("SESSION_STATE_RESOLVED")
    );
    assert!(attr(&resolve, "macp.commitment.hash").is_some_and(|h| h.starts_with("sha256:")));
}

// A corrupt or unknown-version anchor record is ignored: no link, no error,
// the session works, and the miss is cached (one read, not one per span).
#[tokio::test]
async fn a_corrupt_anchor_record_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let (garbage, future) = (sid(1), sid(2));
    let before = Node::boot(Arc::clone(&storage), true, NOW).await;
    for s in [&garbage, &future] {
        assert!(before.send(start(s)).await.ok);
        wait_for_stored_anchor(storage.as_ref(), s).await;
    }
    drop(before);
    let anchor_file = |s: &str| dir.path().join("sessions").join(s).join("anchor.json");
    std::fs::write(anchor_file(&garbage), b"\x00not json{").unwrap();
    std::fs::write(
        anchor_file(&future),
        br#"{"v":2,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}"#,
    )
    .unwrap();

    let counting = Counting::new(Arc::clone(&storage));
    let after = Node::boot(counting.clone(), true, NOW + 1_000).await;
    for s in [&garbage, &future] {
        assert!(after.send(proposal(&format!("{s}-p1"), s)).await.ok);
        assert!(after.send(proposal(&format!("{s}-p2"), s)).await.ok);
        for n in ["p1", "p2"] {
            assert!(after.process_of(&format!("{s}-{n}")).links.is_empty());
        }
    }
    after.clock.set(NOW + TTL_MS + 2_500);
    after.runtime.cleanup_expired_sessions().await;
    for s in [&garbage, &future] {
        assert!(after.expire_of(s).links.is_empty(), "{s}");
    }
    assert_eq!(counting.loads(), 2, "one read per session, misses cached");
}

// A session started while telemetry was inactive has no stored anchor and
// gets no link after a restart with telemetry active, for one storage read.
#[tokio::test]
async fn a_pre_telemetry_session_costs_one_read() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let s = sid(1);
    let before = Node::boot(Arc::clone(&storage), false, NOW).await;
    assert!(before.send(start(&s)).await.ok);
    drop(before);
    assert_eq!(
        session_files(dir.path(), &s),
        ["log.jsonl", "session.json"],
        "inactive telemetry writes no anchor"
    );

    let counting = Counting::new(Arc::clone(&storage));
    let after = Node::boot(counting.clone(), true, NOW + 1_000).await;
    for n in 0..20 {
        after.send(proposal(&format!("p{n}"), &s)).await;
    }
    assert!(after.process_of("p0").links.is_empty());
    assert_eq!(counting.loads(), 1);
}

// With telemetry inactive no anchor is ever written, on any message.
#[tokio::test]
async fn inactive_telemetry_writes_no_anchor_file() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let node = Node::boot(Arc::clone(&storage), false, NOW).await;
    let s = sid(1);
    assert!(node.send(start(&s)).await.ok);
    assert!(node.send(proposal("p", &s)).await.ok);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(session_files(dir.path(), &s), ["log.jsonl", "session.json"]);
    assert_eq!(storage.load_trace_anchor(&s).await.unwrap(), None);
}

// Disk GC deletes the anchor with its session: nothing is left behind, and a
// later lookup reads nothing.
#[tokio::test]
async fn disk_gc_removes_the_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let node = Node::boot(Arc::clone(&storage), true, NOW).await;
    let (collected, kept) = (sid(1), sid(2));
    for s in [&collected, &kept] {
        assert!(node.send(start(s)).await.ok);
        wait_for_stored_anchor(storage.as_ref(), s).await;
    }
    // Both expire; only `collected` is past retention when GC runs first.
    node.clock.set(NOW + TTL_MS + 1);
    node.runtime.cleanup_expired_sessions().await;
    assert_eq!(
        node.runtime.gc_disk_sessions(3_600).await,
        0,
        "within retention"
    );
    assert!(storage.load_trace_anchor(&kept).await.unwrap().is_some());
    node.clock.set(NOW + 1_000_000);
    let removed = node.runtime.gc_disk_sessions(1).await;
    assert_eq!(removed, 2);
    for s in [&collected, &kept] {
        assert_eq!(storage.load_trace_anchor(s).await.unwrap(), None);
        assert!(!dir.path().join("sessions").join(s).exists());
    }
    assert_eq!(node.runtime.trace_anchor_count(), 0);
    let leftovers: Vec<_> = walk(dir.path())
        .into_iter()
        .filter(|p| p.contains("anchor"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path.to_string_lossy().into_owned());
    }
    out
}

// MACP_MEMORY_ONLY: the memory backend keeps no anchor; it lives in the
// runtime's cache only, links still work within the process, and nothing
// errors.
#[tokio::test]
async fn memory_only_keeps_the_anchor_in_memory() {
    let node = Node::boot(Arc::new(MemoryBackend), true, NOW).await;
    let s = sid(1);
    assert!(node.send(start(&s)).await.ok);
    assert!(node.send(proposal("p", &s)).await.ok);
    let anchor = node.anchor_of(&s);
    let process = node.process_of("p");
    assert_eq!(process.links.len(), 1);
    assert_eq!(process.links[0].span_context.span_id(), anchor);
    assert_eq!(node.runtime.trace_anchor_count(), 1);
    assert_eq!(
        node.runtime.storage.load_trace_anchor(&s).await.unwrap(),
        None
    );
}

// The anchor record is stored only after the session snapshot, so a stored
// anchor always has a stored session and the startup orphan sweep (which
// deletes anchors whose session is absent) cannot delete a live one.
#[tokio::test]
async fn the_anchor_is_stored_only_after_the_session_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let file: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let counting = Counting::new(file);
    let node = Node::boot(counting.clone(), true, NOW).await;
    let s = sid(1);
    assert!(node.send(start(&s)).await.ok);
    wait_for_stored_anchor(counting.as_ref(), &s).await;
    let writes = counting.writes();
    let save = writes.iter().position(|w| *w == format!("save:{s}"));
    let anchor = writes.iter().position(|w| *w == format!("anchor:{s}"));
    assert!(
        matches!((save, anchor), (Some(a), Some(b)) if a < b),
        "{writes:?}"
    );
}

// A backend whose trace-anchor store and load both fail: every message is
// still accepted, nothing is rolled back, and transitions still emit their
// spans (without a link once the in-memory anchor is gone after a restart).
#[tokio::test]
async fn anchor_storage_failures_never_fail_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let file: Arc<dyn StorageBackend> =
        Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    let (expiring, cancelled) = (sid(1), sid(2));

    let failing = Counting::failing_anchors(Arc::clone(&file));
    let before = Node::boot(failing.clone(), true, NOW).await;
    for s in [&expiring, &cancelled] {
        assert!(before.send(start(s)).await.ok, "{s}");
        assert!(before.send(proposal(&format!("{s}-p1"), s)).await.ok);
    }
    // The spawned stores ran and failed.
    let deadline = Instant::now() + Duration::from_secs(5);
    while failing.stores() < 2 {
        assert!(Instant::now() < deadline, "anchor stores never attempted");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // In-process links still work from the in-memory anchor.
    assert_eq!(before.process_of(&format!("{expiring}-p1")).links.len(), 1);
    drop(before);
    for s in [&expiring, &cancelled] {
        assert_eq!(session_files(dir.path(), s), ["log.jsonl", "session.json"]);
    }

    // Restart on the same failing backend: every anchor load errors.
    let failing = Counting::failing_anchors(Arc::clone(&file));
    let after = Node::boot(failing.clone(), true, NOW + 1_000).await;
    for s in [&expiring, &cancelled] {
        let ack = after.send(proposal(&format!("{s}-p2"), s)).await;
        assert!(ack.ok, "{ack:?}");
        assert!(after.process_of(&format!("{s}-p2")).links.is_empty());
    }
    let mut req = tonic::Request::new(macp_runtime::pb::CancelSessionRequest {
        session_id: cancelled.clone(),
        reason: "stop".into(),
    });
    req.metadata_mut().insert(
        "authorization",
        format!("Bearer {INITIATOR}").parse().unwrap(),
    );
    MacpRuntimeServiceClient::new(after.channel.clone())
        .cancel_session(req)
        .await
        .unwrap();
    after.clock.set(NOW + TTL_MS + 2_500);
    after.runtime.cleanup_expired_sessions().await;

    let spans = after.all();
    for (op, s) in [("expire ", &expiring), ("cancel ", &cancelled)] {
        let span = spans
            .iter()
            .find(|x| x.name.starts_with(op) && attr(x, "macp.session.id").as_deref() == Some(s))
            .unwrap_or_else(|| panic!("no {op}span for {s}"));
        assert!(span.links.is_empty(), "{op}");
    }
    // Nothing rolled back: every accepted message stays accepted (the
    // terminal log is compacted, so read the session's accepted ids) and
    // the sessions reached their terminal states, durably.
    for (s, state) in [
        (&expiring, macp_runtime::session::SessionState::Expired),
        (&cancelled, macp_runtime::session::SessionState::Cancelled),
    ] {
        let shared = after.runtime.registry.get_shared(s).await.unwrap();
        let session = shared.lock().await;
        assert_eq!(session.state, state);
        for id in [format!("{s}-start"), format!("{s}-p1"), format!("{s}-p2")] {
            assert!(session.seen_message_ids.contains(&id), "{id} accepted");
        }
        assert!(!file.load_log(s).await.unwrap().is_empty());
        assert_eq!(file.load_session(s).await.unwrap().unwrap().state, state);
    }
    // One failed load per session, the miss cached.
    assert_eq!(failing.loads(), 2);
}
