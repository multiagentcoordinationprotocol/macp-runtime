//! RFC-MACP-0013 canonical commitment hash.
//!
//! `commitment_hash` maps a `CommitmentPayload` to a stable
//! `sha256:<64 lowercase hex>` string by:
//!
//! 1. **Projecting** the payload to a frozen JSON object (RFC-MACP-0013 §3).
//! 2. **Canonicalizing** that object with a hand-written RFC 8785 (JCS)
//!    serializer (§4) -- deliberately not `serde_json`, whose object
//!    serialization does not implement JCS's key ordering or escaping rules.
//! 3. Hashing the domain-separated preimage with SHA-256 and hex-encoding
//!    the digest.
//!
//! This module has exactly one job and does it unconditionally: it never
//! validates the payload and never fails (see [`commitment_hash`]'s doc for
//! the D3 guarantee). Semantic validation of `CommitmentPayload` (matching
//! session-bound versions, well-formed `supersedes`, etc.) is a separate
//! concern handled elsewhere (see `macp-modes::mode::util`).

use macp_pb::pb::CommitmentPayload;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Domain-separation label for this hashing algorithm (RFC-MACP-0013 §4).
///
/// The preimage is `LABEL + ":" + canonical_json_bytes`, not `LABEL` alone --
/// see [`commitment_hash`] for the literal concatenation.
///
/// This label, the fixed nine-field projection, and the fixed top-level key
/// order below are all part of what this label identifies. If
/// `CommitmentPayload` ever grows a tenth field, RFC-MACP-0013 §5/§7 requires
/// minting a *new* label (`macp-commitment-hash/2`) for the new projection --
/// never silently extending the projection under this label.
const LABEL: &str = "macp-commitment-hash/1";

/// Compute the RFC-MACP-0013 commitment hash for `payload`.
///
/// Returns `"sha256:"` followed by 64 lowercase hex characters.
///
/// D3 -- this function never fails. It accepts any `CommitmentPayload`,
/// well-formed or not (including one with every string field empty, or a
/// `supersedes` present with empty sub-fields), and always returns a
/// `String`. It performs no validation; callers that need to reject
/// malformed payloads before hashing them must do so separately.
pub fn commitment_hash(payload: &CommitmentPayload) -> String {
    let canonical = canonicalize(payload);

    let mut preimage = Vec::with_capacity(LABEL.len() + 1 + canonical.len());
    preimage.extend_from_slice(LABEL.as_bytes());
    preimage.push(b':');
    preimage.extend_from_slice(&canonical);

    let digest = Sha256::digest(&preimage);

    let mut out = String::with_capacity("sha256:".len() + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        write!(out, "{byte:02x}").expect("write! to String never fails");
    }
    out
}

/// Project `payload` to the RFC-MACP-0013 §3 JSON object and serialize it
/// with RFC 8785 (JCS) rules, returning canonical UTF-8 bytes.
///
/// The projection's nine top-level member names (`action`,
/// `authority_scope`, `commitment_id`, `configuration_version`,
/// `mode_version`, `outcome_positive`, `policy_version`, `reason`,
/// `supersedes`) are a frozen, all-ASCII set, so JCS's "sort members by
/// UTF-16 code unit" rule (RFC 8785 §3.2.3) collapses to one static byte
/// order that we can hard-code instead of sorting at runtime. Verified by
/// direct byte comparison of the key strings:
///
///   "action" < "authority_scope"          (index 1: 'c' 0x63 < 'u' 0x75)
///   "authority_scope" < "commitment_id"   (index 0: 'a' 0x61 < 'c' 0x63)
///   "commitment_id" < "configuration_version" (index 2: 'm' 0x6d < 'n' 0x6e)
///   "configuration_version" < "mode_version"  (index 0: 'c' 0x63 < 'm' 0x6d)
///   "mode_version" < "outcome_positive"       (index 0: 'm' 0x6d < 'o' 0x6f)
///   "outcome_positive" < "policy_version"     (index 0: 'o' 0x6f < 'p' 0x70)
///   "policy_version" < "reason"                (index 0: 'p' 0x70 < 'r' 0x72)
///   "reason" < "supersedes"                    (index 0: 'r' 0x72 < 's' 0x73)
///
/// and, inside the nested `supersedes` object:
///
///   "commitment_hash" < "session_id"           (index 0: 'c' 0x63 < 's' 0x73)
///
/// `supersedes` is omitted from the object entirely when `None` (unset, not
/// merely empty) -- see the `cmt_hash_003` / `cmt_hash_004` test pair below,
/// which is the only thing pinning that this is implemented as omission
/// rather than "empty string fields count as absent".
///
/// Do NOT extend this fixed order if `CommitmentPayload` grows a tenth
/// field -- see the [`LABEL`] doc comment.
fn canonicalize(payload: &CommitmentPayload) -> Vec<u8> {
    let mut out = String::new();
    out.push('{');

    out.push_str("\"action\":");
    push_json_string(&mut out, &payload.action);

    out.push_str(",\"authority_scope\":");
    push_json_string(&mut out, &payload.authority_scope);

    out.push_str(",\"commitment_id\":");
    push_json_string(&mut out, &payload.commitment_id);

    out.push_str(",\"configuration_version\":");
    push_json_string(&mut out, &payload.configuration_version);

    out.push_str(",\"mode_version\":");
    push_json_string(&mut out, &payload.mode_version);

    out.push_str(",\"outcome_positive\":");
    out.push_str(if payload.outcome_positive {
        "true"
    } else {
        "false"
    });

    out.push_str(",\"policy_version\":");
    push_json_string(&mut out, &payload.policy_version);

    out.push_str(",\"reason\":");
    push_json_string(&mut out, &payload.reason);

    if let Some(ref supersedes) = payload.supersedes {
        out.push_str(",\"supersedes\":{\"commitment_hash\":");
        push_json_string(&mut out, &supersedes.commitment_hash);
        out.push_str(",\"session_id\":");
        push_json_string(&mut out, &supersedes.session_id);
        out.push('}');
    }

    out.push('}');
    out.into_bytes()
}

/// Append `value` as a JCS-canonical JSON string literal (including
/// surrounding quotes) to `out`, per RFC 8785 §3.2.2.2.
///
/// - `"`, `\`, and the controls with short forms (`\b \t \n \f \r`) use
///   their short escape.
/// - Any other C0 control character (0x00-0x1F) is emitted as `\u00XX`.
/// - Everything else -- including non-ASCII BMP characters and astral-plane
///   codepoints -- is emitted as literal UTF-8. Rust's `char` is always a
///   valid Unicode scalar value, so astral-plane codepoints round-trip as a
///   single `char` here with no manual UTF-16 surrogate-pair encoding.
///
/// Key names are not run through this function: the nine projection keys
/// are a fixed compile-time list already known to need no escaping.
fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                write!(out, "\\u{:04x}", c as u32).expect("write! to String never fails");
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use macp_pb::pb::CommitmentRef;

    /// Decode a lowercase-hex string into bytes (test-only; no `hex` crate
    /// dependency -- see the "exactly one new dependency" constraint).
    fn decode_hex(s: &str) -> Vec<u8> {
        assert_eq!(s.len() % 2, 0, "hex string must have even length");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex byte"))
            .collect()
    }

    fn base_payload() -> CommitmentPayload {
        CommitmentPayload {
            commitment_id: String::new(),
            action: String::new(),
            authority_scope: String::new(),
            reason: String::new(),
            mode_version: String::new(),
            policy_version: String::new(),
            configuration_version: String::new(),
            outcome_positive: false,
            supersedes: None,
        }
    }

    // --- spec reference vectors (RFC-MACP-0013 conformance fixtures) ---
    // Payload field values are copied verbatim from
    // schemas/conformance/cmt-hash/cmt_hash_00{1..5}_*.json in the sibling
    // multiagentcoordinationprotocol repo.

    #[test]
    fn cmt_hash_001_minimal() {
        let payload = CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.approved".into(),
            authority_scope: "seam".into(),
            reason: "sealed by seam".into(),
            mode_version: "macp.mode.decision.v1".into(),
            policy_version: "1.0.0".into(),
            configuration_version: "1.0.0".into(),
            outcome_positive: true,
            supersedes: None,
        };

        let jcs_hex = "7b22616374696f6e223a226465636973696f6e2e617070726f766564222c22617574686f726974795f73636f7065223a227365616d222c22636f6d6d69746d656e745f6964223a226331222c22636f6e66696775726174696f6e5f76657273696f6e223a22312e302e30222c226d6f64655f76657273696f6e223a226d6163702e6d6f64652e6465636973696f6e2e7631222c226f7574636f6d655f706f736974697665223a747275652c22706f6c6963795f76657273696f6e223a22312e302e30222c22726561736f6e223a227365616c6564206279207365616d227d";
        assert_eq!(
            canonicalize(&payload),
            decode_hex(jcs_hex),
            "cmt_hash_001_minimal: JCS bytes mismatch"
        );

        assert_eq!(
            commitment_hash(&payload),
            "sha256:9f58e9d114d11860d48aa2bcb8cda458b9618b1cc8560595a802b68c4af85d41",
            "cmt_hash_001_minimal: hash mismatch"
        );
    }

    #[test]
    fn cmt_hash_002_supersedes() {
        let payload = CommitmentPayload {
            commitment_id: "c2".into(),
            action: "decision.approved".into(),
            authority_scope: "seam".into(),
            reason: "sealed by seam".into(),
            mode_version: "macp.mode.decision.v1".into(),
            policy_version: "1.0.0".into(),
            configuration_version: "1.0.0".into(),
            outcome_positive: true,
            supersedes: Some(CommitmentRef {
                session_id: "prior-sess".into(),
                commitment_hash:
                    "sha256:9f58e9d114d11860d48aa2bcb8cda458b9618b1cc8560595a802b68c4af85d41".into(),
            }),
        };

        assert_eq!(
            commitment_hash(&payload),
            "sha256:7cc490432ad6b25e9c19fc7c3a84f1e33abe497fca1fd5266ff0275db3650f9d",
            "cmt_hash_002_supersedes: hash mismatch"
        );
    }

    #[test]
    fn cmt_hash_003_all_empty() {
        let payload = base_payload();

        assert_eq!(
            commitment_hash(&payload),
            "sha256:3240d1a7adb7bd9420ad5490182227ce699c9e4e465f7934885fe2ded939f32e",
            "cmt_hash_003_all_empty: hash mismatch"
        );
    }

    #[test]
    fn cmt_hash_004_empty_supersedes() {
        let mut payload = base_payload();
        payload.supersedes = Some(CommitmentRef {
            session_id: String::new(),
            commitment_hash: String::new(),
        });

        assert_eq!(
            commitment_hash(&payload),
            "sha256:9776c22ef165f26817f89bb456cf6bc56a659eb1561a576f6ea9a435bd3291d7",
            "cmt_hash_004_empty_supersedes: hash mismatch"
        );
    }

    #[test]
    fn cmt_hash_003_and_004_hashes_differ() {
        // The sole check that `supersedes: None` (unset) and
        // `supersedes: Some(all-empty)` (empty) project differently, i.e.
        // that omission-when-None is actually implemented rather than
        // "empty sub-fields are treated as absent".
        let without = commitment_hash(&base_payload());
        let mut with_empty_supersedes = base_payload();
        with_empty_supersedes.supersedes = Some(CommitmentRef {
            session_id: String::new(),
            commitment_hash: String::new(),
        });
        let with = commitment_hash(&with_empty_supersedes);

        assert_ne!(
            without, with,
            "supersedes:None and supersedes:Some(empty) must hash differently"
        );
    }

    #[test]
    fn cmt_hash_005_escapes() {
        let payload = CommitmentPayload {
            commitment_id: "c5".into(),
            action: "decision.\"appro\\ved\"".into(),
            authority_scope: "café".into(),
            reason: "ré\tsumé\n— naïve \u{1F702}".into(),
            mode_version: "macp.mode.decision.v1".into(),
            policy_version: "1.0.0".into(),
            configuration_version: "1.0.0".into(),
            outcome_positive: false,
            supersedes: None,
        };

        let jcs_hex = "7b22616374696f6e223a226465636973696f6e2e5c22617070726f5c5c7665645c22222c22617574686f726974795f73636f7065223a22636166c3a9222c22636f6d6d69746d656e745f6964223a226335222c22636f6e66696775726174696f6e5f76657273696f6e223a22312e302e30222c226d6f64655f76657273696f6e223a226d6163702e6d6f64652e6465636973696f6e2e7631222c226f7574636f6d655f706f736974697665223a66616c73652c22706f6c6963795f76657273696f6e223a22312e302e30222c22726561736f6e223a2272c3a95c7473756dc3a95c6ee28094206e61c3af766520f09f9c82227d";
        assert_eq!(
            canonicalize(&payload),
            decode_hex(jcs_hex),
            "cmt_hash_005_escapes: JCS bytes mismatch"
        );

        assert_eq!(
            commitment_hash(&payload),
            "sha256:03f8ac2b8172958504092ce9fe5154dbcfe300fd30a350453d4e4bd715822ab2",
            "cmt_hash_005_escapes: hash mismatch"
        );
    }

    // --- isolated escaping unit tests ---

    fn escape(value: &str) -> String {
        let mut out = String::new();
        push_json_string(&mut out, value);
        out
    }

    #[test]
    fn escapes_embedded_quote() {
        assert_eq!(escape("a\"b"), "\"a\\\"b\"");
    }

    #[test]
    fn escapes_embedded_backslash() {
        assert_eq!(escape("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn escapes_tab() {
        assert_eq!(escape("a\tb"), "\"a\\tb\"");
    }

    #[test]
    fn escapes_newline() {
        assert_eq!(escape("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn escapes_other_c0_control_as_u00xx() {
        // 0x01 (SOH) has no short form.
        assert_eq!(escape("a\u{01}b"), "\"a\\u0001b\"");
    }

    #[test]
    fn non_ascii_bmp_char_is_emitted_literally() {
        assert_eq!(escape("café"), "\"café\"");
    }

    #[test]
    fn astral_plane_codepoint_is_emitted_literally_not_as_surrogate_pair() {
        // U+1F702 ALCHEMICAL SYMBOL FOR VINEGAR -- vector 005's stress case.
        // RFC 8785 leaves non-BMP characters unescaped; this must NOT be
        // hand-rolled into a 🜂 surrogate pair.
        let value = "\u{1F702}";
        let escaped = escape(value);
        assert_eq!(escaped, "\"\u{1F702}\"");
        assert!(!escaped.contains("\\u"));
    }
}
