//! A minimal fake OTLP/gRPC collector for the `otel` tests.
//!
//! It serves `opentelemetry.proto.collector.trace.v1.TraceService/Export`
//! and `opentelemetry.proto.collector.metrics.v1.MetricsService/Export` on
//! `127.0.0.1:<ephemeral>`, plaintext or TLS, answers every export OK (so the
//! exporter never retries), and records every request it receives. Nothing is ever forwarded. Shared by `tests/otel_bootstrap.rs`
//! (P1) and the end-to-end export tests that come later; include it with
//! `#[path = "support/fake_collector.rs"] mod fake_collector;`.
#![allow(dead_code)] // each including test binary uses a different subset

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_server::{
    MetricsService, MetricsServiceServer,
};
use opentelemetry_proto::tonic::collector::metrics::v1::{
    ExportMetricsServiceRequest, ExportMetricsServiceResponse,
};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use opentelemetry_proto::tonic::metrics::v1::Metric;
use opentelemetry_proto::tonic::trace::v1::Span;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Identity, Server, ServerTlsConfig};

#[derive(Clone, Default)]
struct Recorder {
    requests: Arc<Mutex<Vec<ExportTraceServiceRequest>>>,
    metric_requests: Arc<Mutex<Vec<ExportMetricsServiceRequest>>>,
}

#[tonic::async_trait]
impl TraceService for Recorder {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        self.requests.lock().unwrap().push(request.into_inner());
        Ok(tonic::Response::new(ExportTraceServiceResponse {
            partial_success: None,
        }))
    }
}

#[tonic::async_trait]
impl MetricsService for Recorder {
    async fn export(
        &self,
        request: tonic::Request<ExportMetricsServiceRequest>,
    ) -> Result<tonic::Response<ExportMetricsServiceResponse>, tonic::Status> {
        self.metric_requests
            .lock()
            .unwrap()
            .push(request.into_inner());
        Ok(tonic::Response::new(ExportMetricsServiceResponse {
            partial_success: None,
        }))
    }
}

/// A running fake collector. Stops when dropped.
pub struct FakeCollector {
    addr: SocketAddr,
    tls: bool,
    recorder: Recorder,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeCollector {
    /// Start a plaintext collector (`http://127.0.0.1:<port>`).
    pub async fn start() -> Self {
        Self::spawn(None).await
    }

    /// Start a TLS collector (`https://127.0.0.1:<port>`) serving the given
    /// PEM certificate chain and key.
    pub async fn start_tls(cert_pem: &str, key_pem: &str) -> Self {
        Self::spawn(Some(Identity::from_pem(cert_pem, key_pem))).await
    }

    async fn spawn(identity: Option<Identity>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake collector");
        let addr = listener.local_addr().unwrap();
        let recorder = Recorder::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tls = identity.is_some();
        let mut builder = Server::builder();
        if let Some(identity) = identity {
            builder = builder
                .tls_config(ServerTlsConfig::new().identity(identity))
                .expect("fake collector TLS config");
        }
        let router = builder
            .add_service(TraceServiceServer::new(recorder.clone()))
            .add_service(MetricsServiceServer::new(recorder.clone()));
        tokio::spawn(async move {
            let _ = router
                .serve_with_incoming_shutdown(TcpIncoming::from(listener), async {
                    let _ = rx.await;
                })
                .await;
        });
        Self {
            addr,
            tls,
            recorder,
            shutdown: Some(tx),
        }
    }

    /// The URL to put in `OTEL_EXPORTER_OTLP_*ENDPOINT`.
    pub fn endpoint(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.addr)
    }

    /// Every metrics export request received so far.
    pub fn metric_requests(&self) -> Vec<ExportMetricsServiceRequest> {
        self.recorder.metric_requests.lock().unwrap().clone()
    }

    /// Every metric data point batch received so far, across requests.
    pub fn metrics(&self) -> Vec<Metric> {
        self.metric_requests()
            .into_iter()
            .flat_map(|r| r.resource_metrics)
            .flat_map(|rm| rm.scope_metrics)
            .flat_map(|sm| sm.metrics)
            .collect()
    }

    /// Every trace export request received so far.
    pub fn requests(&self) -> Vec<ExportTraceServiceRequest> {
        self.recorder.requests.lock().unwrap().clone()
    }

    /// Every span received so far, across requests.
    pub fn spans(&self) -> Vec<Span> {
        self.requests()
            .into_iter()
            .flat_map(|r| r.resource_spans)
            .flat_map(|rs| rs.scope_spans)
            .flat_map(|ss| ss.spans)
            .collect()
    }

    /// Poll until at least `n` spans arrived or `timeout` passes; returns the
    /// spans received either way.
    pub async fn wait_for_spans(&self, n: usize, timeout: Duration) -> Vec<Span> {
        let deadline = Instant::now() + timeout;
        loop {
            let spans = self.spans();
            if spans.len() >= n || Instant::now() >= deadline {
                return spans;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl Drop for FakeCollector {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}
