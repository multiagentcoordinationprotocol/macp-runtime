//! Pins the vendored `tests/otel/macp-semconv.json` (`macp-semconv/0.1`) to
//! the set of spans and metrics this runtime implements.
//!
//! Always compiled (no `otel` feature needed): it reads only the vendored
//! file. Byte-identity with the spec repo at `SPEC_REV` is gated separately by
//! `check_dir "tests/otel"` in `ci.yml`'s `conformance-oracle` job.

#[path = "support/otel_semconv.rs"]
mod otel_semconv;

use std::collections::BTreeSet;

use otel_semconv::Semconv;

/// Span groups the runtime emits: `process` and the five session transitions.
const RUNTIME_SPANS: &[&str] = &[
    "span.macp.process",
    "span.macp.resolve",
    "span.macp.expire",
    "span.macp.cancel",
    "span.macp.suspend",
    "span.macp.resume",
];

/// Span groups the file defines that only SDKs emit. The runtime never
/// creates a `send` span.
const SDK_ONLY_SPANS: &[&str] = &["span.macp.send"];

/// Metric groups the runtime records.
const RUNTIME_METRICS: &[&str] = &[
    "metric.macp.envelope.processed",
    "metric.macp.envelope.process.duration",
    "metric.macp.session.transitions",
    "metric.macp.session.duration",
];

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn schema_is_macp_semconv_0_1() {
    assert_eq!(Semconv::load().schema, "macp-semconv/0.1");
}

#[test]
fn span_groups_are_runtime_spans_plus_sdk_only_send() {
    let sc = Semconv::load();
    let actual: BTreeSet<String> = sc.spans.iter().map(|s| s.id.clone()).collect();
    let mut expected = set(RUNTIME_SPANS);
    expected.extend(set(SDK_ONLY_SPANS));
    assert_eq!(
        actual, expected,
        "semconv span groups changed; update RUNTIME_SPANS / SDK_ONLY_SPANS and the runtime"
    );

    let send = sc.span("span.macp.send");
    assert_eq!(send.span_kind, "client", "send is the SDK's CLIENT span");
    for id in RUNTIME_SPANS {
        assert_eq!(
            sc.span(id).span_kind,
            "internal",
            "{id}: runtime spans are INTERNAL"
        );
    }
}

#[test]
fn metric_groups_equal_runtime_metrics() {
    let sc = Semconv::load();
    let actual: BTreeSet<String> = sc.metrics.iter().map(|m| m.id.clone()).collect();
    assert_eq!(
        actual,
        set(RUNTIME_METRICS),
        "semconv metric groups changed; update RUNTIME_METRICS and the runtime"
    );
    for m in &sc.metrics {
        assert_eq!(
            format!("metric.{}", m.metric_name),
            m.id,
            "{}: metric_name",
            m.id
        );
    }
}

#[test]
fn span_names_and_operation_enum_agree() {
    let sc = Semconv::load();
    let ops: BTreeSet<String> = sc
        .enum_members("macp.operation.name")
        .iter()
        .cloned()
        .collect();
    let from_spans: BTreeSet<String> = sc.spans.iter().map(|s| s.operation().to_string()).collect();
    assert_eq!(ops, from_spans);
    for s in &sc.spans {
        assert_eq!(
            s.span_name,
            format!("{} {{macp.mode.id}}", s.operation()),
            "{}: span name template",
            s.id
        );
        assert_eq!(s.id, format!("span.macp.{}", s.operation()));
    }
}

#[test]
fn every_attribute_ref_resolves_to_a_registry_attribute() {
    let sc = Semconv::load();
    let refs = sc
        .spans
        .iter()
        .flat_map(|s| s.attributes.iter().map(move |a| (&s.id, &a.id)))
        .chain(
            sc.metrics
                .iter()
                .flat_map(|m| m.attributes.iter().map(move |a| (&m.id, &a.id))),
        );
    for (group, attr) in refs {
        assert!(
            sc.attributes.contains_key(attr),
            "{group}: attribute `{attr}` is not in any registry group"
        );
    }
}

#[test]
fn loader_exposes_requirement_levels_and_enums() {
    let sc = Semconv::load();
    let process = sc.span("span.macp.process");
    assert!(process
        .attribute("macp.envelope.outcome")
        .unwrap()
        .requirement
        .is_required());
    let err = process.attribute("error.type").unwrap();
    assert!(err.requirement.is_conditionally_required());
    assert!(err.requirement.condition.is_some());

    assert_eq!(
        sc.enum_members("macp.envelope.outcome"),
        ["accepted", "rejected", "duplicate"]
    );
    assert!(sc
        .enum_members("macp.session.state")
        .iter()
        .all(|s| s.starts_with("SESSION_STATE_")));

    let processed = sc.metric("metric.macp.envelope.processed");
    assert_eq!(processed.instrument, "counter");
    assert_eq!(processed.unit, "{envelope}");
    assert_eq!(
        sc.metric("metric.macp.session.duration").instrument,
        "histogram"
    );
}
