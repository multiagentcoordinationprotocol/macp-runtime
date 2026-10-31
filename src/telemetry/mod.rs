//! OpenTelemetry for the runtime (`macp-semconv/0.1`).
//!
//! Telemetry is **active** only when all three hold:
//!
//! 1. the binary was built with the `otel` Cargo feature;
//! 2. one of `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
//!    or `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` is set and non-empty;
//! 3. `OTEL_SDK_DISABLED` is not `true` (case-insensitive).
//!
//! Otherwise every telemetry entry point is inert: no subscriber layer, no
//! propagator, no exporter and no new log lines. The switch is the
//! [`Telemetry`] value injected into [`crate::runtime::Runtime`] (and read from
//! there by [`crate::server::MacpServer`]), never a global, so an active and an
//! inactive runtime can live side by side in one test process.
//!
//! Every span this module's callers create uses the fixed target [`TARGET`].
//! With telemetry active, the `fmt` log layer refuses that target whatever
//! `RUST_LOG` says, and the OpenTelemetry layer admits nothing else and no
//! events at all. Log output is therefore unchanged by turning telemetry on,
//! and no `tracing` event (with its `reason` or error text) is ever exported.
//! See `docs/deployment.md`, "OpenTelemetry".

#[cfg(feature = "otel")]
pub mod init;

/// The `tracing` target of every telemetry span.
pub const TARGET: &str = "macp_runtime::telemetry";

/// The endpoint variables whose presence turns telemetry on.
pub const ENDPOINT_VARS: [&str; 3] = [
    "OTEL_EXPORTER_OTLP_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
];

/// The variable `env` holds for `key`, with an empty value treated as unset.
pub(crate) fn non_empty(env: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    env(key).filter(|v| !v.is_empty())
}

/// Whether any OTLP endpoint variable is set (non-empty) in `env`.
pub fn endpoint_configured(env: &impl Fn(&str) -> Option<String>) -> bool {
    ENDPOINT_VARS.iter().any(|k| non_empty(env, k).is_some())
}

/// The runtime's telemetry switch.
///
/// With the `otel` feature it records whether telemetry is active; without it
/// it is a zero-sized type and every query answers "inactive". Cloning is
/// cheap. The default is [`Telemetry::disabled`].
#[derive(Clone, Debug, Default)]
pub struct Telemetry {
    #[cfg(feature = "otel")]
    enabled: bool,
}

impl Telemetry {
    /// Inert telemetry: what every `Runtime` constructor installs.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Active telemetry. Only [`init::Guard::telemetry`] (and tests) build
    /// one, after the pipeline is installed.
    #[cfg(feature = "otel")]
    pub(crate) fn enabled() -> Self {
        Self { enabled: true }
    }

    /// Whether spans and instruments are recorded.
    pub fn is_enabled(&self) -> bool {
        #[cfg(feature = "otel")]
        {
            self.enabled
        }
        #[cfg(not(feature = "otel"))]
        {
            false
        }
    }
}

/// Feature-off startup notice: one `warn!` when an OTLP endpoint variable is
/// set on a binary built without `otel`, so an operator who configured an
/// exporter is not left with silent nothing. Emits nothing when no such
/// variable is set, so the default output is unchanged.
#[cfg(not(feature = "otel"))]
pub fn warn_if_configured_without_feature(env: &impl Fn(&str) -> Option<String>) {
    if endpoint_configured(env) {
        tracing::warn!(
            "this binary was built without the `otel` feature; OTEL_* ignored \
             (rebuild with `--features otel` to export OpenTelemetry)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            vars.iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn disabled_is_inactive() {
        assert!(!Telemetry::disabled().is_enabled());
        assert!(!Telemetry::default().is_enabled());
    }

    #[test]
    fn endpoint_configured_ignores_empty_values() {
        assert!(!endpoint_configured(&env_of(&[])));
        assert!(!endpoint_configured(&env_of(&[(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            ""
        )])));
        for var in ENDPOINT_VARS {
            let vars: &'static [(&'static str, &'static str)] =
                Box::leak(vec![(var, "http://127.0.0.1:4317")].into_boxed_slice());
            assert!(endpoint_configured(&env_of(vars)), "{var}");
        }
    }

    /// Feature off, the switch costs nothing: `Runtime` carries a ZST.
    #[cfg(not(feature = "otel"))]
    #[test]
    fn telemetry_is_zero_sized_without_the_feature() {
        assert_eq!(std::mem::size_of::<Telemetry>(), 0);
    }
}
