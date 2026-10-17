//! The key namespaces of a standards-track `rules` object (RFC-MACP-0012 §4).
//!
//! Three kinds of key may appear in a standards-track rules object:
//!
//! 1. **Defined keys** — the properties the canonical rule schema
//!    (`schemas/json/policy/<mode>-rules.schema.json` in the spec repo) declares
//!    at that nesting level. Rule objects are *closed*: any other key, at any
//!    level, is refused at admission. [`closed_key_violation`] is the walk.
//! 2. **Annotation keys** (`^[_$]`, [`is_annotation_key`]) — legal at every
//!    closed level, carry no governance semantics, and are excluded from §8
//!    equality.
//! 3. **Vendor-extension keys** (`^x-[a-z0-9]+(\.[a-z0-9-]+)+$`,
//!    [`is_vendor_extension_key`]) — legal at the **top level only**, value
//!    must be a JSON object, may only *restrict*, and are **part of** §8
//!    equality. This namespace is proposal S1
//!    (multiagentcoordinationprotocol#186), not yet RFC text, so a runtime
//!    accepts one only when an implementation of it is registered — see
//!    [`RuleExtension`]. With none registered, every `x-` key is refused,
//!    which is exactly today's closed-object behaviour.
//!
//! This module carries no JSON-Schema evaluator. The key trees below are
//! hand-written mirrors of the canonical schemas' `properties`; the
//! `conformance-oracle` CI job pins them to the canonical copies
//! (`closed_key_sets_match_the_canonical_schemas` in `macp-policy`).
//!
//! Extension modes are out of scope: their rules are opaque JSON and none of
//! this applies to them ([`rule_shape_for_mode`] returns `None`).

use serde_json::Value;

/// A vendor-extension implementation (proposal S1).
///
/// The registry calls [`validate`](Self::validate) at admission with the
/// extension key's value (always a JSON object). Enforcement of the
/// extension's *meaning* is the embedder's job — typically a
/// [`PolicyEvaluator`](super::PolicyEvaluator) wrapper or a mode-level check —
/// and is **restrict-only** by contract: an extension may cause a rejection or
/// a denial standard rules would not, never an acceptance they would refuse.
///
/// MACP-standard evaluation ignores vendor-extension keys entirely (the rule
/// structs in [`super::rules`] do not read them).
pub trait RuleExtension: Send + Sync {
    /// Validate the extension's value. `Err` refuses the whole descriptor with
    /// `INVALID_POLICY_DEFINITION`, the message carried through.
    fn validate(&self, value: &Value) -> Result<(), String>;
}

/// The regular expression [`is_vendor_extension_key`] implements, verbatim
/// from proposal S1, for error messages and documentation.
pub const VENDOR_EXTENSION_KEY_PATTERN: &str = r"^x-[a-z0-9]+(\.[a-z0-9-]+)+$";

/// `^[_$]`: the annotation namespace of RFC-MACP-0012 §4.
pub fn is_annotation_key(key: &str) -> bool {
    key.starts_with('_') || key.starts_with('$')
}

/// `^x-[a-z0-9]+(\.[a-z0-9-]+)+$`: a reverse-domain vendor-extension key
/// (proposal S1). Hand-matched (no regex dependency): `x-`, then one segment
/// of `[a-z0-9]`, then one or more `.`-prefixed segments of `[a-z0-9-]`.
pub fn is_vendor_extension_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("x-") else {
        return false;
    };
    let mut segments = rest.split('.');
    let first_ok = segments.next().is_some_and(|s| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    });
    let mut tail = 0usize;
    for s in segments {
        if s.is_empty()
            || !s
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return false;
        }
        tail += 1;
    }
    first_ok && tail >= 1
}

/// The shape of one position in a rule schema.
#[derive(Debug, Clone, Copy)]
pub enum RuleShape {
    /// A closed object: exactly these keys (plus annotations) are defined.
    Closed(&'static [(&'static str, RuleShape)]),
    /// An open map whose keys are data, not schema (e.g. `voting.weights`,
    /// keyed by participant id). Neither closed nor annotation-stripped.
    Map,
    /// A scalar or array; nothing to walk.
    Leaf,
}

use RuleShape::{Closed, Leaf, Map};

const COMMITMENT_COMMON: RuleShape = Closed(&[("authority", Leaf), ("designated_roles", Leaf)]);

/// `decision-rules.schema.json`.
pub const DECISION_RULES: RuleShape = Closed(&[
    (
        "voting",
        Closed(&[
            ("algorithm", Leaf),
            ("threshold", Leaf),
            ("quorum", Closed(&[("type", Leaf), ("value", Leaf)])),
            ("weights", Map),
        ]),
    ),
    (
        "objection_handling",
        Closed(&[
            ("critical_objection_action", Leaf),
            ("critical_severity_vetoes", Leaf),
            ("veto_threshold", Leaf),
        ]),
    ),
    (
        "evaluation",
        Closed(&[
            ("minimum_confidence", Leaf),
            ("required_before_voting", Leaf),
        ]),
    ),
    (
        "commitment",
        Closed(&[
            ("allow_decline_over_approval", Leaf),
            ("authority", Leaf),
            ("designated_roles", Leaf),
            ("require_vote_quorum", Leaf),
        ]),
    ),
]);

/// `proposal-rules.schema.json`.
pub const PROPOSAL_RULES: RuleShape = Closed(&[
    ("acceptance", Closed(&[("criterion", Leaf)])),
    ("counter_proposal", Closed(&[("max_rounds", Leaf)])),
    ("rejection", Closed(&[("terminal_on_any_reject", Leaf)])),
    ("commitment", COMMITMENT_COMMON),
]);

/// `task-rules.schema.json`.
pub const TASK_RULES: RuleShape = Closed(&[
    (
        "assignment",
        Closed(&[("allow_reassignment_on_reject", Leaf)]),
    ),
    ("completion", Closed(&[("require_output", Leaf)])),
    ("commitment", COMMITMENT_COMMON),
]);

/// `handoff-rules.schema.json`.
pub const HANDOFF_RULES: RuleShape = Closed(&[
    (
        "acceptance",
        Closed(&[("implicit_accept_timeout_ms", Leaf)]),
    ),
    ("commitment", COMMITMENT_COMMON),
]);

/// `quorum-rules.schema.json`.
pub const QUORUM_RULES: RuleShape = Closed(&[
    ("threshold", Closed(&[("type", Leaf), ("value", Leaf)])),
    (
        "abstention",
        Closed(&[("counts_toward_quorum", Leaf), ("interpretation", Leaf)]),
    ),
    ("commitment", COMMITMENT_COMMON),
]);

/// Every standards-track mode with its canonical rule shape.
pub const STANDARDS_TRACK_RULE_SHAPES: [(&str, RuleShape); 5] = [
    ("macp.mode.decision.v1", DECISION_RULES),
    ("macp.mode.proposal.v1", PROPOSAL_RULES),
    ("macp.mode.task.v1", TASK_RULES),
    ("macp.mode.handoff.v1", HANDOFF_RULES),
    ("macp.mode.quorum.v1", QUORUM_RULES),
];

/// The shapes a policy targeting `mode` is validated against: one for a
/// standards-track mode, all five for `"*"` (a wildcard binds every mode, so
/// its rules object is closed over the **union** of their key sets), and
/// `None` for an extension mode, whose rules are opaque.
pub fn rule_shape_for_mode(mode: &str) -> Option<Vec<RuleShape>> {
    if mode == "*" {
        return Some(
            STANDARDS_TRACK_RULE_SHAPES
                .iter()
                .map(|(_, s)| *s)
                .collect(),
        );
    }
    STANDARDS_TRACK_RULE_SHAPES
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, s)| vec![*s])
}

/// A closed-object breach found by [`closed_key_violation`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyViolation {
    /// A key no schema defines at this level, and not an annotation (nor, at
    /// the top level, a vendor-extension key).
    Undefined { path: String },
    /// A well-formed vendor-extension key below the top level.
    NestedVendorExtension { path: String },
    /// A top-level vendor-extension key whose value is not a JSON object.
    VendorExtensionNotObject { key: String },
}

impl std::fmt::Display for KeyViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Undefined { path } => write!(
                f,
                "rules.{path} is not a defined key (RFC-MACP-0012 §4: rule objects are closed; \
                 annotation keys must match ^[_$])"
            ),
            Self::NestedVendorExtension { path } => write!(
                f,
                "rules.{path}: vendor-extension keys are legal only at the top level of rules"
            ),
            Self::VendorExtensionNotObject { key } => {
                write!(
                    f,
                    "rules.{key}: a vendor-extension value must be a JSON object"
                )
            }
        }
    }
}

/// Walk `rules` against `shapes` (see [`rule_shape_for_mode`]) and report the
/// first closed-object breach, with its dotted path. Top-level well-formed
/// vendor-extension keys are **not** reported here — whether one is admitted
/// depends on what the caller implements, so the caller checks
/// [`vendor_extension_keys`] itself.
///
/// Positions whose value is not an object are skipped: the type error belongs
/// to the typed parse that follows, which reports it better.
pub fn closed_key_violation(rules: &Value, shapes: &[RuleShape]) -> Option<KeyViolation> {
    walk(rules, shapes, "", true)
}

fn walk(value: &Value, shapes: &[RuleShape], path: &str, top: bool) -> Option<KeyViolation> {
    let Value::Object(map) = value else {
        return None;
    };
    let closed: Vec<&[(&str, RuleShape)]> = shapes
        .iter()
        .filter_map(|s| match s {
            Closed(keys) => Some(*keys),
            _ => None,
        })
        .collect();
    if closed.is_empty() {
        return None;
    }
    // Sorted for a deterministic first violation regardless of map order.
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    for key in keys {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        if is_annotation_key(key) {
            continue;
        }
        if is_vendor_extension_key(key) {
            if !top {
                return Some(KeyViolation::NestedVendorExtension { path: child_path });
            }
            if !map[key].is_object() {
                return Some(KeyViolation::VendorExtensionNotObject { key: key.clone() });
            }
            continue;
        }
        let children: Vec<RuleShape> = closed
            .iter()
            .flat_map(|keys| keys.iter())
            .filter(|(k, _)| k == key)
            .map(|(_, s)| *s)
            .collect();
        if children.is_empty() {
            return Some(KeyViolation::Undefined { path: child_path });
        }
        if let Some(v) = walk(&map[key], &children, &child_path, false) {
            return Some(v);
        }
    }
    None
}

/// The top-level vendor-extension keys of `rules`, sorted.
pub fn vendor_extension_keys(rules: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = rules
        .as_object()
        .map(|m| {
            m.keys()
                .map(String::as_str)
                .filter(|k| is_vendor_extension_key(k))
                .collect()
        })
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}

/// `rules` with every annotation key removed from every **closed** position
/// of `shapes` — the normal form §8 equality compares. Map positions
/// (`voting.weights`) and vendor-extension values are kept verbatim: their
/// keys are data, not schema, so a `_`-prefixed participant id or a vendor's
/// own `_note` is not an annotation. Leaves `rules` untouched for an empty
/// `shapes` (extension modes).
pub fn strip_annotations(rules: &Value, shapes: &[RuleShape]) -> Value {
    strip(rules, shapes)
}

fn strip(value: &Value, shapes: &[RuleShape]) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };
    let closed: Vec<&[(&str, RuleShape)]> = shapes
        .iter()
        .filter_map(|s| match s {
            Closed(keys) => Some(*keys),
            _ => None,
        })
        .collect();
    if closed.is_empty() {
        return value.clone();
    }
    let mut out = serde_json::Map::new();
    for (key, v) in map {
        if is_annotation_key(key) {
            continue;
        }
        let children: Vec<RuleShape> = closed
            .iter()
            .flat_map(|keys| keys.iter())
            .filter(|(k, _)| k == key)
            .map(|(_, s)| *s)
            .collect();
        out.insert(key.clone(), strip(v, &children));
    }
    Value::Object(out)
}

/// RFC-MACP-0012 §8 `rules` equality for a policy targeting `mode`: equal
/// modulo the `^[_$]` annotation namespace, with vendor-extension keys **not**
/// excluded (proposal S1). For an extension mode this is plain JSON equality.
///
/// This is *literal* equality of the normal form. It does not resolve schema
/// defaults — `{}` and `{"voting": {"algorithm": "none"}}` differ here even
/// though they parse identically; §2.2's "spelled out or defaulted" identity
/// for the reserved `policy.std.` namespace is a separate, typed comparison
/// in the registry.
pub fn rules_equal(mode: &str, a: &Value, b: &Value) -> bool {
    let shapes = rule_shape_for_mode(mode).unwrap_or_default();
    strip_annotations(a, &shapes) == strip_annotations(b, &shapes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decision() -> Vec<RuleShape> {
        rule_shape_for_mode("macp.mode.decision.v1").unwrap()
    }

    #[test]
    fn vendor_extension_key_grammar() {
        for ok in [
            "x-com.example",
            "x-com.example.gov",
            "x-io.seam.governance",
            "x-a1.b-2",
            "x-a.-",
        ] {
            assert!(is_vendor_extension_key(ok), "{ok}");
        }
        for bad in [
            "x-seam",
            "x-",
            "x-.com",
            "x-com.",
            "x-com..example",
            "x-Com.example",
            "x-com.Example",
            "x-co-m.example",
            "x_com.example",
            "X-com.example",
            "xcom.example",
            "x-com.exa_mple",
        ] {
            assert!(!is_vendor_extension_key(bad), "{bad}");
        }
    }

    #[test]
    fn annotation_keys() {
        assert!(is_annotation_key("_note"));
        assert!(is_annotation_key("$comment"));
        assert!(!is_annotation_key("note_"));
        assert!(!is_annotation_key("x-com.example"));
    }

    #[test]
    fn undefined_keys_are_found_at_every_level_with_their_path() {
        assert_eq!(
            closed_key_violation(&json!({"votng": {}}), &decision()),
            Some(KeyViolation::Undefined {
                path: "votng".into()
            })
        );
        assert_eq!(
            closed_key_violation(
                &json!({"objection_handling": {"veto_threshhold": 1}}),
                &decision()
            ),
            Some(KeyViolation::Undefined {
                path: "objection_handling.veto_threshhold".into()
            })
        );
        assert_eq!(
            closed_key_violation(
                &json!({"voting": {"quorum": {"kind": "count"}}}),
                &decision()
            ),
            Some(KeyViolation::Undefined {
                path: "voting.quorum.kind".into()
            })
        );
    }

    #[test]
    fn annotations_are_legal_at_every_closed_level() {
        let rules = json!({
            "_why": "x",
            "$comment": "y",
            "voting": {"_n": 1, "quorum": {"$c": true, "type": "count", "value": 1}},
            "commitment": {"_who": "ops"}
        });
        assert_eq!(closed_key_violation(&rules, &decision()), None);
    }

    #[test]
    fn weights_keys_are_participant_ids_not_schema() {
        let rules = json!({"voting": {"algorithm": "weighted", "weights": {"agent://a": 1, "_b": 2, "x-com.ex": 3}}});
        assert_eq!(closed_key_violation(&rules, &decision()), None);
    }

    #[test]
    fn vendor_extensions_top_level_object_only() {
        assert_eq!(
            closed_key_violation(
                &json!({"x-com.example.gov": {"require_reason": true}}),
                &decision()
            ),
            None
        );
        assert_eq!(
            closed_key_violation(&json!({"voting": {"x-com.example.gov": {}}}), &decision()),
            Some(KeyViolation::NestedVendorExtension {
                path: "voting.x-com.example.gov".into()
            })
        );
        assert_eq!(
            closed_key_violation(&json!({"x-com.example.gov": true}), &decision()),
            Some(KeyViolation::VendorExtensionNotObject {
                key: "x-com.example.gov".into()
            })
        );
        // Not reverse-domain: an ordinary undefined key.
        assert_eq!(
            closed_key_violation(&json!({"x-seam": {}}), &decision()),
            Some(KeyViolation::Undefined {
                path: "x-seam".into()
            })
        );
    }

    #[test]
    fn wildcard_is_closed_over_the_union() {
        let wildcard = rule_shape_for_mode("*").unwrap();
        // Keys from three different modes, and a nested union (`acceptance`
        // is proposal's `criterion` plus handoff's `implicit_accept_timeout_ms`).
        let rules = json!({
            "voting": {"algorithm": "majority", "threshold": 0.5},
            "threshold": {"type": "n_of_m", "value": 2},
            "acceptance": {"criterion": "all_parties", "implicit_accept_timeout_ms": 1000},
            "commitment": {"authority": "initiator_only", "require_vote_quorum": true}
        });
        assert_eq!(closed_key_violation(&rules, &wildcard), None);
        assert_eq!(
            closed_key_violation(&json!({"acceptance": {"quorum": 1}}), &wildcard),
            Some(KeyViolation::Undefined {
                path: "acceptance.quorum".into()
            })
        );
    }

    #[test]
    fn extension_modes_are_opaque() {
        assert!(rule_shape_for_mode("ext.multi_round.v1").is_none());
        assert!(rules_equal(
            "ext.multi_round.v1",
            &json!({"_a": 1}),
            &json!({"_a": 1})
        ));
        assert!(!rules_equal(
            "ext.multi_round.v1",
            &json!({"_a": 1}),
            &json!({})
        ));
    }

    #[test]
    fn section_8_equality_excludes_annotations_but_not_vendor_extensions() {
        let m = "macp.mode.decision.v1";
        let base = json!({"voting": {"algorithm": "majority", "threshold": 0.5}});
        let annotated =
            json!({"_note": "a", "voting": {"algorithm": "majority", "threshold": 0.5, "$c": 1}});
        assert!(rules_equal(m, &base, &annotated));
        let extended = json!({"voting": {"algorithm": "majority", "threshold": 0.5}, "x-com.example.gov": {"require_reason": true}});
        assert!(!rules_equal(m, &base, &extended));
        // Annotation-like keys inside a vendor value or the weights map are data.
        assert!(!rules_equal(
            m,
            &json!({"x-com.example.gov": {"_k": 1}}),
            &json!({"x-com.example.gov": {}})
        ));
        assert!(!rules_equal(
            m,
            &json!({"voting": {"weights": {"_p": 1}}}),
            &json!({"voting": {"weights": {}}})
        ));
    }

    #[test]
    fn vendor_extension_keys_are_listed_sorted() {
        let rules = json!({"x-org.b": {}, "voting": {}, "x-com.a": {}, "x-bad": {}});
        assert_eq!(vendor_extension_keys(&rules), vec!["x-com.a", "x-org.b"]);
    }
}
