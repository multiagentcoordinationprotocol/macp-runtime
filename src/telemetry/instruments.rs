//! The OpenTelemetry metric instruments (`otel` feature only).
//!
//! Names, instruments, units and descriptions are those of
//! `macp-semconv/0.1` (`tests/otel/macp-semconv.json`). They are fed by
//! [`crate::metrics::RuntimeMetrics::record_envelope`], the same recorder as
//! the Prometheus rendering, with the same normalized labels.

use std::fmt;

use opentelemetry::metrics::{Counter, Histogram, MeterProvider};
use opentelemetry::KeyValue;

use crate::metrics::ENVELOPE_DURATION_BOUNDARIES;

/// The instrumentation scope of every runtime instrument.
pub const METER_NAME: &str = "macp-runtime";

/// The runtime's OpenTelemetry instruments.
pub struct Instruments {
    envelope_processed: Counter<u64>,
    envelope_duration: Histogram<f64>,
}

impl fmt::Debug for Instruments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Instruments").finish_non_exhaustive()
    }
}

impl Instruments {
    /// Create the instruments on `provider`'s [`METER_NAME`] meter.
    pub fn new(provider: &impl MeterProvider) -> Self {
        let meter = provider.meter(METER_NAME);
        Self {
            envelope_processed: meter
                .u64_counter("macp.envelope.processed")
                .with_unit("{envelope}")
                .with_description("Envelopes the runtime processed, by outcome.")
                .build(),
            envelope_duration: meter
                .f64_histogram("macp.envelope.process.duration")
                .with_unit("s")
                .with_description("Time the runtime took to process one Envelope.")
                .with_boundaries(ENVELOPE_DURATION_BOUNDARIES.to_vec())
                .build(),
        }
    }

    /// Record one processed Envelope. Labels are already normalized.
    pub(crate) fn record_envelope(
        &self,
        mode: Option<&str>,
        message_type: &str,
        outcome: &'static str,
        seconds: f64,
    ) {
        let mut attrs = Vec::with_capacity(3);
        if let Some(mode) = mode {
            attrs.push(KeyValue::new("macp.mode.id", mode.to_owned()));
        }
        attrs.push(KeyValue::new("macp.message.type", message_type.to_owned()));
        attrs.push(KeyValue::new("macp.envelope.outcome", outcome));
        self.envelope_processed.add(1, &attrs);
        self.envelope_duration.record(seconds, &attrs);
    }
}
