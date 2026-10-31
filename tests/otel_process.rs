//! The `process` span and the Envelope metrics (R2 P3), through a real tonic
//! server on `127.0.0.1:0` built as `src/main.rs` builds it (the SERVER span
//! layer from `telemetry::grpc::optional_server_layer`, the MACP service).
//! Spans go to an in-memory span exporter and metrics to an in-memory metric
//! exporter, through `telemetry::init::active_for_tests` installed as the
//! thread's default dispatcher. Each test runs on tokio's current-thread
//! runtime, so the server tasks it spawns see that dispatcher.
//!
//! The global text-map propagator is set once per process to the W3C one,
//! as `init::init` does in production (see `tests/otel_grpc.rs`).
//!
//! Every assertion about names, kinds, attributes, enum values, instruments
//! and units reads the vendored `tests/otel/macp-semconv.json`.

#![cfg(feature = "otel")]

#[path = "support/otel_semconv.rs"]
mod otel_semconv;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use macp_runtime::decision_pb::{ProposalPayload, VotePayload};
use macp_runtime::log_store::{LogEntry, LogStore};
use macp_runtime::metrics::{RuntimeMetrics, ENVELOPE_DURATION_BOUNDARIES};
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    Ack, CancelSessionRequest, CommitmentPayload, Envelope, SendRequest, SessionStartPayload,
    SignalPayload, StreamSessionRequest,
};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::session::Session;
use macp_runtime::storage::StorageBackend;
use macp_runtime::telemetry::labels::EnvelopeLabels;
use macp_runtime::telemetry::process::{Outcome, ProcessSpan};
use macp_runtime::telemetry::{grpc, init, Telemetry};
use opentelemetry::trace::{SpanId, SpanKind, Status, TraceId};
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
const BOGUS: &str = "ext.bogus.v1";
const SID: &str = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f60";
const SID2: &str = "7d1e2f3a-4b5c-4d6e-8f70-112233445566";
const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT_ID: &str = "00f067aa0ba902b7";
const NOW: i64 = 1_800_000_000_000;
const INITIATOR: &str = "agent://a";
const PEER: &str = "agent://b";
const OUTSIDER: &str = "agent://c";
const SENTINEL_PAYLOAD: &str = "SENTINEL-PAYLOAD-7f3c";
const SENTINEL_REASON: &str = "SENTINEL-REASON-9a51";
const SENTINEL_VOTE: &str = "SENTINEL-VOTE-REASON-41d2";

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// The in-memory span and metric pipeline of one test.
struct Pipeline {
    spans: InMemorySpanExporter,
    metrics: InMemoryMetricExporter,
    tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
    _guard: tracing::dispatcher::DefaultGuard,
}

impl Pipeline {
    fn install() -> (Self, Telemetry) {
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
        (
            Self {
                spans,
                metrics,
                tracer,
                meter,
                _guard: guard,
            },
            telemetry,
        )
    }

    fn all(&self) -> Vec<SpanData> {
        self.tracer.force_flush().unwrap();
        self.spans.get_finished_spans().unwrap()
    }

    fn process(&self) -> Vec<SpanData> {
        self.all()
            .into_iter()
            .filter(|s| s.name == "process" || s.name.starts_with("process "))
            .collect()
    }

    fn server(&self) -> Vec<SpanData> {
        self.all()
            .into_iter()
            .filter(|s| s.span_kind == SpanKind::Server)
            .collect()
    }

    /// Wait (yielding to the server tasks) until `n` SERVER spans ended.
    async fn wait_for_server(&self, n: usize) -> Vec<SpanData> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let spans = self.server();
            if spans.len() >= n || Instant::now() > deadline {
                return spans;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The latest cumulative collection.
    fn collect(&self) -> ResourceMetrics {
        self.meter.force_flush().unwrap();
        self.metrics
            .get_finished_metrics()
            .unwrap()
            .pop()
            .expect("one collection")
    }
}

fn runtime(storage: Arc<dyn StorageBackend>, telemetry: Telemetry) -> Runtime {
    Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_clock(Arc::new(macp_runtime::macp_core::ManualClock::new(NOW)))
    .with_telemetry(telemetry)
}

async fn serve_runtime(runtime: Runtime) -> Channel {
    let telemetry_layer = grpc::server_layer();
    let svc = MacpServer::new(Arc::new(runtime), SecurityLayer::dev_mode());
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    let router = Server::builder()
        .layer(telemetry_layer)
        .add_service(MacpRuntimeServiceServer::new(svc));
    tokio::spawn(router.serve_with_incoming(incoming));
    tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

async fn serve(telemetry: Telemetry) -> Channel {
    serve_runtime(runtime(
        Arc::new(macp_runtime::storage::MemoryBackend),
        telemetry,
    ))
    .await
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

fn start_payload(extensions: HashMap<String, Vec<u8>>) -> Vec<u8> {
    SessionStartPayload {
        intent: "intent".into(),
        participants: vec![INITIATOR.into(), PEER.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: 60_000,
        extensions,
        ..Default::default()
    }
    .encode_to_vec()
}

fn start(mode: &str, message_id: &str, sid: &str) -> Envelope {
    envelope(
        mode,
        "SessionStart",
        message_id,
        sid,
        start_payload(HashMap::new()),
    )
}

fn proposal(message_id: &str, sid: &str) -> Envelope {
    let payload = ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: SENTINEL_PAYLOAD.into(),
        supporting_data: SENTINEL_PAYLOAD.as_bytes().to_vec(),
    }
    .encode_to_vec();
    envelope(DECISION, "Proposal", message_id, sid, payload)
}

fn vote(message_id: &str, sid: &str, reason: &str) -> Envelope {
    let payload = VotePayload {
        proposal_id: "p1".into(),
        vote: "APPROVE".into(),
        reason: reason.into(),
    }
    .encode_to_vec();
    envelope(DECISION, "Vote", message_id, sid, payload)
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
    envelope(DECISION, "Commitment", message_id, sid, payload)
}

fn signal(message_id: &str) -> Envelope {
    let payload = SignalPayload {
        signal_type: "heartbeat".into(),
        ..Default::default()
    }
    .encode_to_vec();
    envelope("", "Signal", message_id, "", payload)
}

fn authed<T>(msg: T, sender: &str, traceparent: Option<&str>) -> tonic::Request<T> {
    let mut req = tonic::Request::new(msg);
    req.metadata_mut()
        .insert("authorization", format!("Bearer {sender}").parse().unwrap());
    if let Some(tp) = traceparent {
        req.metadata_mut()
            .insert("traceparent", tp.parse().unwrap());
    }
    req
}

async fn send_as(channel: &Channel, sender: &str, env: Envelope, traceparent: Option<&str>) -> Ack {
    MacpRuntimeServiceClient::new(channel.clone())
        .send(authed(
            SendRequest {
                envelope: Some(env),
            },
            sender,
            traceparent,
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap()
}

async fn send(channel: &Channel, env: Envelope) -> Ack {
    send_as(channel, INITIATOR, env, None).await
}

fn traceparent() -> String {
    format!("00-{TRACE_ID}-{PARENT_ID}-01")
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

fn by_message_id<'a>(spans: &'a [SpanData], message_id: &str) -> Vec<&'a SpanData> {
    spans
        .iter()
        .filter(|s| attr_str(s, "macp.message.id").as_deref() == Some(message_id))
        .collect()
}

/// The scripted Decision traffic of AC 1, sent through `Send`, with the
/// acks: accepted SessionStart, accepted Proposal, duplicate Proposal,
/// rejected Vote by a non-participant, accepted ambient Signal, and a
/// SessionStart naming an unregistered mode.
async fn scripted_decision(channel: &Channel) -> Vec<Ack> {
    vec![
        send(channel, start(DECISION, "m1", SID)).await,
        send(channel, proposal("m2", SID)).await,
        send(channel, proposal("m2", SID)).await,
        send_as(channel, OUTSIDER, vote("m3", SID, SENTINEL_VOTE), None).await,
        send(channel, signal("m4")).await,
        send(channel, start(BOGUS, "m5", SID2)).await,
    ]
}

/// Assert `span` against `span.macp.process` in the vendored semconv.
fn assert_conforms(semconv: &Semconv, span: &SpanData) {
    let group = semconv.span("span.macp.process");
    let mode = attr_str(span, "macp.mode.id");
    let session = attr_str(span, "macp.session.id");
    let outcome = attr_str(span, "macp.envelope.outcome").expect("outcome");

    // Name: the template with the mode, `_OTHER` for an unregistered one,
    // bare for an empty one.
    let expected = match mode.as_deref() {
        None => group.operation().to_string(),
        Some(BOGUS) => format!("{} _OTHER", group.operation()),
        Some(m) => group.span_name.replace("{macp.mode.id}", m),
    };
    assert_eq!(span.name, expected, "{span:?}");
    assert_eq!(group.span_kind, "internal");
    assert_eq!(span.span_kind, SpanKind::Internal);

    let keys: BTreeSet<&str> = span.attributes.iter().map(|kv| kv.key.as_str()).collect();
    for a in &group.attributes {
        if a.requirement.is_required() {
            assert!(
                keys.contains(a.id.as_str()),
                "required {} missing: {span:?}",
                a.id
            );
        }
    }
    // Conditionally required: present exactly when the condition holds.
    let holds = |id: &str| match id {
        "macp.session.id" => session.is_some(),
        "macp.mode.id" => mode.is_some(),
        "error.type" => outcome == "rejected",
        other => panic!("unhandled conditionally_required attribute {other}"),
    };
    for a in &group.attributes {
        if a.requirement.is_conditionally_required() {
            assert_eq!(
                keys.contains(a.id.as_str()),
                holds(&a.id),
                "{} vs its condition {:?}: {span:?}",
                a.id,
                a.requirement.condition
            );
        }
    }
    // Enum values come from the file's members.
    for id in ["macp.operation.name", "macp.envelope.outcome"] {
        let v = attr_str(span, id).unwrap();
        assert!(semconv.enum_members(id).contains(&v), "{id}={v}");
    }
    assert_eq!(
        attr_str(span, "macp.operation.name").as_deref(),
        Some("process")
    );
    // Nothing outside the group's refs, `macp.*` or otherwise.
    let refs: BTreeSet<&str> = group.attributes.iter().map(|a| a.id.as_str()).collect();
    for k in &keys {
        assert!(
            refs.contains(k),
            "{k} is not a ref of span.macp.process: {span:?}"
        );
    }
    // No `tracing` event is ever exported.
    assert!(span.events.is_empty(), "{:?}", span.events);
}

// AC 1: semconv conformance of every `process` span of a scripted session.
#[tokio::test]
async fn process_spans_conform_to_the_semconv() {
    let semconv = Semconv::load();
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    let acks = scripted_decision(&channel).await;
    assert!(acks[0].ok && acks[1].ok && acks[4].ok, "{acks:?}");
    assert!(acks[2].ok && acks[2].duplicate, "{:?}", acks[2]);
    assert_eq!(acks[3].error.as_ref().unwrap().code, "FORBIDDEN");
    assert!(!acks[5].ok, "{:?}", acks[5]);

    let spans = pipeline.process();
    assert_eq!(spans.len(), 6, "{spans:?}");
    for span in &spans {
        assert_conforms(&semconv, span);
    }

    let outcomes: Vec<(String, String)> = ["m1", "m2", "m3", "m4", "m5"]
        .iter()
        .flat_map(|m| by_message_id(&spans, m))
        .map(|s| {
            (
                attr_str(s, "macp.message.id").unwrap(),
                attr_str(s, "macp.envelope.outcome").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            ("m1", "accepted"),
            ("m2", "accepted"),
            ("m2", "duplicate"),
            ("m3", "rejected"),
            ("m4", "accepted"),
            ("m5", "rejected"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );

    // The recommended attributes: the authenticated sender, the session's
    // bound versions and its resolved policy.
    let m2 = by_message_id(&spans, "m2")[0];
    assert_eq!(attr_str(m2, "macp.sender.id").as_deref(), Some(INITIATOR));
    assert_eq!(attr_str(m2, "macp.version").as_deref(), Some("1.0"));
    assert_eq!(attr_str(m2, "macp.mode.version").as_deref(), Some("1.0.0"));
    assert_eq!(
        attr_str(m2, "macp.configuration.version").as_deref(),
        Some("cfg-1")
    );
    assert_eq!(
        attr_str(m2, "macp.policy.id").as_deref(),
        Some("policy.default")
    );
    let m1 = by_message_id(&spans, "m1")[0];
    assert_eq!(
        attr_str(m1, "macp.policy.id").as_deref(),
        Some("policy.default")
    );

    // The unregistered mode: `process _OTHER`, the attribute as received.
    let m5 = by_message_id(&spans, "m5")[0];
    assert_eq!(m5.name, "process _OTHER");
    assert_eq!(attr_str(m5, "macp.mode.id").as_deref(), Some(BOGUS));
    assert_eq!(
        attr_str(m5, "error.type").as_deref(),
        Some("MODE_NOT_SUPPORTED")
    );
    // The Signal: bare name, no session or mode.
    let m4 = by_message_id(&spans, "m4")[0];
    assert_eq!(m4.name, "process");

    // ... and its metric point is `_OTHER` / `_OTHER`.
    let points = processed_points(&pipeline.collect());
    assert!(
        points.contains(&(
            vec![
                ("macp.envelope.outcome".into(), "rejected".into()),
                ("macp.message.type".into(), "_OTHER".into()),
                ("macp.mode.id".into(), "_OTHER".into()),
            ],
            1
        )),
        "{points:?}"
    );
}

// AC 2 (Send): client `send` -> SERVER -> `process`, one trace.
#[tokio::test]
async fn process_span_is_a_child_of_the_send_server_span() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    let ack = send_as(
        &channel,
        INITIATOR,
        start(DECISION, "m1", SID),
        Some(&traceparent()),
    )
    .await;
    assert!(ack.ok, "{ack:?}");
    let server = pipeline.wait_for_server(1).await;
    assert_eq!(server.len(), 1, "{server:?}");
    let process = pipeline.process();
    assert_eq!(process.len(), 1, "{process:?}");
    let (server, process) = (&server[0], &process[0]);
    let trace = TraceId::from_hex(TRACE_ID).unwrap();
    assert_eq!(server.span_context.trace_id(), trace);
    assert_eq!(server.parent_span_id, SpanId::from_hex(PARENT_ID).unwrap());
    assert_eq!(process.span_context.trace_id(), trace);
    assert_eq!(process.parent_span_id, server.span_context.span_id());
    assert!(!process.parent_span_is_remote);
}

/// A StreamSession on `channel` as `INITIATOR`, with the client traceparent.
struct Stream {
    tx: tokio::sync::mpsc::Sender<StreamSessionRequest>,
    inbound: tonic::Streaming<macp_runtime::pb::StreamSessionResponse>,
}

impl Stream {
    async fn open(channel: &Channel) -> Self {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let inbound = MacpRuntimeServiceClient::new(channel.clone())
            .stream_session(authed(
                tokio_stream::wrappers::ReceiverStream::new(rx),
                INITIATOR,
                Some(&traceparent()),
            ))
            .await
            .unwrap()
            .into_inner();
        Self { tx, inbound }
    }

    async fn send(&self, env: Envelope) {
        self.tx
            .send(StreamSessionRequest {
                envelope: Some(env),
                ..Default::default()
            })
            .await
            .unwrap();
    }

    /// The next echoed envelope's message_id.
    async fn echoed(&mut self) -> String {
        use macp_runtime::pb::stream_session_response::Response;
        match self.inbound.message().await.unwrap().unwrap().response {
            Some(Response::Envelope(e)) => e.message_id,
            other => panic!("expected an envelope, got {other:?}"),
        }
    }

    async fn close(self) {
        let Self { tx, mut inbound } = self;
        drop(tx);
        while let Ok(Some(_)) = inbound.message().await {}
    }
}

// AC 2 (StreamSession): both envelopes on one stream are children of the
// stream-open SERVER span.
#[tokio::test]
async fn stream_envelopes_are_children_of_the_stream_open_server_span() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    let mut stream = Stream::open(&channel).await;
    stream.send(start(DECISION, "m1", SID)).await;
    assert_eq!(stream.echoed().await, "m1");
    stream.send(proposal("m2", SID)).await;
    assert_eq!(stream.echoed().await, "m2");
    stream.close().await;

    let server = pipeline.wait_for_server(1).await;
    assert_eq!(server.len(), 1, "{server:?}");
    let server = &server[0];
    assert_eq!(server.name, "macp.v1.MACPRuntimeService/StreamSession");
    let process = pipeline.process();
    assert_eq!(process.len(), 2, "{process:?}");
    for span in &process {
        assert_eq!(
            span.parent_span_id,
            server.span_context.span_id(),
            "{span:?}"
        );
        assert_eq!(
            span.span_context.trace_id(),
            TraceId::from_hex(TRACE_ID).unwrap()
        );
        assert_eq!(
            attr_str(span, "macp.envelope.outcome").as_deref(),
            Some("accepted")
        );
        assert_eq!(attr_str(span, "macp.sender.id").as_deref(), Some(INITIATOR));
    }
}

// AC 3: a `traceparent` inside `SessionStartPayload.extensions` is never
// read (RFC-MACP-0006 §2.1 MUST NOT).
#[tokio::test]
async fn traceparent_in_extensions_is_ignored() {
    const T2: &str = "0af7651916cd43dd8448eb211c80319c";
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    let mut extensions = HashMap::new();
    extensions.insert(
        "traceparent".to_string(),
        format!("00-{T2}-b7ad6b7169203331-01").into_bytes(),
    );
    let env = envelope(
        DECISION,
        "SessionStart",
        "m1",
        SID,
        start_payload(extensions),
    );
    assert!(send(&channel, env).await.ok);
    pipeline.wait_for_server(1).await;
    let all = pipeline.all();
    assert_eq!(all.len(), 2, "{all:?}");
    let t2 = TraceId::from_hex(T2).unwrap();
    for span in &all {
        assert_ne!(span.span_context.trace_id(), t2, "{span:?}");
    }
    let process = pipeline.process();
    assert_eq!(
        process[0].parent_span_id,
        pipeline.server()[0].span_context.span_id()
    );
}

// AC 4: a rejection records the code, leaves the status Unset, and carries
// no error message or rejected-payload text.
#[tokio::test]
async fn rejection_records_the_code_only() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    assert!(send(&channel, start(DECISION, "m1", SID)).await.ok);
    let ack = send_as(&channel, OUTSIDER, vote("m3", SID, SENTINEL_VOTE), None).await;
    let error = ack.error.unwrap();
    assert_eq!(error.code, "FORBIDDEN");
    assert!(!error.message.is_empty());

    let spans = pipeline.process();
    let span = by_message_id(&spans, "m3")[0];
    assert_eq!(attr_str(span, "error.type").as_deref(), Some("FORBIDDEN"));
    assert_eq!(
        attr_str(span, "macp.envelope.outcome").as_deref(),
        Some("rejected")
    );
    assert_eq!(span.status, Status::Unset);
    assert_eq!(attr_str(span, "macp.sender.id").as_deref(), Some(OUTSIDER));
    let rendered = format!("{:?}", pipeline.all());
    assert!(!rendered.contains(SENTINEL_VOTE), "{rendered}");
    assert!(
        !rendered.contains(&error.message),
        "message {:?} leaked",
        error.message
    );
}

// AC 5: payload and reason text never reach a span (names, attributes,
// events, links, status), including the CancelSession reason the runtime
// logs at info.
#[tokio::test]
async fn payload_and_reason_text_never_reach_telemetry() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    assert!(send(&channel, start(DECISION, "m1", SID)).await.ok);
    assert!(send(&channel, proposal("m2", SID)).await.ok);
    MacpRuntimeServiceClient::new(channel.clone())
        .cancel_session(authed(
            CancelSessionRequest {
                session_id: SID.into(),
                reason: SENTINEL_REASON.into(),
            },
            INITIATOR,
            None,
        ))
        .await
        .unwrap();
    let all = pipeline.wait_for_server(3).await;
    assert_eq!(all.len(), 3, "{all:?}");
    let all = pipeline.all();
    assert_eq!(pipeline.process().len(), 2);
    for span in &all {
        let rendered = format!("{span:?}");
        for sentinel in [SENTINEL_PAYLOAD, SENTINEL_REASON] {
            assert!(!rendered.contains(sentinel), "{sentinel} in {rendered}");
        }
        assert!(span.events.is_empty() && span.links.is_empty(), "{span:?}");
    }
}

/// `(sorted attributes, value)` of every `macp.envelope.processed` point.
fn processed_points(rm: &ResourceMetrics) -> Vec<(Vec<(String, String)>, u64)> {
    let mut out = Vec::new();
    for sm in rm.scope_metrics() {
        for m in sm.metrics() {
            if m.name() != "macp.envelope.processed" {
                continue;
            }
            let AggregatedMetrics::U64(MetricData::Sum(sum)) = m.data() else {
                panic!("macp.envelope.processed is not a u64 Sum");
            };
            for p in sum.data_points() {
                let mut attrs: Vec<(String, String)> = p
                    .attributes()
                    .map(|kv| (kv.key.to_string(), kv.value.as_str().into_owned()))
                    .collect();
                attrs.sort();
                out.push((attrs, p.value()));
            }
        }
    }
    out
}

// AC 6: exactly the two semconv Envelope instruments, with the file's names,
// instruments and units, and attribute keys from its refs (no identifier).
#[tokio::test]
async fn envelope_metrics_match_the_semconv() {
    let semconv = Semconv::load();
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    scripted_decision(&channel).await;
    let rm = pipeline.collect();

    let metrics: Vec<_> = rm.scope_metrics().flat_map(|sm| sm.metrics()).collect();
    let mut names: Vec<&str> = metrics.iter().map(|m| m.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["macp.envelope.process.duration", "macp.envelope.processed"]
    );
    let identifiers = ["macp.session.id", "macp.message.id", "macp.sender.id"];
    for m in &metrics {
        let group = semconv
            .metrics
            .iter()
            .find(|g| g.metric_name == m.name())
            .unwrap_or_else(|| panic!("{} not in the semconv", m.name()));
        assert_eq!(m.unit(), group.unit, "{}", m.name());
        let refs: BTreeSet<&str> = group.attributes.iter().map(|a| a.id.as_str()).collect();
        let check_keys = |keys: Vec<String>| {
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
                    check_keys(p.attributes().map(|kv| kv.key.to_string()).collect());
                }
            }
            ("histogram", AggregatedMetrics::F64(MetricData::Histogram(h))) => {
                for p in h.data_points() {
                    assert_eq!(p.bounds().collect::<Vec<_>>(), ENVELOPE_DURATION_BOUNDARIES);
                    check_keys(p.attributes().map(|kv| kv.key.to_string()).collect());
                }
            }
            (instrument, data) => panic!("{}: {instrument} recorded as {data:?}", m.name()),
        }
    }

    // One point per (mode, type, outcome); the duplicate counted apart.
    let points = processed_points(&rm);
    let total: u64 = points.iter().map(|(_, v)| v).sum();
    assert_eq!(total, 6, "{points:?}");
    let kv = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        let mut v: Vec<_> = pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        v.sort();
        v
    };
    for (attrs, value) in [
        (
            kv(&[
                ("macp.mode.id", DECISION),
                ("macp.message.type", "Proposal"),
                ("macp.envelope.outcome", "duplicate"),
            ]),
            1,
        ),
        (
            kv(&[
                ("macp.message.type", "Signal"),
                ("macp.envelope.outcome", "accepted"),
            ]),
            1,
        ),
        (
            kv(&[
                ("macp.mode.id", DECISION),
                ("macp.message.type", "Vote"),
                ("macp.envelope.outcome", "rejected"),
            ]),
            1,
        ),
    ] {
        assert!(
            points.contains(&(attrs.clone(), value)),
            "{attrs:?} in {points:?}"
        );
    }
}

fn labels() -> EnvelopeLabels {
    EnvelopeLabels::new(DECISION, "Vote", Some(&["Vote".to_string()].into()))
}

fn assert_unfinished(span: &SpanData) {
    assert_eq!(span.status, Status::error("INTERNAL_ERROR"), "{span:?}");
    assert!(attr(span, "macp.envelope.outcome").is_none(), "{span:?}");
    // An unknown failure: OpenTelemetry's `_OTHER` (the semconv requires
    // `error.type` when the operation failed).
    assert_eq!(
        attr_str(span, "error.type").as_deref(),
        Some("_OTHER"),
        "{span:?}"
    );
}

// AC 8 (unit): a `ProcessSpan` dropped without `finish` is ERROR with
// `INTERNAL_ERROR`, no outcome, and no metric point anywhere.
#[test]
fn unfinished_process_span_is_an_internal_error_without_a_metric() {
    let (pipeline, telemetry) = Pipeline::install();
    let metrics = RuntimeMetrics::new();
    drop(ProcessSpan::start(
        &telemetry,
        &vote("m1", SID, "r"),
        labels(),
        None,
    ));
    let spans = pipeline.process();
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_unfinished(&spans[0]);
    let mut prom = String::new();
    metrics.semconv_prometheus_lines(&mut prom);
    assert!(prom.is_empty(), "{prom}");
    // With nothing recorded the reader may export no collection at all.
    pipeline.meter.force_flush().unwrap();
    for rm in pipeline.metrics.get_finished_metrics().unwrap() {
        assert!(processed_points(&rm).is_empty());
    }
}

// AC 8 (in-process): a panic inside the instrumented block is caught; the
// span still ends as an implementation failure.
#[test]
fn panic_inside_a_process_span_marks_it_an_internal_error() {
    let (pipeline, telemetry) = Pipeline::install();
    let metrics = RuntimeMetrics::new();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let process = ProcessSpan::start(&telemetry, &vote("m1", SID, "r"), labels(), None);
        process.span().in_scope(|| panic!("boom inside process"));
        process.finish(&metrics, Outcome::Accepted);
    }));
    assert!(caught.is_err());
    let spans = pipeline.process();
    assert_eq!(spans.len(), 1, "{spans:?}");
    assert_unfinished(&spans[0]);
    assert!(!format!("{spans:?}").contains("boom"));
    let mut prom = String::new();
    metrics.semconv_prometheus_lines(&mut prom);
    assert!(prom.is_empty(), "{prom}");
}

// AC 9: on a stream, a re-sent envelope is `duplicate`, and an envelope for a
// resolved session records the carried code, INVALID_ENVELOPE.
#[tokio::test]
async fn stream_records_duplicate_and_the_not_open_code() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve(telemetry).await;
    let mut stream = Stream::open(&channel).await;
    stream.send(start(DECISION, "m1", SID)).await;
    assert_eq!(stream.echoed().await, "m1");
    stream.send(start(DECISION, "m1", SID)).await; // duplicate: no echo
    stream.send(proposal("m2", SID)).await;
    assert_eq!(stream.echoed().await, "m2");
    stream.send(vote("m3", SID, "ok")).await;
    assert_eq!(stream.echoed().await, "m3");
    stream.send(commitment("m4", SID)).await;
    assert_eq!(stream.echoed().await, "m4");
    stream.send(proposal("m5", SID)).await; // session resolved
    let status = loop {
        match stream.inbound.message().await {
            Ok(Some(_)) => continue,
            Ok(None) => panic!("stream ended without an error"),
            Err(status) => break status,
        }
    };
    assert_eq!(
        status.metadata().get("macp-error-code").unwrap(),
        "INVALID_ENVELOPE"
    );

    let spans = pipeline.process();
    let m1: Vec<_> = by_message_id(&spans, "m1")
        .into_iter()
        .map(|s| attr_str(s, "macp.envelope.outcome").unwrap())
        .collect();
    assert_eq!(m1, ["accepted", "duplicate"]);
    let m5 = by_message_id(&spans, "m5")[0];
    assert_eq!(
        attr_str(m5, "macp.envelope.outcome").as_deref(),
        Some("rejected")
    );
    assert_eq!(
        attr_str(m5, "error.type").as_deref(),
        Some("INVALID_ENVELOPE")
    );
    assert_eq!(m5.status, Status::Unset);
    assert!(!format!("{m5:?}").contains("SESSION_NOT_OPEN"));
}

/// A backend whose log append always fails.
struct FailingBackend;

#[async_trait::async_trait]
impl StorageBackend for FailingBackend {
    async fn save_session(&self, _: &Session) -> std::io::Result<()> {
        Ok(())
    }
    async fn load_session(&self, _: &str) -> std::io::Result<Option<Session>> {
        Ok(None)
    }
    async fn load_all_sessions(&self) -> std::io::Result<Vec<Session>> {
        Ok(vec![])
    }
    async fn delete_session(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    async fn list_session_ids(&self) -> std::io::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn append_log_entry(&self, _: &str, _: &LogEntry) -> std::io::Result<()> {
        Err(std::io::Error::other("disk full SENTINEL-IO-5e21"))
    }
    async fn load_log(&self, _: &str) -> std::io::Result<Vec<LogEntry>> {
        Ok(vec![])
    }
    async fn create_session_storage(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

// An implementation failure (storage) is ERROR with the code only; a
// rejection never is.
#[tokio::test]
async fn storage_failure_is_an_error_status_with_the_code_only() {
    let (pipeline, telemetry) = Pipeline::install();
    let channel = serve_runtime(runtime(Arc::new(FailingBackend), telemetry)).await;
    let ack = send(&channel, start(DECISION, "m1", SID)).await;
    assert_eq!(ack.error.unwrap().code, "INTERNAL_ERROR");
    let spans = pipeline.process();
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!(span.status, Status::error("INTERNAL_ERROR"));
    assert_eq!(
        attr_str(span, "error.type").as_deref(),
        Some("INTERNAL_ERROR")
    );
    assert_eq!(
        attr_str(span, "macp.envelope.outcome").as_deref(),
        Some("rejected")
    );
    assert!(!format!("{spans:?}").contains("SENTINEL-IO"));
}

// Inactive telemetry: no span at all, the same acks, and the Prometheus
// families still count (they are always on).
#[tokio::test]
async fn inactive_telemetry_creates_no_process_span_and_still_counts() {
    let (pipeline, _) = Pipeline::install();
    let rt = Arc::new(runtime(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Telemetry::disabled(),
    ));
    let server = MacpServer::new(Arc::clone(&rt), SecurityLayer::dev_mode());
    use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeService;
    let ack = server
        .send(authed(
            SendRequest {
                envelope: Some(start(DECISION, "m1", SID)),
            },
            INITIATOR,
            None,
        ))
        .await
        .unwrap()
        .into_inner()
        .ack
        .unwrap();
    assert!(ack.ok);
    assert!(pipeline.all().is_empty(), "{:?}", pipeline.all());
    let mut prom = String::new();
    rt.metrics().semconv_prometheus_lines(&mut prom);
    assert!(prom.contains(
        "macp_envelope_processed_total{macp_mode_id=\"macp.mode.decision.v1\",\
         macp_message_type=\"SessionStart\",macp_envelope_outcome=\"accepted\"} 1"
    ));
}
