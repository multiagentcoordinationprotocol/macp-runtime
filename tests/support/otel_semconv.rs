//! Loader for the vendored MACP OpenTelemetry semantic conventions,
//! `tests/otel/macp-semconv.json` (`macp-semconv/0.1`, see
//! `tests/otel/SOURCE.md`).
//!
//! Shared by the integration tests that assert span names, attributes and
//! metric shapes against the canonical list. Include it with
//! `#[path = "support/otel_semconv.rs"] mod otel_semconv;`.
//!
//! The file is parsed through `serde_json::Value` rather than a derived
//! struct so a new optional field upstream does not break the loader; any
//! shape this loader depends on is checked with a panic naming the group.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

/// Path of the vendored semconv file.
pub fn semconv_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/otel/macp-semconv.json")
}

/// A requirement level: `required`, `recommended`, `opt_in`, or
/// `conditionally_required`, with the condition text when the file gives one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub level: String,
    pub condition: Option<String>,
}

impl Requirement {
    fn parse(v: &Value, ctx: &str) -> Self {
        match v {
            Value::String(s) => Requirement {
                level: s.clone(),
                condition: None,
            },
            Value::Object(m) if m.len() == 1 => {
                let (level, cond) = m.iter().next().unwrap();
                Requirement {
                    level: level.clone(),
                    condition: Some(
                        cond.as_str()
                            .unwrap_or_else(|| panic!("{ctx}: requirement condition not a string"))
                            .to_string(),
                    ),
                }
            }
            other => panic!("{ctx}: unexpected requirement_level {other}"),
        }
    }

    pub fn is_required(&self) -> bool {
        self.level == "required"
    }

    pub fn is_conditionally_required(&self) -> bool {
        self.level == "conditionally_required"
    }

    pub fn is_recommended(&self) -> bool {
        self.level == "recommended"
    }
}

/// An attribute defined in a `registry.*` attribute group.
#[derive(Debug, Clone)]
pub struct AttributeDef {
    pub id: String,
    /// `string`, `enum`, ...; `None` for a reused (`ref`) OTel attribute.
    pub ty: Option<String>,
    /// Enum member values (empty unless `ty == Some("enum")`).
    pub members: Vec<String>,
    pub requirement: Requirement,
}

/// A use of an attribute inside a span or metric group.
#[derive(Debug, Clone)]
pub struct AttributeRef {
    pub id: String,
    pub requirement: Requirement,
}

#[derive(Debug, Clone)]
pub struct SpanGroup {
    /// Group id, e.g. `span.macp.process`.
    pub id: String,
    /// `client`, `internal`, ...
    pub span_kind: String,
    /// Name template, e.g. `process {macp.mode.id}`.
    pub span_name: String,
    pub attributes: Vec<AttributeRef>,
}

impl SpanGroup {
    /// The operation name: the span name's leading word (`process`).
    pub fn operation(&self) -> &str {
        self.span_name.split(' ').next().unwrap_or_default()
    }

    pub fn attribute(&self, id: &str) -> Option<&AttributeRef> {
        self.attributes.iter().find(|a| a.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct MetricGroup {
    /// Group id, e.g. `metric.macp.envelope.processed`.
    pub id: String,
    /// Instrument name, e.g. `macp.envelope.processed`.
    pub metric_name: String,
    /// `counter`, `histogram`, ...
    pub instrument: String,
    pub unit: String,
    pub attributes: Vec<AttributeRef>,
}

impl MetricGroup {
    pub fn attribute(&self, id: &str) -> Option<&AttributeRef> {
        self.attributes.iter().find(|a| a.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct Semconv {
    pub schema: String,
    /// Every registry attribute, MACP-defined and reused, by id.
    pub attributes: BTreeMap<String, AttributeDef>,
    pub spans: Vec<SpanGroup>,
    pub metrics: Vec<MetricGroup>,
}

fn str_field(v: &Value, key: &str, ctx: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{ctx}: missing string field `{key}`"))
        .to_string()
}

fn attribute_refs(group: &Value, ctx: &str) -> Vec<AttributeRef> {
    group
        .get("attributes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{ctx}: missing `attributes` array"))
        .iter()
        .map(|a| {
            let id = str_field(a, "ref", ctx);
            let requirement = Requirement::parse(
                a.get("requirement_level")
                    .unwrap_or_else(|| panic!("{ctx}/{id}: missing requirement_level")),
                &format!("{ctx}/{id}"),
            );
            AttributeRef { id, requirement }
        })
        .collect()
}

impl Semconv {
    /// Load and parse the vendored file. Panics with context on any shape
    /// the loader does not understand.
    pub fn load() -> Self {
        let path = semconv_path();
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        Self::from_slice(&bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        let root: Value = serde_json::from_slice(bytes).expect("macp-semconv.json is valid JSON");
        let schema = str_field(&root, "schema", "root");
        let groups = root
            .get("groups")
            .and_then(Value::as_array)
            .expect("root: missing `groups` array");

        let mut attributes = BTreeMap::new();
        let mut spans = Vec::new();
        let mut metrics = Vec::new();

        for g in groups {
            let id = str_field(g, "id", "group");
            match str_field(g, "type", &id).as_str() {
                "attribute_group" => {
                    for a in g
                        .get("attributes")
                        .and_then(Value::as_array)
                        .unwrap_or_else(|| panic!("{id}: missing `attributes` array"))
                    {
                        // MACP-defined attributes carry `id`; reused OTel ones `ref`.
                        let attr_id = a
                            .get("id")
                            .or_else(|| a.get("ref"))
                            .and_then(Value::as_str)
                            .unwrap_or_else(|| panic!("{id}: attribute without id/ref"))
                            .to_string();
                        let ctx = format!("{id}/{attr_id}");
                        let ty = a.get("type").and_then(Value::as_str).map(str::to_string);
                        let members = a
                            .get("members")
                            .and_then(Value::as_array)
                            .map(|ms| ms.iter().map(|m| str_field(m, "value", &ctx)).collect())
                            .unwrap_or_default();
                        let requirement = Requirement::parse(
                            a.get("requirement_level")
                                .unwrap_or_else(|| panic!("{ctx}: missing requirement_level")),
                            &ctx,
                        );
                        let prev = attributes.insert(
                            attr_id.clone(),
                            AttributeDef {
                                id: attr_id,
                                ty,
                                members,
                                requirement,
                            },
                        );
                        assert!(prev.is_none(), "{ctx}: attribute defined twice");
                    }
                }
                "span" => spans.push(SpanGroup {
                    span_kind: str_field(g, "span_kind", &id),
                    span_name: str_field(g, "span_name", &id),
                    attributes: attribute_refs(g, &id),
                    id,
                }),
                "metric" => metrics.push(MetricGroup {
                    metric_name: str_field(g, "metric_name", &id),
                    instrument: str_field(g, "instrument", &id),
                    unit: str_field(g, "unit", &id),
                    attributes: attribute_refs(g, &id),
                    id,
                }),
                other => panic!("{id}: unknown group type `{other}`"),
            }
        }

        Semconv {
            schema,
            attributes,
            spans,
            metrics,
        }
    }

    pub fn span(&self, id: &str) -> &SpanGroup {
        self.spans
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("no span group `{id}`"))
    }

    pub fn metric(&self, id: &str) -> &MetricGroup {
        self.metrics
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("no metric group `{id}`"))
    }

    pub fn attribute(&self, id: &str) -> &AttributeDef {
        self.attributes
            .get(id)
            .unwrap_or_else(|| panic!("no registry attribute `{id}`"))
    }

    /// Enum member values of a registry attribute, e.g.
    /// `enum_members("macp.envelope.outcome")`.
    pub fn enum_members(&self, id: &str) -> &[String] {
        let a = self.attribute(id);
        assert_eq!(a.ty.as_deref(), Some("enum"), "`{id}` is not an enum");
        &a.members
    }
}
