//! The gRPC SERVER span (R2 P2), through a real tonic server on
//! `127.0.0.1:0` built exactly as `src/main.rs` builds it: the layer from
//! `telemetry::grpc::optional_server_layer`, the health service and the MACP
//! service. Spans go to an in-memory exporter through
//! `telemetry::init::active_for_tests`, installed as the thread's default
//! dispatcher. Each test runs on tokio's current-thread runtime, so the server
//! and connection tasks it spawns are polled on that same thread and see that
//! dispatcher.
//!
//! The global text-map propagator: extraction goes through
//! `opentelemetry::global`, as in production. Every test that needs it calls
//! `install_propagator`, which sets the W3C `TraceContextPropagator` once per
//! process (`Once`). That is the value `telemetry::init::init` installs, it is
//! never changed back, and this test binary is its own process, so nothing
//! leaks into other test binaries and no test here depends on another's order.

#![cfg(feature = "otel")]

use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use macp_runtime::log_store::LogStore;
use macp_runtime::pb::macp_runtime_service_client::MacpRuntimeServiceClient;
use macp_runtime::pb::macp_runtime_service_server::MacpRuntimeServiceServer;
use macp_runtime::pb::{
    Envelope, GetSessionRequest, GetSessionResponse, SendRequest, SendResponse,
    SessionStartPayload, StreamSessionRequest, WatchSignalsRequest, WatchSignalsResponse,
};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::telemetry::{grpc, init, Telemetry};
use opentelemetry::trace::{SpanId, SpanKind, Status, TraceId};
use opentelemetry::Value;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use prost::Message;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

const SID: &str = "0b7c1a3e-2f4d-4c6b-9a8e-1d2c3b4a5f60";
const UNKNOWN_SID: &str = "7d1e2f3a-4b5c-4d6e-8f70-112233445566";
const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const PARENT_ID: &str = "00f067aa0ba902b7";
const SERVICE: &str = "macp.v1.MACPRuntimeService";

fn install_propagator() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    });
}

/// The in-memory pipeline of one test.
struct Spans {
    exporter: InMemorySpanExporter,
    // Kept alive for the test: dropping the provider shuts it down.
    _provider: SdkTracerProvider,
    _guard: tracing::dispatcher::DefaultGuard,
}

impl Spans {
    /// Installs an active telemetry pipeline as this thread's dispatcher.
    fn install() -> (Self, Telemetry) {
        install_propagator();
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let (dispatch, telemetry) = init::active_for_tests(&provider);
        let guard = tracing::dispatcher::set_default(&dispatch);
        (
            Self {
                exporter,
                _provider: provider,
                _guard: guard,
            },
            telemetry,
        )
    }

    fn finished(&self) -> Vec<SpanData> {
        self.exporter.get_finished_spans().unwrap()
    }

    /// Wait (yielding to the server tasks) until `n` spans have ended. A
    /// span ends when tower-http drops its response body, a moment after the
    /// client has the response.
    async fn wait_for(&self, n: usize) -> Vec<SpanData> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let spans = self.finished();
            if spans.len() >= n || Instant::now() > deadline {
                return spans;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Serve the health and MACP services behind
/// `optional_server_layer(&telemetry)`, as `src/main.rs` does.
async fn serve(telemetry: Telemetry) -> Channel {
    let runtime = Runtime::new(
        Arc::new(macp_runtime::storage::MemoryBackend),
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_clock(Arc::new(macp_runtime::macp_core::ManualClock::new(
        1_800_000_000_000,
    )))
    .with_telemetry(telemetry.clone());
    let svc = MacpServer::new(Arc::new(runtime), SecurityLayer::dev_mode());
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<MacpRuntimeServiceServer<MacpServer>>()
        .await;
    let incoming = TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = incoming.local_addr().unwrap();
    let router = Server::builder()
        .layer(grpc::optional_server_layer(&telemetry))
        .add_service(health_service)
        .add_service(MacpRuntimeServiceServer::new(svc));
    tokio::spawn(router.serve_with_incoming(incoming));
    tonic::transport::Endpoint::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .unwrap()
}

fn start_envelope() -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: "macp.mode.decision.v1".into(),
        message_type: "SessionStart".into(),
        message_id: "m1".into(),
        session_id: SID.into(),
        sender: String::new(),
        timestamp_unix_ms: 1_800_000_000_000,
        payload: SessionStartPayload {
            intent: "intent".into(),
            participants: vec!["agent://a".into()],
            mode_version: "1.0.0".into(),
            configuration_version: "cfg-1".into(),
            ttl_ms: 60_000,
            ..Default::default()
        }
        .encode_to_vec(),
    }
}

fn authed<T>(msg: T, traceparent: Option<&str>) -> tonic::Request<T> {
    let mut req = tonic::Request::new(msg);
    req.metadata_mut()
        .insert("authorization", "Bearer agent://a".parse().unwrap());
    if let Some(tp) = traceparent {
        req.metadata_mut()
            .insert("traceparent", tp.parse().unwrap());
        req.metadata_mut()
            .insert("tracestate", "vendor=opaque".parse().unwrap());
    }
    req
}

fn traceparent() -> String {
    format!("00-{TRACE_ID}-{PARENT_ID}-01")
}

async fn send_start(channel: Channel, traceparent: Option<&str>) -> SendResponse {
    MacpRuntimeServiceClient::new(channel)
        .send(authed(
            SendRequest {
                envelope: Some(start_envelope()),
            },
            traceparent,
        ))
        .await
        .unwrap()
        .into_inner()
}

async fn get_unknown_session(channel: Channel) -> tonic::Status {
    MacpRuntimeServiceClient::new(channel)
        .get_session(authed(
            GetSessionRequest {
                session_id: UNKNOWN_SID.into(),
            },
            None,
        ))
        .await
        .unwrap_err()
}

/// A method the service does not define, so tonic answers UNIMPLEMENTED.
async fn call_unknown_method(channel: Channel) -> tonic::Status {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.unwrap();
    grpc.unary::<_, GetSessionResponse, _>(
        authed(
            GetSessionRequest {
                session_id: SID.into(),
            },
            None,
        ),
        http::uri::PathAndQuery::from_static("/macp.v1.MACPRuntimeService/NoSuchMethod"),
        tonic_prost::ProstCodec::default(),
    )
    .await
    .unwrap_err()
}

async fn health_check(channel: Channel) {
    use tonic_health::pb::health_check_response::ServingStatus;
    use tonic_health::pb::health_client::HealthClient;
    use tonic_health::pb::HealthCheckRequest;
    let resp = HealthClient::new(channel)
        .check(HealthCheckRequest {
            service: String::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.status, ServingStatus::Serving as i32);
}

fn attr<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| &kv.value)
}

fn assert_server_span(span: &SpanData, method: &'static str, status_code: i64) {
    assert_eq!(span.name, format!("{SERVICE}/{method}"));
    assert_eq!(span.span_kind, SpanKind::Server);
    assert_eq!(attr(span, "rpc.system"), Some(&Value::from("grpc")));
    assert_eq!(attr(span, "rpc.service"), Some(&Value::from(SERVICE)));
    assert_eq!(attr(span, "rpc.method"), Some(&Value::from(method)));
    assert_eq!(
        attr(span, "rpc.grpc.status_code"),
        Some(&Value::I64(status_code)),
        "{span:?}"
    );
    // Nothing but the declared attributes: no peer address, no target, no
    // message text.
    let mut keys: Vec<&str> = span.attributes.iter().map(|kv| kv.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "rpc.grpc.status_code",
            "rpc.method",
            "rpc.service",
            "rpc.system"
        ]
    );
    // tower-http's own log events are off and no `tracing` event is ever
    // exported: a SERVER span carries no span events.
    assert!(span.events.is_empty(), "{:?}", span.events);
}

// AC 1: a client `traceparent` makes the SERVER span a child of that remote
// context.
#[tokio::test]
async fn client_traceparent_parents_the_server_span() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    let resp = send_start(channel, Some(&traceparent())).await;
    assert!(resp.ack.unwrap().ok);

    let finished = spans.wait_for(1).await;
    assert_eq!(finished.len(), 1, "{finished:?}");
    let span = &finished[0];
    assert_server_span(span, "Send", 0);
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex(TRACE_ID).unwrap()
    );
    assert_eq!(span.parent_span_id, SpanId::from_hex(PARENT_ID).unwrap());
    assert!(span.parent_span_is_remote);
    assert_ne!(
        span.span_context.span_id(),
        SpanId::from_hex(PARENT_ID).unwrap()
    );
    assert_eq!(span.status, Status::Unset);
}

// AC 2: no `traceparent`, or a malformed one, gives a new root.
#[tokio::test]
async fn no_or_malformed_traceparent_gives_a_new_root() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    assert!(send_start(channel.clone(), None).await.ack.unwrap().ok);
    // Same envelope again: a duplicate, still an OK RPC.
    send_start(channel, Some("00-not-a-valid-traceparent")).await;

    let finished = spans.wait_for(2).await;
    assert_eq!(finished.len(), 2, "{finished:?}");
    for span in &finished {
        assert_server_span(span, "Send", 0);
        assert_eq!(span.parent_span_id, SpanId::INVALID, "{span:?}");
        assert!(!span.parent_span_is_remote);
        assert_ne!(
            span.span_context.trace_id(),
            TraceId::from_hex(TRACE_ID).unwrap()
        );
    }
    assert_ne!(
        finished[0].span_context.trace_id(),
        finished[1].span_context.trace_id(),
        "each root starts its own trace"
    );
}

// AC 3: a health check exports no span. The Send after it proves the
// pipeline was live (the check is not vacuous).
#[tokio::test]
async fn health_check_exports_no_span() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    health_check(channel.clone()).await;
    health_check(channel.clone()).await;
    send_start(channel, None).await;

    let finished = spans.wait_for(1).await;
    // Give a straggling health span every chance to show up.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let finished_later = spans.finished();
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_eq!(finished_later.len(), 1, "{finished_later:?}");
    assert_server_span(&finished_later[0], "Send", 0);
}

// AC 4: a protocol outcome such as NOT_FOUND records the code and leaves the
// status Unset.
#[tokio::test]
async fn not_found_records_the_code_with_status_unset() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    let status = get_unknown_session(channel).await;
    assert_eq!(status.code(), tonic::Code::NotFound, "{status:?}");

    let finished = spans.wait_for(1).await;
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_server_span(&finished[0], "GetSession", 5);
    assert_eq!(finished[0].status, Status::Unset);
}

// AC 5 (through the layer): UNIMPLEMENTED is a SERVER-side error. The method
// is outside the service's set, so the span name and `rpc.method` use
// `_OTHER`, and the description is the code name only.
#[tokio::test]
async fn unimplemented_method_is_an_error_named_other() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    let status = call_unknown_method(channel).await;
    assert_eq!(status.code(), tonic::Code::Unimplemented, "{status:?}");

    let finished = spans.wait_for(1).await;
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_server_span(&finished[0], grpc::OTHER_METHOD, 12);
    assert_eq!(finished[0].status, Status::error("UNIMPLEMENTED"));
}

// A streaming RPC: the span stays open while the stream does, and ends
// when the stream ends.
#[tokio::test]
async fn stream_session_span_lasts_until_the_stream_ends() {
    let (spans, telemetry) = Spans::install();
    let channel = serve(telemetry).await;
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let mut client = MacpRuntimeServiceClient::new(channel);
    let mut inbound = client
        .stream_session(authed(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            Some(&traceparent()),
        ))
        .await
        .unwrap()
        .into_inner();
    tx.send(StreamSessionRequest {
        envelope: Some(start_envelope()),
        ..Default::default()
    })
    .await
    .unwrap();
    let first = inbound.message().await.unwrap();
    assert!(first.is_some(), "the accepted SessionStart is echoed");

    // The handler has answered and a message went out, yet the stream is
    // open: no span has ended.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(spans.finished().is_empty(), "{:?}", spans.finished());

    drop(tx);
    while let Ok(Some(_)) = inbound.message().await {}
    drop(inbound);
    let finished = spans.wait_for(1).await;
    assert_eq!(finished.len(), 1, "{finished:?}");
    let span = &finished[0];
    // The client closed its side, the runtime ended the stream with an OK
    // status in the trailers, and `on_eos` recorded it.
    assert_server_span(span, "StreamSession", 0);
    assert_eq!(
        span.span_context.trace_id(),
        TraceId::from_hex(TRACE_ID).unwrap()
    );
    assert_eq!(span.parent_span_id, SpanId::from_hex(PARENT_ID).unwrap());
    assert_eq!(span.status, Status::Unset);
}

/// The responses one server gives to the comparison calls, with
/// `accepted_at_unix_ms` cleared.
async fn responses(channel: Channel) -> (SendResponse, tonic::Code, tonic::Code) {
    let mut send = send_start(channel.clone(), Some(&traceparent())).await;
    if let Some(ack) = send.ack.as_mut() {
        ack.accepted_at_unix_ms = 0;
    }
    let not_found = get_unknown_session(channel.clone()).await.code();
    let unimplemented = call_unknown_method(channel.clone()).await.code();
    health_check(channel).await;
    (send, not_found, unimplemented)
}

// Inactive telemetry installs the identity layer: no span is exported even
// with a live OpenTelemetry dispatcher, and the responses are those of an
// active server.
#[tokio::test]
async fn inactive_telemetry_exports_nothing_and_answers_identically() {
    let (spans, active) = Spans::install();

    let inactive_responses = responses(serve(Telemetry::disabled()).await).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(spans.finished().is_empty(), "{:?}", spans.finished());

    let active_responses = responses(serve(active).await).await;
    assert_eq!(spans.wait_for(3).await.len(), 3);

    assert!(inactive_responses.0.ack.as_ref().unwrap().ok);
    assert_eq!(inactive_responses, active_responses);
}

/// A server-streaming handler that sends one message and then fails with
/// UNAVAILABLE, which tonic encodes in the trailers after the headers and the
/// data frame have gone out.
struct FailsAfterOneMessage;

impl tonic::server::ServerStreamingService<WatchSignalsRequest> for FailsAfterOneMessage {
    type Response = WatchSignalsResponse;
    type ResponseStream =
        tokio_stream::Iter<std::vec::IntoIter<Result<WatchSignalsResponse, tonic::Status>>>;
    type Future = std::future::Ready<Result<tonic::Response<Self::ResponseStream>, tonic::Status>>;

    fn call(&mut self, _: tonic::Request<WatchSignalsRequest>) -> Self::Future {
        std::future::ready(Ok(tonic::Response::new(tokio_stream::iter(vec![
            Ok(WatchSignalsResponse::default()),
            Err(tonic::Status::unavailable("backend gone")),
        ]))))
    }
}

// An error-set code that arrives in the trailers (not trailers-only) sets
// ERROR through `on_eos`. The MACP service has no path that ends a stream
// with an error-set code on demand, so the layer wraps a tonic
// server-streaming handler mounted at a MACP path directly.
#[tokio::test]
async fn error_code_in_trailers_sets_error_through_on_eos() {
    use tonic::codegen::Body as _;
    use tower::{Layer as _, ServiceExt as _};

    let (spans, _) = Spans::install();
    let inner = tower::service_fn(|req: http::Request<tonic::body::Body>| async move {
        let mut grpc = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
            WatchSignalsResponse,
            WatchSignalsRequest,
        >::default());
        Ok::<_, std::convert::Infallible>(grpc.server_streaming(FailsAfterOneMessage, req).await)
    });
    // One gRPC frame holding the empty `WatchSignalsRequest`: uncompressed
    // flag 0 and length 0.
    let request = http::Request::builder()
        .method("POST")
        .uri("/macp.v1.MACPRuntimeService/WatchSignals")
        .header("content-type", "application/grpc")
        .header("traceparent", traceparent())
        .body(tonic::body::Body::new(String::from("\0\0\0\0\0")))
        .unwrap();
    let response = grpc::server_layer()
        .layer(inner)
        .oneshot(request)
        .await
        .unwrap();
    assert!(
        response.headers().get("grpc-status").is_none(),
        "not trailers-only: {:?}",
        response.headers()
    );
    let mut body = response.into_body();
    let mut trailers = None;
    let mut data_frames = 0;
    while let Some(frame) =
        std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await
    {
        let frame = frame.unwrap();
        if frame.is_data() {
            data_frames += 1;
        } else if let Ok(t) = frame.into_trailers() {
            trailers = Some(t);
        }
    }
    assert!(data_frames >= 1, "a message went out before the failure");
    assert_eq!(trailers.unwrap().get("grpc-status").unwrap(), "14");
    drop(body);

    let finished = spans.wait_for(1).await;
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_server_span(&finished[0], "WatchSignals", 14);
    assert_eq!(finished[0].status, Status::error("UNAVAILABLE"));
}
