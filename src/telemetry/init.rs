//! Telemetry bootstrap (`otel` feature only): activation, resource,
//! propagator, OTLP exporter, the subscriber layout and bounded shutdown.
//!
//! Nothing here aborts startup. A misconfiguration logs one `error!` naming
//! the variable and leaves telemetry inactive; telemetry never decides whether
//! the runtime runs.

use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::tonic_types::transport::{Certificate, ClientTlsConfig};
use opentelemetry_otlp::{WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing::Subscriber;
use tracing_subscriber::filter::{filter_fn, FilterExt, LevelFilter};
use tracing_subscriber::layer::{Filter, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use super::{non_empty, Telemetry, TARGET};

/// `service.name` when neither `OTEL_SERVICE_NAME` nor
/// `OTEL_RESOURCE_ATTRIBUTES` names one (the SDK default is `unknown_service`).
pub const DEFAULT_SERVICE_NAME: &str = "macp-runtime";

/// The instrumentation scope of every runtime span.
pub const TRACER_NAME: &str = "macp-runtime";

/// Upper bound on provider shutdown, so a dead collector cannot extend
/// `MACP_SHUTDOWN_DRAIN_SECS`.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Which signals export. Inactive means neither.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    pub traces: bool,
    pub metrics: bool,
}

impl Activation {
    pub fn is_active(&self) -> bool {
        self.traces || self.metrics
    }
}

/// Decide activation from `env` (a pure function of the variables).
///
/// Traces export iff the generic or the traces endpoint is set; metrics iff
/// the generic or the metrics endpoint is set. Empty values count as unset,
/// and `OTEL_SDK_DISABLED=true` (any case) turns everything off.
pub fn activation(env: &impl Fn(&str) -> Option<String>) -> Activation {
    if env("OTEL_SDK_DISABLED").is_some_and(|v| v.trim().eq_ignore_ascii_case("true")) {
        return Activation::default();
    }
    let generic = non_empty(env, "OTEL_EXPORTER_OTLP_ENDPOINT").is_some();
    Activation {
        traces: generic || non_empty(env, "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some(),
        metrics: generic || non_empty(env, "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT").is_some(),
    }
}

/// The resource every signal carries.
///
/// Built from an empty resource (the SDK's default builder would yield
/// `service.name=unknown_service`): `service.name=macp-runtime` and
/// `service.version=<crate version>`, then `OTEL_RESOURCE_ATTRIBUTES`, then
/// an explicit `OTEL_SERVICE_NAME`. Later entries win, so
/// `OTEL_SERVICE_NAME` beats a `service.name` in `OTEL_RESOURCE_ATTRIBUTES`.
pub fn resource(env: &impl Fn(&str) -> Option<String>) -> Resource {
    let mut builder = Resource::builder_empty()
        .with_attribute(KeyValue::new("service.name", DEFAULT_SERVICE_NAME))
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")));
    if let Some(attrs) = non_empty(env, "OTEL_RESOURCE_ATTRIBUTES") {
        builder = builder.with_attributes(parse_resource_attributes(&attrs));
    }
    if let Some(name) = non_empty(env, "OTEL_SERVICE_NAME") {
        builder = builder.with_service_name(name);
    }
    builder.build()
}

/// `key1=value1,key2=value2`, parsed exactly as the SDK's
/// `EnvResourceDetector` does (`opentelemetry_sdk-0.32.1/src/resource/env.rs`,
/// `construct_otel_resources`). Re-implemented only so [`resource`] reads the
/// injected `env` rather than the process environment.
fn parse_resource_attributes(s: &str) -> Vec<KeyValue> {
    s.split_terminator(',')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            Some(KeyValue::new(
                key.trim().to_owned(),
                value.trim().to_owned(),
            ))
        })
        .collect()
}

/// One signal's resolved export settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalExport {
    /// The collector endpoint, `http://` or `https://`.
    pub endpoint: String,
    /// A private CA (PEM) to trust instead of the native roots, from
    /// `OTEL_EXPORTER_OTLP_<SIGNAL>_CERTIFICATE` or
    /// `OTEL_EXPORTER_OTLP_CERTIFICATE`.
    pub ca_pem: Option<Vec<u8>>,
}

impl SignalExport {
    fn is_https(&self) -> bool {
        self.endpoint
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    }
}

/// The validated export configuration of an active runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportConfig {
    pub traces: Option<SignalExport>,
    pub metrics: Option<SignalExport>,
}

/// Validate the OTLP variables for `activation` and resolve each signal.
///
/// `Err` names the offending variable. It means "log it and stay inactive",
/// never "abort startup".
pub fn export_config(
    env: &impl Fn(&str) -> Option<String>,
    activation: Activation,
) -> Result<ExportConfig, String> {
    // Only gRPC is compiled in. An exporter told to speak another protocol
    // would fail every export silently, so refuse to start it.
    for var in [
        "OTEL_EXPORTER_OTLP_PROTOCOL",
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
    ] {
        if let Some(protocol) = non_empty(env, var) {
            if protocol.trim() != "grpc" {
                return Err(format!(
                    "OpenTelemetry disabled: {var}='{protocol}' is not supported; this build \
                     exports OTLP over gRPC only (set {var}=grpc or unset it)"
                ));
            }
        }
    }
    let signal = |name: &str| -> Result<SignalExport, String> {
        let specific = format!("OTEL_EXPORTER_OTLP_{name}_ENDPOINT");
        // The exporter's own precedence (`opentelemetry-otlp-0.32.0`
        // `exporter/tonic/mod.rs`, `resolve_endpoint`): the signal-specific
        // variable, then the generic one. The generic one is used as-is for
        // gRPC (no `/v1/<signal>` suffix). No built-in default: activation
        // already required one of them.
        let (var, endpoint) = match non_empty(env, &specific) {
            Some(e) => (specific, e),
            None => (
                "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
                non_empty(env, "OTEL_EXPORTER_OTLP_ENDPOINT").unwrap_or_default(),
            ),
        };
        check_endpoint(&var, &endpoint)?;
        let cert_specific = format!("OTEL_EXPORTER_OTLP_{name}_CERTIFICATE");
        let cert = non_empty(env, &cert_specific)
            .map(|path| (cert_specific, path))
            .or_else(|| {
                non_empty(env, "OTEL_EXPORTER_OTLP_CERTIFICATE")
                    .map(|path| ("OTEL_EXPORTER_OTLP_CERTIFICATE".to_string(), path))
            });
        let ca_pem = match cert {
            Some((var, path)) => Some(std::fs::read(&path).map_err(|e| {
                format!("OpenTelemetry disabled: {var}: cannot read '{path}': {e}")
            })?),
            None => None,
        };
        Ok(SignalExport { endpoint, ca_pem })
    };
    Ok(ExportConfig {
        traces: activation.traces.then(|| signal("TRACES")).transpose()?,
        metrics: activation.metrics.then(|| signal("METRICS")).transpose()?,
    })
}

fn check_endpoint(var: &str, endpoint: &str) -> Result<(), String> {
    let ok = endpoint
        .parse::<http::Uri>()
        .ok()
        .filter(|uri| {
            matches!(uri.scheme_str(), Some(s) if s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"))
                && uri.authority().is_some()
        })
        .is_some();
    if ok {
        Ok(())
    } else {
        Err(format!(
            "OpenTelemetry disabled: {var}='{}' is not an http:// or https:// URI",
            redact_userinfo(endpoint)
        ))
    }
}

/// `scheme://user:pass@host/...` -> `scheme://<redacted>@host/...`, so an
/// endpoint carrying credentials is never logged verbatim. Anything without
/// a userinfo part is returned unchanged.
fn redact_userinfo(endpoint: &str) -> String {
    let (prefix, rest) = match endpoint.find("://") {
        Some(i) => endpoint.split_at(i + 3),
        None => ("", endpoint),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{prefix}<redacted>{}", &rest[at..]),
        None => endpoint.to_string(),
    }
}

/// Build the batch-exporting tracer provider for `traces`.
///
/// For an `https://` endpoint: with a configured CA
/// (`OTEL_EXPORTER_OTLP_CERTIFICATE`), that CA is the **only** trust anchor,
/// which is the OTel spec's meaning of "the trusted certificate" and works on
/// a host with no native root store. Without one, the TLS config asks for the
/// native roots (`with_enabled_roots`); otlp 0.32 would otherwise build a
/// bare `ClientTlsConfig::new()` that trusts nothing. Must run inside a tokio
/// runtime (the tonic channel is built with `connect_lazy`).
pub fn build_tracer_provider(
    traces: &SignalExport,
    resource: Resource,
) -> Result<SdkTracerProvider, String> {
    // The endpoint is resolved by `export_config` with the exporter's own
    // precedence, so passing it here changes nothing about which variable
    // wins; it only keeps an empty variable from reaching the exporter.
    let mut builder = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(traces.endpoint.clone());
    if traces.is_https() {
        // Never both: `with_enabled_roots` fails eagerly with
        // `NativeCertsNotFound` on a host without a native store
        // (tonic-0.14.6 `channel/service/tls.rs:86-94`), which would refuse
        // a private-CA collector for no reason.
        let tls = match &traces.ca_pem {
            Some(pem) => ClientTlsConfig::new().ca_certificate(Certificate::from_pem(pem)),
            None => ClientTlsConfig::new().with_enabled_roots(),
        };
        builder = builder.with_tls_config(tls);
    }
    let exporter = builder
        .build()
        .map_err(|e| format!("OpenTelemetry disabled: cannot build the OTLP span exporter: {e}"))?;
    Ok(SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

/// The `fmt` layer's filter while telemetry is active: the operator's
/// `EnvFilter`, minus the telemetry target.
///
/// The exclusion is a `filter_fn` rather than (only) a directive because an
/// operator `RUST_LOG=macp_runtime::telemetry=info` would tie with our
/// `=off` directive on specificity; the `filter_fn` excludes the target
/// whatever `RUST_LOG` says. Without it every log line inside a request would
/// gain a span-context prefix.
///
/// Deliberately no `macp_runtime::telemetry=off` directive on the
/// `EnvFilter`: directives match by target *prefix*, so it would also
/// silence the events this module logs (target
/// `macp_runtime::telemetry::init`), such as the startup notice and the
/// shutdown warnings.
pub fn fmt_filter<S>(env_filter: EnvFilter) -> impl Filter<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    env_filter.and(
        filter_fn(|meta| meta.target() != TARGET)
            // Keeps the combined hint equal to the EnvFilter's own.
            .with_max_level_hint(LevelFilter::TRACE),
    )
}

/// The OpenTelemetry layer: spans only, target [`TARGET`] only, at `INFO` or
/// above. `RUST_LOG` cannot silence it, it never exports a `tracing` event,
/// and it never sees the SDK's own internal logs (no feedback loop).
pub fn otel_layer<S>(tracer: SdkTracer) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_opentelemetry::layer()
        .with_tracer(tracer)
        .with_threads(false)
        .with_location(false)
        .with_tracked_inactivity(false)
        .with_target(false)
        .with_filter(
            filter_fn(|meta| {
                meta.is_span() && meta.target() == TARGET && *meta.level() <= tracing::Level::INFO
            })
            .with_max_level_hint(LevelFilter::INFO),
        )
}

/// Owns the providers of an active runtime.
#[derive(Debug)]
pub struct Guard {
    tracer_provider: Option<SdkTracerProvider>,
}

impl Guard {
    /// A guard over a caller-built tracer provider, for tests of the bounded
    /// shutdown. Not part of the supported API.
    #[doc(hidden)]
    pub fn from_tracer_provider(tracer_provider: SdkTracerProvider) -> Self {
        Self {
            tracer_provider: Some(tracer_provider),
        }
    }

    /// The switch to inject with `Runtime::with_telemetry`.
    pub fn telemetry(&self) -> Telemetry {
        Telemetry::enabled()
    }

    /// Flush and shut down every provider, bounded by [`SHUTDOWN_TIMEOUT`].
    ///
    /// The shutdown runs on a plain `std::thread`, not `spawn_blocking`:
    /// tokio's blocking pool is joined without limit when the runtime drops,
    /// so a stuck blocking task would hold process exit however this future
    /// timed out. A thread still running when the bound expires is abandoned,
    /// and process exit ends it.
    pub async fn shutdown(self) {
        let Some(provider) = self.tracer_provider else {
            return;
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("otel-shutdown".into())
            .spawn(move || {
                let _ = tx.send(provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT));
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "OpenTelemetry shutdown thread failed to start");
            return;
        }
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => {
                tracing::warn!(error = %e, "OpenTelemetry tracer provider shutdown failed")
            }
            Ok(Err(_)) => tracing::warn!("OpenTelemetry tracer provider shutdown panicked"),
            Err(_) => tracing::warn!(
                "OpenTelemetry tracer provider shutdown timed out; abandoning the flush"
            ),
        }
    }
}

/// An active [`Telemetry`] plus a thread-local-ready dispatcher routing
/// telemetry spans to `tracer_provider`, for tests that run active and
/// inactive runtimes side by side without the global [`init`]. Install the
/// dispatcher with `tracing::dispatcher::set_default` (or `with_default`) and
/// pass the `Telemetry` to `Runtime::with_telemetry`. The dispatcher carries
/// exactly the production [`otel_layer`] and no `fmt` layer. Not part of the
/// supported API; the meter provider joins the signature when the metric
/// instruments do.
#[doc(hidden)]
pub fn active_for_tests(tracer_provider: &SdkTracerProvider) -> (tracing::Dispatch, Telemetry) {
    let subscriber =
        tracing_subscriber::registry().with(otel_layer(tracer_provider.tracer(TRACER_NAME)));
    (tracing::Dispatch::new(subscriber), Telemetry::enabled())
}

/// Install the process-wide subscriber, and the telemetry pipeline when
/// active.
///
/// Inactive (`Ok(None)`): installs exactly the feature-off subscriber,
/// `tracing_subscriber::fmt().with_env_filter(env_filter).init()`. On a
/// misconfiguration it installs the same subscriber and returns `Err` with
/// one message naming the variable, for the caller to log at `error!` (the
/// subscriber is in place by then). Active: installs the `fmt` layer behind
/// [`fmt_filter`] plus [`otel_layer`], sets the W3C trace-context propagator,
/// and returns the [`Guard`].
///
/// Reads the process environment, and must run inside a tokio runtime.
pub fn init(env_filter: EnvFilter) -> Result<Option<Guard>, String> {
    let env = |k: &str| std::env::var(k).ok();
    let activation = activation(&env);
    if !activation.is_active() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
        return Ok(None);
    }
    let built = export_config(&env, activation).and_then(|config| {
        config
            .traces
            .as_ref()
            .map(|traces| build_tracer_provider(traces, resource(&env)))
            .transpose()
    });
    let tracer_provider = match built {
        Ok(provider) => provider,
        Err(e) => {
            tracing_subscriber::fmt().with_env_filter(env_filter).init();
            return Err(e);
        }
    };

    // The spec names `traceparent`/`tracestate` only: no baggage propagator.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let otel = tracer_provider
        .as_ref()
        .map(|provider| otel_layer(provider.tracer(TRACER_NAME)));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(fmt_filter(env_filter)))
        .with(otel)
        .init();
    tracing::info!(
        traces = activation.traces,
        metrics = activation.metrics,
        "OpenTelemetry enabled"
    );
    Ok(Some(Guard { tracer_provider }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    const BOTH: Activation = Activation {
        traces: true,
        metrics: true,
    };

    #[test]
    fn activation_no_vars_is_inactive() {
        let a = activation(&env_of(&[]));
        assert_eq!(a, Activation::default());
        assert!(!a.is_active());
    }

    #[test]
    fn activation_generic_endpoint_exports_both_signals() {
        let a = activation(&env_of(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://127.0.0.1:4317",
        )]));
        assert_eq!(a, BOTH);
    }

    #[test]
    fn activation_traces_endpoint_exports_traces_only() {
        let a = activation(&env_of(&[(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://127.0.0.1:4317",
        )]));
        assert_eq!(
            a,
            Activation {
                traces: true,
                metrics: false
            }
        );
    }

    #[test]
    fn activation_metrics_endpoint_exports_metrics_only() {
        let a = activation(&env_of(&[(
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            "http://127.0.0.1:4317",
        )]));
        assert_eq!(
            a,
            Activation {
                traces: false,
                metrics: true
            }
        );
    }

    #[test]
    fn activation_sdk_disabled_wins_case_insensitively() {
        for v in ["TRUE", "true", "True"] {
            let a = activation(&env_of(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
                ("OTEL_SDK_DISABLED", v),
            ]));
            assert!(!a.is_active(), "OTEL_SDK_DISABLED={v}");
        }
        let a = activation(&env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
            ("OTEL_SDK_DISABLED", "false"),
        ]));
        assert_eq!(a, BOTH);
    }

    #[test]
    fn activation_empty_endpoint_is_unset() {
        let a = activation(&env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", ""),
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", ""),
            ("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT", ""),
        ]));
        assert!(!a.is_active());
    }

    fn attr(r: &Resource, key: &'static str) -> Option<String> {
        r.get(&opentelemetry::Key::from_static_str(key))
            .map(|v| v.to_string())
    }

    #[test]
    fn resource_defaults() {
        let r = resource(&env_of(&[]));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("macp-runtime"));
        assert_eq!(
            attr(&r, "service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        // Started from an empty builder: no SDK telemetry.* attributes.
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn resource_service_name_variable() {
        let r = resource(&env_of(&[("OTEL_SERVICE_NAME", "x")]));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("x"));
    }

    #[test]
    fn resource_attributes_variable() {
        let r = resource(&env_of(&[(
            "OTEL_RESOURCE_ATTRIBUTES",
            "service.name=y, deployment.environment = prod",
        )]));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("y"));
        assert_eq!(attr(&r, "deployment.environment").as_deref(), Some("prod"));
        assert_eq!(
            attr(&r, "service.version").as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn resource_service_name_beats_resource_attributes() {
        let r = resource(&env_of(&[
            ("OTEL_SERVICE_NAME", "x"),
            ("OTEL_RESOURCE_ATTRIBUTES", "service.name=y"),
        ]));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("x"));
    }

    #[test]
    fn resource_empty_service_name_is_unset() {
        let r = resource(&env_of(&[("OTEL_SERVICE_NAME", "")]));
        assert_eq!(attr(&r, "service.name").as_deref(), Some("macp-runtime"));
    }

    #[test]
    fn export_config_resolves_signal_specific_over_generic() {
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://generic:4317"),
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "https://traces:4317"),
        ]);
        let c = export_config(&env, activation(&env)).unwrap();
        assert_eq!(c.traces.unwrap().endpoint, "https://traces:4317");
        assert_eq!(c.metrics.unwrap().endpoint, "http://generic:4317");
    }

    #[test]
    fn export_config_refuses_a_non_grpc_protocol_naming_the_variable() {
        for var in [
            "OTEL_EXPORTER_OTLP_PROTOCOL",
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
        ] {
            let env = env_of(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
                (var, "http/protobuf"),
            ]);
            let e = export_config(&env, activation(&env)).unwrap_err();
            assert!(e.contains(var), "{e}");
        }
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ]);
        assert!(export_config(&env, activation(&env)).is_ok());
    }

    #[test]
    fn export_config_refuses_a_non_http_endpoint_naming_the_variable() {
        for bad in [
            "127.0.0.1:4317",
            "grpc://collector:4317",
            "not a uri",
            "http://",
        ] {
            let env = env_of(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", bad)]);
            let e = export_config(&env, activation(&env)).unwrap_err();
            assert!(
                e.contains("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn endpoint_errors_redact_userinfo() {
        let env = env_of(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "ftp://alice:s3cret@collector:4317/x",
        )]);
        let e = export_config(&env, activation(&env)).unwrap_err();
        assert!(!e.contains("s3cret") && !e.contains("alice"), "{e}");
        assert!(e.contains("ftp://<redacted>@collector:4317/x"), "{e}");
        assert_eq!(
            redact_userinfo("http://collector:4317"),
            "http://collector:4317"
        );
        assert_eq!(redact_userinfo("u:p@host"), "<redacted>@host");
        assert_eq!(
            redact_userinfo("https://host:1/path@x"),
            "https://host:1/path@x"
        );
    }

    #[test]
    fn export_config_reads_the_certificate_and_refuses_an_unreadable_one() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("ca.pem");
        std::fs::write(&pem, b"PEM").unwrap();
        let pem = pem.to_str().unwrap().to_string();
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://collector:4317"),
            ("OTEL_EXPORTER_OTLP_CERTIFICATE", &pem),
        ]);
        let c = export_config(&env, activation(&env)).unwrap();
        assert_eq!(c.traces.unwrap().ca_pem.as_deref(), Some(&b"PEM"[..]));

        let missing = dir.path().join("missing.pem");
        let missing = missing.to_str().unwrap();
        let env = env_of(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://collector:4317"),
            ("OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE", missing),
        ]);
        let e = export_config(&env, activation(&env)).unwrap_err();
        assert!(e.contains("OTEL_EXPORTER_OTLP_TRACES_CERTIFICATE"), "{e}");
    }
}
