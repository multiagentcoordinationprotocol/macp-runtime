//! The session anchor, the transition spans and the session metrics (R2 P4),
//! through a real tonic server on `127.0.0.1:0` built as `src/main.rs`
//! builds it (the SERVER span layer, the MACP service), with the runtime
//! shared so a test can drive the background sweeps and the `ManualClock`.
//!
//! Spans go to an in-memory span exporter and metrics to an in-memory metric
//! exporter through `telemetry::init::active_for_tests`, installed as the
//! thread's default dispatcher; each test runs on tokio's current-thread
//! runtime so the server tasks see it. Names, kinds, attributes, instruments
//! and units are read from the vendored `tests/otel/macp-semconv.json`.

#![cfg(feature = "otel")]

#[path = "support/counting_backend.rs"]
mod counting_backend;
#[path = "support/otel_semconv.rs"]
mod otel_semconv;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use macp_runtime::decision_pb::ProposalPayload;
use macp_runtime::log_store::LogStore;
use macp_runtime::macp_core::commitment_hash::commitment_hash_of_wire;
use macp_runtime::macp_core::ManualClock;
use macp_runtime::metrics::SESSION_DURATION_BOUNDARIES;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    Ack, CancelSessionRequest, CommitmentPayload, Envelope, GetSessionRequest, ModeDescriptor,
    ResumeSessionRequest, SendRequest, SessionStartPayload, SuspendSessionRequest,
};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::storage::StorageBackend;
use macp_runtime::telemetry::{grpc, init, Telemetry};
use opentelemetry::trace::{SpanId, SpanKind};
use opentelemetry::Value;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use otel_semconv::Semconv;
use prost::Message;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const DECISION: &str = "macp.mode.decision.v1";
const CUSTOM: &str = "ext.custom.v1";
const NOW: i64 = 1_800_000_000_000;
const TTL_MS: i64 = 60_000;
const INITIATOR: &str = "agent://a";
const PEER: &str = "agent://b";
const OUTSIDER: &str = "agent://c";
const TRANSITIONS: [&str; 5] = ["resolve", "expire", "cancel", "suspend", "resume"];

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// One test's in-memory pipeline, server and runtime.
struct Harness {
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
    runtime: Arc<Runtime>,
    clock: Arc<ManualClock>,
    channel: Channel,
    _guard: tracing::dispatcher::DefaultGuard,
}

impl Harness {
    async fn start() -> Self {
        Self::with_storage(Arc::new(macp_runtime::storage::MemoryBackend)).await
    }

    async fn with_storage(storage: Arc<dyn StorageBackend>) -> Self {
        install_propagator();
        let spans = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(spans.clone())
            .build();
        let metrics = InMemoryMetricExporter::default();
        let meter = SdkMeterProvider::builder()
            .with_periodic_exporter(metrics.clone())
            .build();
        let (dispatch, telemetry) = init::active_for_tests(&tracer, Some(&meter));
        let guard = tracing::dispatcher::set_default(&dispatch);
        let clock = Arc::new(ManualClock::new(NOW));
        let runtime = Arc::new(runtime(storage, telemetry, Arc::clone(&clock)));
        let channel = serve(Arc::clone(&runtime)).await;
        Self {
            spans,
            metrics,
            tracer,
            meter,
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

    fn named(&self, op: &str) -> Vec<SpanData> {
        self.all()
            .into_iter()
            .filter(|s| s.name.split(' ').next() == Some(op))
            .collect()
    }

    fn transitions(&self) -> Vec<SpanData> {
        self.all()
            .into_iter()
            .filter(|s| TRANSITIONS.contains(&s.name.split(' ').next().unwrap_or("")))
            .collect()
    }

    /// Wait (yielding to the server tasks) until `n` SERVER spans ended.
    async fn wait_for_server(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .all()
            .iter()
            .filter(|s| s.span_kind == SpanKind::Server)
            .count()
            < n
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn collect(&self) -> ResourceMetrics {
        self.meter.force_flush().unwrap();
        self.metrics
            .get_finished_metrics()
            .unwrap()
            .pop()
            .expect("one collection")
    }

    fn client(&self) -> MacpRuntimeServiceClient<Channel> {
        MacpRuntimeServiceClient::new(self.channel.clone())
    }

    async fn send_as(&self, sender: &str, env: Envelope) -> Ack {
        self.client()
            .send(authed(
                SendRequest {
                    envelope: Some(env),
                },
                sender,
            ))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap()
    }

    async fn send(&self, env: Envelope) -> Ack {
        self.send_as(INITIATOR, env).await
    }

    /// An accepted Decision session with one Proposal (so a Commitment can
    /// resolve it).
    async fn open_decision(&self, sid: &str) {
        assert!(
            self.send(start(DECISION, &format!("{sid}-start"), sid))
                .await
                .ok
        );
        assert!(self.send(proposal(&format!("{sid}-p"), sid)).await.ok);
    }

    async fn cancel(&self, sid: &str) {
        self.client()
            .cancel_session(authed(
                CancelSessionRequest {
                    session_id: sid.into(),
                    reason: "stop".into(),
                },
                INITIATOR,
            ))
            .await
            .unwrap();
    }

    async fn suspend(&self, sid: &str) {
        self.client()
            .suspend_session(authed(
                SuspendSessionRequest {
                    session_id: sid.into(),
                    reason: "pause".into(),
                },
                INITIATOR,
            ))
            .await
            .unwrap();
    }

    async fn resume(&self, sid: &str) -> Ack {
        self.client()
            .resume_session(authed(
                ResumeSessionRequest {
                    session_id: sid.into(),
                    reason: "go".into(),
                },
                INITIATOR,
            ))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap()
    }

    /// The span id of `sid`'s anchor: its accepted SessionStart's `process`
    /// span.
    fn anchor_of(&self, sid: &str) -> SpanId {
        let all = self.all();
        let start = all
            .iter()
            .find(|s| {
                s.name.starts_with("process")
                    && attr_str(s, "macp.session.id").as_deref() == Some(sid)
                    && attr_str(s, "macp.message.type").as_deref() == Some("SessionStart")
                    && attr_str(s, "macp.envelope.outcome").as_deref() == Some("accepted")
            })
            .unwrap_or_else(|| panic!("no accepted SessionStart span for {sid}"));
        start.span_context.span_id()
    }
}

fn runtime(
    storage: Arc<dyn StorageBackend>,
    telemetry: Telemetry,
    clock: Arc<ManualClock>,
) -> Runtime {
    Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_clock(clock)
    .with_telemetry(telemetry)
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

fn authed<T>(msg: T, sender: &str) -> tonic::Request<T> {
    let mut req = tonic::Request::new(msg);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {sender}").parse().unwrap());
    req
}

fn sid(n: u32) -> String {
    format!("0b7c1a3e-2f4d-4c6b-9a8e-{n:012x}")
}

fn envelope(
    mode: &str,
    message_type: &str,
    message_id: &str,
    sid: &str,
    payload: Vec<u8>,
) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: mode.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: sid.into(),
        sender: String::new(),
        timestamp_unix_ms: NOW,
        payload,
    }
}

fn start(mode: &str, message_id: &str, sid: &str) -> Envelope {
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
    envelope(mode, "SessionStart", message_id, sid, payload)
}

fn proposal(message_id: &str, sid: &str) -> Envelope {
    let payload = ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "why".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    envelope(DECISION, "Proposal", message_id, sid, payload)
}

fn commitment_payload() -> Vec<u8> {
    CommitmentPayload {
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
    .encode_to_vec()
}

fn commitment(mode: &str, message_id: &str, sid: &str) -> Envelope {
    envelope(mode, "Commitment", message_id, sid, commitment_payload())
}

fn custom_descriptor() -> ModeDescriptor {
    ModeDescriptor {
        mode: CUSTOM.into(),
        mode_version: "1.0.0".into(),
        title: "Custom".into(),
        message_types: vec!["SessionStart".into(), "Ping".into(), "Commitment".into()],
        terminal_message_types: vec!["Commitment".into()],
        ..Default::default()
    }
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

fn attr_str(span: &SpanData, key: &str) -> Option<String> {
    attr(span, key).map(|v| v.as_str().into_owned())
}

fn for_session<'a>(spans: &'a [SpanData], op: &str, sid: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|s| s.name.split(' ').next() == Some(op))
        .filter(|s| attr_str(s, "macp.session.id").as_deref() == Some(sid))
        .collect()
}

fn one<'a>(spans: &'a [SpanData], op: &str, sid: &str) -> &'a SpanData {
    let found = for_session(spans, op, sid);
    assert_eq!(found.len(), 1, "{op} spans for {sid}: {found:?}");
    found[0]
}

fn by_message_id<'a>(spans: &'a [SpanData], message_id: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|s| attr_str(s, "macp.message.id").as_deref() == Some(message_id))
        .collect()
}

/// Assert `span` against `span.macp.{op}` of the vendored semconv, with the
/// state entered and exactly one link, to `anchor`.
fn assert_transition(semconv: &Semconv, span: &SpanData, op: &str, state: &str, anchor: SpanId) {
    let group = semconv.span(&format!("span.macp.{op}"));
    let mode = attr_str(span, "macp.mode.id").expect("macp.mode.id");
    assert_eq!(group.operation(), op);
    assert_eq!(
        span.name,
        group.span_name.replace("{macp.mode.id}", &mode),
        "{span:?}"
    );
    assert_eq!(group.span_kind, "internal");
    assert_eq!(span.span_kind, SpanKind::Internal);
    let keys: BTreeSet<&str> = span.attributes.iter().map(|kv| kv.key.as_str()).collect();
    for a in &group.attributes {
        if a.requirement.is_required() {
            assert!(keys.contains(a.id.as_str()), "{} missing: {span:?}", a.id);
        }
    }
    let refs: BTreeSet<&str> = group.attributes.iter().map(|a| a.id.as_str()).collect();
    for k in &keys {
        assert!(
            refs.contains(k),
            "{k} is not a ref of {}: {span:?}",
            group.id
        );
    }
    assert_eq!(attr_str(span, "macp.operation.name").as_deref(), Some(op));
    assert!(semconv
        .enum_members("macp.operation.name")
        .contains(&op.to_string()));
    assert_eq!(attr_str(span, "macp.session.state").as_deref(), Some(state));
    assert!(semconv
        .enum_members("macp.session.state")
        .contains(&state.to_string()));
    assert_eq!(span.links.len(), 1, "exactly one link: {span:?}");
    let link = span.links.iter().next().unwrap();
    assert_eq!(link.span_context.span_id(), anchor, "link to the anchor");
    assert!(link.attributes.is_empty());
    assert!(span.events.is_empty(), "{:?}", span.events);
}

// AC 1: every transition span conforms to its semconv group, records the
// state entered, and links exactly once to the SessionStart's span.
#[tokio::test]
async fn transition_spans_conform_and_link_the_anchor() {
    let semconv = Semconv::load();
    let h = Harness::start().await;
    let (resolved, cancelled, paused, timed_out) = (sid(1), sid(2), sid(3), sid(4));

    h.open_decision(&resolved).await;
    assert!(h.send(commitment(DECISION, "c", &resolved)).await.ok);
    h.open_decision(&cancelled).await;
    h.cancel(&cancelled).await;
    h.open_decision(&paused).await;
    h.suspend(&paused).await;
    assert!(h.resume(&paused).await.ok);
    h.open_decision(&timed_out).await;
    h.clock.set(NOW + TTL_MS + 2_500);
    h.runtime.cleanup_expired_sessions().await;
    h.wait_for_server(12).await;

    let all = h.all();
    for (sid, op, state) in [
        (&resolved, "resolve", "SESSION_STATE_RESOLVED"),
        (&cancelled, "cancel", "SESSION_STATE_CANCELLED"),
        (&paused, "suspend", "SESSION_STATE_SUSPENDED"),
        (&paused, "resume", "SESSION_STATE_OPEN"),
        (&timed_out, "expire", "SESSION_STATE_EXPIRED"),
        // Open again after the resume, so the sweep expires it too.
        (&paused, "expire", "SESSION_STATE_EXPIRED"),
    ] {
        assert_transition(&semconv, one(&all, op, sid), op, state, h.anchor_of(sid));
    }
    // Nothing else transitioned.
    assert_eq!(h.transitions().len(), 6, "{:?}", h.transitions());
}

// AC 2: `expire` is a root span on every path: the background sweep, a
// message observing the deadline lazily, a read RPC observing it, and the
// resume that force-expires a session past its suspension cap.
#[tokio::test]
async fn expire_is_a_root_on_every_path() {
    let h = Harness::start().await;
    let (swept, lazy, read, capped) = (sid(1), sid(2), sid(3), sid(4));
    for s in [&swept, &lazy, &read, &capped] {
        h.open_decision(s).await;
    }
    h.suspend(&capped).await;

    // Lazy, inside a `process` span.
    h.clock.set(NOW + TTL_MS + 1);
    let ack = h.send(proposal("late", &lazy)).await;
    assert!(!ack.ok, "{ack:?}");
    // A read RPC.
    h.client()
        .get_session(authed(
            GetSessionRequest {
                session_id: read.clone(),
            },
            INITIATOR,
        ))
        .await
        .unwrap();
    // The sweep (the capped session is Suspended, so the sweep skips it).
    h.runtime.cleanup_expired_sessions().await;
    // Resume past the suspension cap: a force-expire.
    h.clock
        .set(NOW + macp_runtime::macp_core::session::MAX_SUSPEND_MS + 10_000);
    let ack = h.resume(&capped).await;
    assert_eq!(ack.error.unwrap().code, "SESSION_NOT_OPEN", "force-expired");
    h.wait_for_server(12).await;

    let all = h.all();
    for s in [&swept, &lazy, &read, &capped] {
        let expire = one(&all, "expire", s);
        assert_eq!(expire.parent_span_id, SpanId::INVALID, "root: {expire:?}");
        assert_eq!(
            attr_str(expire, "macp.session.state").as_deref(),
            Some("SESSION_STATE_EXPIRED")
        );
        assert_eq!(expire.links.len(), 1);
        assert_eq!(
            expire.links.iter().next().unwrap().span_context.span_id(),
            h.anchor_of(s)
        );
    }
    // The observing request's trace is not the expire's.
    let observer = by_message_id(&all, "late")[0];
    assert_eq!(
        attr_str(observer, "error.type").as_deref(),
        Some("SESSION_NOT_OPEN")
    );
    let expire = one(&all, "expire", &lazy);
    assert_ne!(
        expire.span_context.trace_id(),
        observer.span_context.trace_id()
    );
    // The force-expire is an `expire`, never a `resume`.
    assert!(for_session(&all, "resume", &capped).is_empty());
}

// AC 2 (cycle cap): the MAX_SUSPENSION_CYCLES force-expire on resume is a
// root `expire` too. Driven on the runtime directly, as 1,025 cycles through
// gRPC would only slow the test.
#[tokio::test]
async fn suspension_cycle_cap_force_expire_is_a_root_expire() {
    let h = Harness::start().await;
    let s = sid(9);
    h.open_decision(&s).await;
    let cap = macp_runtime::macp_core::session::MAX_SUSPENSION_CYCLES;
    let mut expired = false;
    for _ in 0..=cap {
        h.runtime.suspend_session(&s, "", INITIATOR).await.unwrap();
        if h.runtime.resume_session(&s, "", INITIATOR).await.is_err() {
            expired = true;
            break;
        }
    }
    assert!(expired, "the cycle cap fired");
    let expire = h.named("expire");
    assert_eq!(expire.len(), 1, "{expire:?}");
    assert_eq!(expire[0].parent_span_id, SpanId::INVALID);
    assert_eq!(expire[0].links.len(), 1);
    assert_eq!(h.named("resume").len(), cap);
    assert_eq!(h.named("suspend").len(), cap + 1);
}

// AC 3: `resolve` is a child of the Commitment's `process` span and carries
// the RFC-MACP-0013 commitment hash of the payload.
#[tokio::test]
async fn resolve_is_a_child_of_the_commitment_with_its_hash() {
    let h = Harness::start().await;
    let s = sid(1);
    h.open_decision(&s).await;
    assert!(h.send(commitment(DECISION, "commit", &s)).await.ok);
    let all = h.all();
    let resolve = one(&all, "resolve", &s);
    let commit = by_message_id(&all, "commit")[0];
    assert_eq!(resolve.parent_span_id, commit.span_context.span_id());
    assert_eq!(
        resolve.span_context.trace_id(),
        commit.span_context.trace_id()
    );
    let hash = attr_str(resolve, "macp.commitment.hash").expect("hash");
    assert_eq!(
        hash,
        commitment_hash_of_wire(&commitment_payload()).unwrap()
    );
    let hex = hash.strip_prefix("sha256:").expect("sha256: prefix");
    assert!(
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    // The recommended attributes of the resolve group.
    assert_eq!(
        attr_str(resolve, "macp.mode.version").as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        attr_str(resolve, "macp.configuration.version").as_deref(),
        Some("cfg-1")
    );
    assert_eq!(
        attr_str(resolve, "macp.policy.id").as_deref(),
        Some("policy.default")
    );
}

// AC 4 (negative): an extension (passthrough-backed) mode's resolve carries
// no commitment hash, and a mode unregistered mid-session names its
// transition spans and labels its metrics `_OTHER`.
#[tokio::test]
async fn extension_resolve_has_no_hash_and_unregistered_mode_is_other() {
    let h = Harness::start().await;
    h.runtime.register_extension(custom_descriptor()).unwrap();
    let (resolved, orphaned) = (sid(1), sid(2));
    assert!(h.send(start(CUSTOM, "s1", &resolved)).await.ok);
    assert!(h.send(commitment(CUSTOM, "c1", &resolved)).await.ok);
    assert!(h.send(start(CUSTOM, "s2", &orphaned)).await.ok);
    h.runtime.unregister_extension(CUSTOM).unwrap();
    h.cancel(&orphaned).await;
    h.wait_for_server(4).await;

    let all = h.all();
    let resolve = one(&all, "resolve", &resolved);
    assert_eq!(resolve.name, format!("resolve {CUSTOM}"));
    assert!(
        attr(resolve, "macp.commitment.hash").is_none(),
        "{resolve:?}"
    );
    let cancel = one(&all, "cancel", &orphaned);
    assert_eq!(cancel.name, "cancel _OTHER");
    assert_eq!(attr_str(cancel, "macp.mode.id").as_deref(), Some(CUSTOM));

    let rm = h.collect();
    let points = transition_points(&rm);
    assert!(
        points.contains(&(CUSTOM.to_string(), "resolve".to_string(), 1)),
        "{points:?}"
    );
    assert!(
        points.contains(&("_OTHER".to_string(), "cancel".to_string(), 1)),
        "{points:?}"
    );
}

/// `(mode, operation, value)` of every `macp.session.transitions` point.
fn transition_points(rm: &ResourceMetrics) -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    for m in rm.scope_metrics().flat_map(|sm| sm.metrics()) {
        if m.name() != "macp.session.transitions" {
            continue;
        }
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = m.data() else {
            panic!("macp.session.transitions is not a u64 Sum");
        };
        for p in sum.data_points() {
            let get = |k: &str| {
                p.attributes()
                    .find(|kv| kv.key.as_str() == k)
                    .map(|kv| kv.value.as_str().into_owned())
                    .unwrap()
            };
            out.push((get("macp.mode.id"), get("macp.operation.name"), p.value()));
        }
    }
    out.sort();
    out
}

/// `(state, count, sum)` of every `macp.session.duration` point.
fn duration_points(rm: &ResourceMetrics) -> Vec<(String, u64, f64)> {
    let mut out = Vec::new();
    for m in rm.scope_metrics().flat_map(|sm| sm.metrics()) {
        if m.name() != "macp.session.duration" {
            continue;
        }
        let AggregatedMetrics::F64(MetricData::Histogram(hist)) = m.data() else {
            panic!("macp.session.duration is not an f64 Histogram");
        };
        for p in hist.data_points() {
            assert_eq!(p.bounds().collect::<Vec<_>>(), SESSION_DURATION_BOUNDARIES);
            let state = p
                .attributes()
                .find(|kv| kv.key.as_str() == "macp.session.state")
                .map(|kv| kv.value.as_str().into_owned())
                .unwrap();
            out.push((state, p.count(), p.sum()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// AC 4: the session metrics match the semconv (name, instrument, unit,
// attribute keys, no identifiers), and the duration is measured on the
// session timeline: `(now_ms - started_at_unix_ms) / 1000` exactly, at the
// clock value the sweep (or the Commitment) observed, not `ttl_ms / 1000`.
#[tokio::test]
async fn session_metrics_match_the_semconv_and_the_session_timeline() {
    let semconv = Semconv::load();
    let h = Harness::start().await;
    let (resolved, timed_out, paused) = (sid(1), sid(2), sid(3));
    h.open_decision(&resolved).await;
    h.open_decision(&timed_out).await;
    h.open_decision(&paused).await;
    h.suspend(&paused).await;
    assert!(h.resume(&paused).await.ok);
    h.clock.set(NOW + 12_345);
    assert!(h.send(commitment(DECISION, "c", &resolved)).await.ok);
    h.clock.set(NOW + TTL_MS + 2_500);
    h.runtime.cleanup_expired_sessions().await;
    let rm = h.collect();

    let identifiers = ["macp.session.id", "macp.message.id", "macp.sender.id"];
    let session_metrics: Vec<_> = rm
        .scope_metrics()
        .flat_map(|sm| sm.metrics())
        .filter(|m| m.name().starts_with("macp.session."))
        .collect();
    let mut names: Vec<&str> = session_metrics.iter().map(|m| m.name()).collect();
    names.sort_unstable();
    assert_eq!(names, ["macp.session.duration", "macp.session.transitions"]);
    for m in &session_metrics {
        let group = semconv
            .metrics
            .iter()
            .find(|g| g.metric_name == m.name())
            .unwrap_or_else(|| panic!("{} not in the semconv", m.name()));
        assert_eq!(m.unit(), group.unit, "{}", m.name());
        let refs: BTreeSet<&str> = group.attributes.iter().map(|a| a.id.as_str()).collect();
        let check = |keys: Vec<String>| {
            for k in &keys {
                assert!(refs.contains(k.as_str()), "{k} not a ref of {}", group.id);
                assert!(!identifiers.contains(&k.as_str()), "identifier {k}");
            }
            for a in &group.attributes {
                if a.requirement.is_required() {
                    assert!(keys.contains(&a.id), "{} missing on {}", a.id, m.name());
                }
            }
        };
        match (group.instrument.as_str(), m.data()) {
            ("counter", AggregatedMetrics::U64(MetricData::Sum(sum))) => {
                assert!(sum.is_monotonic());
                for p in sum.data_points() {
                    check(p.attributes().map(|kv| kv.key.to_string()).collect());
                }
            }
            ("histogram", AggregatedMetrics::F64(MetricData::Histogram(hist))) => {
                for p in hist.data_points() {
                    check(p.attributes().map(|kv| kv.key.to_string()).collect());
                }
            }
            (instrument, data) => panic!("{}: {instrument} recorded as {data:?}", m.name()),
        }
    }

    assert_eq!(
        transition_points(&rm),
        [
            (DECISION.to_string(), "expire".to_string(), 2),
            (DECISION.to_string(), "resolve".to_string(), 1),
            (DECISION.to_string(), "resume".to_string(), 1),
            (DECISION.to_string(), "suspend".to_string(), 1),
        ]
    );
    // The resumed session is Open again, so the sweep expires it with the
    // other: two expiries, each observed at TTL + 2.5s.
    let durations = duration_points(&rm);
    let started = NOW; // every SessionStart is stamped NOW
    let expired_s = ((NOW + TTL_MS + 2_500) - started) as f64 / 1000.0;
    let resolved_s = ((NOW + 12_345) - started) as f64 / 1000.0;
    assert_eq!(
        durations,
        [
            ("SESSION_STATE_EXPIRED".to_string(), 2, 2.0 * expired_s),
            ("SESSION_STATE_RESOLVED".to_string(), 1, resolved_s),
        ],
        "{durations:?}"
    );
    assert_eq!(expired_s, 62.5);
    assert_ne!(expired_s, TTL_MS as f64 / 1000.0);

    // The Prometheus rendering carries the same families.
    let mut body = String::new();
    h.runtime.metrics().semconv_prometheus_lines(&mut body);
    assert!(body.contains(&format!(
        "macp_session_transitions_total{{macp_mode_id=\"{DECISION}\",macp_operation_name=\"resolve\"}} 1\n"
    )));
    assert!(body.contains(&format!(
        "macp_session_duration_seconds_sum{{macp_mode_id=\"{DECISION}\",macp_session_state=\"SESSION_STATE_EXPIRED\"}} 125\n"
    )));
}

// Links: every later `process` span of a session links once to the anchor,
// whatever its outcome (accepted, duplicate, rejected by the kernel,
// rejected by the server before the kernel, a duplicate SessionStart and a
// SessionStart for an existing id). The anchor's own span has no link.
#[tokio::test]
async fn later_process_spans_link_the_anchor_exactly_once() {
    let h = Harness::start().await;
    let s = sid(1);
    assert!(h.send(start(DECISION, "start", &s)).await.ok);
    assert!(h.send(proposal("p", &s)).await.ok);
    assert!(h.send(proposal("p", &s)).await.duplicate);
    let ack = h.send_as(OUTSIDER, proposal("outsider", &s)).await;
    assert_eq!(ack.error.unwrap().code, "FORBIDDEN");
    // Rejected by the server's shape check, before the kernel.
    let mut bad = proposal("bad-version", &s);
    bad.macp_version = "9.9".into();
    assert!(!h.send(bad).await.ok);
    assert!(h.send(start(DECISION, "start", &s)).await.duplicate);
    assert!(!h.send(start(DECISION, "start-again", &s)).await.ok);

    let all = h.all();
    let anchor = h.anchor_of(&s);
    let process: Vec<_> = all
        .iter()
        .filter(|sp| sp.name.starts_with("process"))
        .collect();
    assert_eq!(process.len(), 7, "{process:?}");
    for sp in process {
        if sp.span_context.span_id() == anchor {
            assert!(sp.links.is_empty(), "the anchor links to nothing: {sp:?}");
            continue;
        }
        assert_eq!(sp.links.len(), 1, "{sp:?}");
        assert_eq!(
            sp.links.iter().next().unwrap().span_context.span_id(),
            anchor
        );
    }
}

// Bounded: 10,000 rejected envelopes with distinct client-chosen session ids
// (unauthenticated, so refused before the kernel) add nothing to the anchor
// cache and get no link. One real session proves the cache is live.
#[tokio::test]
async fn rejected_envelopes_with_arbitrary_session_ids_never_grow_the_anchor_cache() {
    // Counts storage anchor loads (P5): a non-resident id never reaches
    // storage either.
    let counting = counting_backend::Counting::new(Arc::new(macp_runtime::storage::MemoryBackend));
    let h = Harness::with_storage(counting.clone()).await;
    h.open_decision(&sid(1)).await;
    assert_eq!(h.runtime.trace_anchor_count(), 1);

    let mut tasks = Vec::new();
    for batch in 0..100u32 {
        let channel = h.channel.clone();
        tasks.push(tokio::spawn(async move {
            let mut client = MacpRuntimeServiceClient::new(channel);
            for i in 0..100u32 {
                let n = 1_000 + batch * 100 + i;
                // No authorization header: refused before the kernel.
                let _ = client
                    .send(SendRequest {
                        envelope: Some(proposal(&format!("m{n}"), &sid(n))),
                    })
                    .await;
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(h.runtime.trace_anchor_count(), 1);
    let rejected: Vec<_> = h
        .all()
        .into_iter()
        .filter(|s| s.name.starts_with("process"))
        .filter(|s| attr_str(s, "macp.envelope.outcome").as_deref() == Some("rejected"))
        .collect();
    assert_eq!(rejected.len(), 10_000);
    assert!(rejected.iter().all(|s| s.links.is_empty()));
    assert_eq!(counting.loads(), 0, "no storage read for a non-resident id");
}

// Anchors leave with their session: memory eviction, disk GC, and any other
// removal from the registry (pruned on the next eviction pass), so the map
// cannot outgrow the resident set.
#[tokio::test]
async fn anchors_are_forgotten_when_sessions_leave_the_registry() {
    let dir = tempfile::tempdir().unwrap();
    let storage =
        Arc::new(macp_runtime::storage::FileBackend::new(dir.path().to_path_buf()).unwrap());
    let h = Harness::with_storage(storage).await;
    let (evicted, collected, removed, live) = (sid(1), sid(2), sid(3), sid(4));
    for s in [&evicted, &collected, &removed, &live] {
        h.open_decision(s).await;
    }
    assert_eq!(h.runtime.trace_anchor_count(), 4);

    // Memory eviction of a terminal session.
    h.cancel(&evicted).await;
    h.clock.set(NOW + 10_000);
    h.runtime.evict_stale_sessions(5).await;
    assert!(h.runtime.registry.get_shared(&evicted).await.is_none());
    assert_eq!(h.runtime.trace_anchor_count(), 3);

    // Disk GC of a terminal session still resident in memory.
    h.cancel(&collected).await;
    assert_eq!(
        h.runtime.gc_disk_sessions(5).await,
        2,
        "both ended sessions"
    );
    assert!(h.runtime.registry.get_shared(&collected).await.is_none());
    assert_eq!(h.runtime.trace_anchor_count(), 2);

    // Removed from the (public) registry map by anything else: pruned on
    // the next eviction pass, which evicts nothing itself.
    h.runtime.registry.sessions.write().await.remove(&removed);
    assert_eq!(h.runtime.trace_anchor_count(), 2);
    h.runtime.evict_stale_sessions(5).await;
    assert_eq!(h.runtime.trace_anchor_count(), 1);

    // The live session keeps its anchor and its later spans still link.
    // (Its outcome does not matter here, only its link.)
    h.send(proposal("after", &live)).await;
    let all = h.all();
    let after = by_message_id(&all, "after");
    assert_eq!(after[0].links.len(), 1);

    // Many sessions started and evicted: the map tracks the resident set.
    for n in 100..300u32 {
        let s = sid(n);
        h.open_decision(&s).await;
        h.cancel(&s).await;
    }
    assert_eq!(h.runtime.trace_anchor_count(), 201);
    h.clock.set(NOW + 1_000_000);
    h.runtime.evict_stale_sessions(5).await;
    assert_eq!(h.runtime.trace_anchor_count(), 1, "only the live session");
}

// The inactive path: a runtime with disabled telemetry records no anchor
// and no span, while the Prometheus session families still count.
#[tokio::test]
async fn inactive_telemetry_records_no_anchor_and_no_span() {
    let h = Harness::start().await; // installs the dispatcher only
    let clock = Arc::new(ManualClock::new(NOW));
    let inactive = Arc::new(runtime(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Telemetry::disabled(),
        Arc::clone(&clock),
    ));
    let channel = serve(Arc::clone(&inactive)).await;
    let client = || MacpRuntimeServiceClient::new(channel.clone());
    let s = sid(1);
    for env in [
        start(DECISION, "start", &s),
        proposal("p", &s),
        commitment(DECISION, "c", &s),
    ] {
        let ack = client()
            .send(authed(
                SendRequest {
                    envelope: Some(env),
                },
                INITIATOR,
            ))
            .await
            .unwrap()
            .into_inner()
            .ack
            .unwrap();
        assert!(ack.ok, "{ack:?}");
    }
    assert_eq!(inactive.trace_anchor_count(), 0);
    let spans = h.all();
    assert!(
        spans.iter().all(|s| s.span_kind != SpanKind::Internal),
        "{spans:?}"
    );
    let mut body = String::new();
    inactive.metrics().semconv_prometheus_lines(&mut body);
    assert!(
        body.contains("macp_operation_name=\"resolve\"} 1"),
        "{body}"
    );
}

// Parents: `cancel`, `suspend` and `resume` are each a child of the SERVER
// span of the RPC that caused them, in that RPC's trace.
#[tokio::test]
async fn lifecycle_rpc_transitions_are_children_of_their_server_span() {
    let h = Harness::start().await;
    let (cancelled, paused) = (sid(1), sid(2));
    h.open_decision(&cancelled).await;
    h.open_decision(&paused).await;
    h.cancel(&cancelled).await;
    h.suspend(&paused).await;
    assert!(h.resume(&paused).await.ok);
    // Four Sends, then the three lifecycle RPCs.
    h.wait_for_server(7).await;

    let all = h.all();
    for (op, sid, method) in [
        ("cancel", &cancelled, "CancelSession"),
        ("suspend", &paused, "SuspendSession"),
        ("resume", &paused, "ResumeSession"),
    ] {
        let name = format!("macp.v1.MACPRuntimeService/{method}");
        let servers: Vec<_> = all
            .iter()
            .filter(|s| s.span_kind == SpanKind::Server && s.name == name)
            .collect();
        assert_eq!(servers.len(), 1, "{name}: {servers:?}");
        let span = one(&all, op, sid);
        assert_eq!(
            span.parent_span_id,
            servers[0].span_context.span_id(),
            "{op} is a child of {name}"
        );
        assert_eq!(
            span.span_context.trace_id(),
            servers[0].span_context.trace_id()
        );
    }
}
