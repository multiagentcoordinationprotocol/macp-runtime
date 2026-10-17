//! Structured `POLICY_DENIED` detail (RFC-MACP-0012 §6.2, proposal S13).
//!
//! A denial reason carries a stable machine identifier ([`DenyCode`]), a human
//! message, and the participant identifiers it concerns — *separately* from the
//! message. A runtime that must not disclose, say, who raised a blocking
//! objection to the submitter drops [`DenyReason::participants`] (or calls
//! [`DenyReason::redacted`]) instead of parsing prose. The message of every
//! reason this crate family produces is free of participant identifiers.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;

/// A stable, machine-readable denial-reason identifier: lowercase snake_case.
///
/// An open set by design. The spec gives `veto`, `quorum_not_met`,
/// `threshold_not_met`, `confidence_below_minimum` and `no_votes` as examples
/// rather than a closed registry, so a custom
/// [`crate::policy::PolicyEvaluator`] may mint its own with
/// [`DenyCode::new`]. The associated constants are the codes the reference
/// evaluator emits; clients should branch on them and treat any other code as
/// a generic denial.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DenyCode(Cow<'static, str>);

impl DenyCode {
    /// No stable identifier: a reason converted from bare text.
    pub const UNSPECIFIED: Self = Self::from_static("unspecified");
    /// A standing critical-objection veto blocks the commitment.
    pub const VETO: Self = Self::from_static("veto");
    /// A critical-objection veto holds the session for escalation
    /// (`critical_objection_action: "hold"`); the session stays open.
    pub const VETO_HOLD: Self = Self::from_static("veto_hold");
    /// The voting (or quorum-mode approval) participation floor is not met.
    pub const QUORUM_NOT_MET: Self = Self::from_static("quorum_not_met");
    /// The voting algorithm's approval threshold is not met.
    pub const THRESHOLD_NOT_MET: Self = Self::from_static("threshold_not_met");
    /// No qualifying evaluation reaches `evaluation.minimum_confidence`.
    pub const CONFIDENCE_BELOW_MINIMUM: Self = Self::from_static("confidence_below_minimum");
    /// Evaluations are required before voting and none qualifies.
    pub const EVALUATION_REQUIRED: Self = Self::from_static("evaluation_required");
    /// No decisive vote was cast.
    pub const NO_VOTES: Self = Self::from_static("no_votes");
    /// A decline was requested over a passing vote without
    /// `commitment.allow_decline_over_approval`.
    pub const DECLINE_OVER_APPROVAL: Self = Self::from_static("decline_over_approval");
    /// A decline has no decisive explicit reject backing it (RFC-MACP-0007
    /// §6.2's decline guard).
    pub const DECLINE_NOT_BACKED: Self = Self::from_static("decline_not_backed");
    /// The mode's own acceptance criterion is not met (proposal acceptance,
    /// quorum threshold, ...).
    pub const CRITERION_NOT_MET: Self = Self::from_static("criterion_not_met");
    /// The policy requires task output before commitment.
    pub const OUTPUT_REQUIRED: Self = Self::from_static("output_required");
    /// Task reassignment after a rejection is not permitted by
    /// `assignment.allow_reassignment_on_reject`.
    pub const REASSIGNMENT_NOT_ALLOWED: Self = Self::from_static("reassignment_not_allowed");
    /// The bound descriptor's `schema_version` is not supported.
    pub const UNSUPPORTED_SCHEMA_VERSION: Self = Self::from_static("unsupported_schema_version");
    /// The bound rules are malformed or out of domain at evaluation time.
    pub const INVALID_RULES: Self = Self::from_static("invalid_rules");

    /// A code from a static string.
    pub const fn from_static(code: &'static str) -> Self {
        Self(Cow::Borrowed(code))
    }

    /// A custom code, e.g. from a vendor evaluator.
    pub fn new(code: impl Into<Cow<'static, str>>) -> Self {
        Self(code.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DenyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One reason a policy denied a commitment.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyReason {
    /// The stable identifier clients branch on.
    pub code: DenyCode,
    /// Human-readable explanation. Contains no participant identifiers.
    pub message: String,
    /// The participants this reason concerns (e.g. the senders of the blocking
    /// objections behind a `veto`), sorted and deduplicated. Empty when the
    /// reason concerns no particular participant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
}

impl DenyReason {
    pub fn new(code: DenyCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            participants: Vec::new(),
        }
    }

    /// Attach the participants this reason concerns (sorted, deduplicated).
    pub fn with_participants<I, S>(mut self, participants: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids: Vec<String> = participants.into_iter().map(Into::into).collect();
        ids.sort();
        ids.dedup();
        self.participants = ids;
        self
    }

    /// The same reason without its participant identifiers, for a deployment
    /// that must not disclose them to the submitter.
    pub fn redacted(&self) -> Self {
        Self {
            participants: Vec::new(),
            ..self.clone()
        }
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The reasons' messages joined with `sep` — for log lines and status text.
pub fn join_messages(reasons: &[DenyReason], sep: &str) -> String {
    reasons
        .iter()
        .map(|r| r.message.as_str())
        .collect::<Vec<_>>()
        .join(sep)
}

/// Bare text becomes an [`DenyCode::UNSPECIFIED`] reason — the migration path
/// for an evaluator written against the pre-0.9 `Vec<String>` shape. Never
/// put participant identifiers in such text.
impl From<String> for DenyReason {
    fn from(message: String) -> Self {
        Self::new(DenyCode::UNSPECIFIED, message)
    }
}

impl From<&str> for DenyReason {
    fn from(message: &str) -> Self {
        Self::new(DenyCode::UNSPECIFIED, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_participants_beside_the_message() {
        let r = DenyReason::new(
            DenyCode::VETO,
            "2 blocking objection(s) (veto threshold: 1)",
        )
        .with_participants(["b", "a", "b"]);
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            serde_json::json!({
                "code": "veto",
                "message": "2 blocking objection(s) (veto threshold: 1)",
                "participants": ["a", "b"],
            })
        );
        let redacted = serde_json::to_value(r.redacted()).unwrap();
        assert_eq!(redacted.get("participants"), None);
        assert_eq!(redacted["code"], "veto");
    }

    #[test]
    fn custom_codes_round_trip() {
        let r: DenyReason =
            serde_json::from_value(serde_json::json!({"code": "acme_budget", "message": "m"}))
                .unwrap();
        assert_eq!(r.code, DenyCode::new("acme_budget"));
        assert!(r.participants.is_empty());
        assert_eq!(DenyReason::from("text").code, DenyCode::UNSPECIFIED);
    }
}
