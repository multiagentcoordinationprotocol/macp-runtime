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
//! [`commitment_hash`] never validates the payload and never fails (see its
//! doc for the D3 guarantee). Semantic validation of `CommitmentPayload`
//! (matching session-bound versions, well-formed `supersedes`, etc.) is a
//! separate concern handled elsewhere (see `macp-modes::mode::util`).
//!
//! # Verification (RFC-MACP-0013 §5, §6)
//!
//! Verification is a separate operation from computation, because it has a
//! third outcome computation cannot express. [`verify_commitment_hash`] takes
//! the payload's **protobuf wire encoding** — not a decoded
//! [`CommitmentPayload`] — and a claimed hash, and returns exactly one
//! [`VerifyOutcome`]:
//!
//! - [`VerifyOutcome::Match`] — the label covers the payload and the hash
//!   agrees.
//! - [`VerifyOutcome::Mismatch`] — the label covers the payload and the hash
//!   disagrees, **or** the claimed value is not canonical (§9: the literal
//!   `sha256:` prefix and 64 lowercase hex characters, nothing else — no
//!   trimming, no uppercase, no bare hex).
//! - [`VerifyOutcome::CannotVerify`] — the payload carries a field outside the
//!   nine-field `macp-commitment-hash/1` set (or a field other than tags 1–2
//!   inside `supersedes`), or the bytes are not a decodable
//!   `CommitmentPayload`. §5 forbids treating that as a match on the fields
//!   the label does cover.
//!
//! The wire encoding is required because a decoder silently drops unknown
//! fields: once bytes are a `CommitmentPayload` struct, a tenth field is no
//! longer observable and a verifier would "match" a payload the label does not
//! cover. That is also why there is deliberately no `verify` overload taking a
//! decoded struct.

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

/// Check whether `s` is a canonical RFC-MACP-0013 commitment hash (§9): the
/// literal prefix `sha256:` followed by exactly 64 lowercase hex characters.
/// No trimming is performed — leading/trailing whitespace is a rejection, not
/// something to be trimmed away before checking.
pub fn is_canonical_commitment_hash(s: &str) -> bool {
    match s.strip_prefix("sha256:") {
        Some(rest) => {
            rest.len() == 64
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }
        None => false,
    }
}

/// The outcome of [`verify_commitment_hash`] (RFC-MACP-0013 §5, §6).
///
/// `#[non_exhaustive]`: a future label may need a further outcome; match with
/// a wildcard arm that treats anything unknown as "not verified".
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The label covers the payload's shape and the claimed hash agrees.
    Match,
    /// The label covers the payload's shape but the claimed hash disagrees
    /// with `computed`, or the claimed value is not canonical (§9).
    Mismatch {
        /// The hash recomputed from the wire bytes, for diagnostics.
        computed: String,
    },
    /// The payload's shape is outside what `macp-commitment-hash/1` covers.
    CannotVerify(CannotVerify),
}

impl VerifyOutcome {
    /// `true` only for [`VerifyOutcome::Match`].
    pub fn is_match(&self) -> bool {
        matches!(self, VerifyOutcome::Match)
    }
}

/// Why a payload is not hashable under `macp-commitment-hash/1`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CannotVerify {
    /// A field number outside the label's frozen set. `in_supersedes` is
    /// `true` when it sits inside `supersedes` (`CommitmentRef`, tags 1–2)
    /// rather than at the top level (`CommitmentPayload`, tags 1–9).
    UnknownField {
        /// The protobuf field number.
        tag: u32,
        /// Whether the field was inside `supersedes`.
        in_supersedes: bool,
    },
    /// The bytes are not a well-formed `CommitmentPayload` encoding (truncated,
    /// a known field with the wrong wire type, invalid UTF-8 in a string
    /// field, a group, an invalid tag, ...).
    Malformed {
        /// Decoder detail, for diagnostics only — not a stable format.
        detail: String,
    },
}

impl std::fmt::Display for CannotVerify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CannotVerify::UnknownField {
                tag,
                in_supersedes: false,
            } => write!(
                f,
                "CommitmentPayload field {tag} is outside macp-commitment-hash/1"
            ),
            CannotVerify::UnknownField {
                tag,
                in_supersedes: true,
            } => write!(
                f,
                "CommitmentPayload.supersedes field {tag} is outside macp-commitment-hash/1"
            ),
            CannotVerify::Malformed { detail } => {
                write!(f, "not a decodable CommitmentPayload: {detail}")
            }
        }
    }
}

impl std::error::Error for CannotVerify {}

/// Fields 1–9 of `CommitmentPayload` (RFC-MACP-0013 §5 table).
const PAYLOAD_TAGS: std::ops::RangeInclusive<u32> = 1..=9;
/// `CommitmentPayload.supersedes` — the one nested message the label covers.
const SUPERSEDES_TAG: u32 = 9;
/// Fields 1–2 of `CommitmentRef` (`session_id`, `commitment_hash`).
const COMMITMENT_REF_TAGS: std::ops::RangeInclusive<u32> = 1..=2;

/// Compute the RFC-MACP-0013 commitment hash of a `CommitmentPayload`'s
/// protobuf wire encoding, refusing a payload the label does not cover.
///
/// This is [`commitment_hash`] for bytes as received: it first scans every
/// field number (top level, and inside `supersedes`), returning
/// [`CannotVerify::UnknownField`] for any outside the label's set, then
/// decodes and hashes. Repeated occurrences of a known field follow protobuf
/// semantics (last scalar wins, `supersedes` merges), exactly as any conformant
/// decoder of the same bytes would see them.
pub fn commitment_hash_of_wire(wire: &[u8]) -> Result<String, CannotVerify> {
    scan_known_fields(wire, PAYLOAD_TAGS, false)?;
    let payload = <CommitmentPayload as prost::Message>::decode(wire).map_err(|e| {
        CannotVerify::Malformed {
            detail: e.to_string(),
        }
    })?;
    Ok(commitment_hash(&payload))
}

/// Verify `claimed` against the `CommitmentPayload` encoded in `wire`
/// (RFC-MACP-0013 §5, §6). See the [module docs](self) for the three
/// outcomes. `claimed` must be the full wire form, `sha256:` prefix included.
///
/// Comparison is over the canonical string, which carries no secret, so a
/// constant-time comparison is not required.
pub fn verify_commitment_hash(wire: &[u8], claimed: &str) -> VerifyOutcome {
    let computed = match commitment_hash_of_wire(wire) {
        Ok(h) => h,
        Err(reason) => return VerifyOutcome::CannotVerify(reason),
    };
    if is_canonical_commitment_hash(claimed) && claimed == computed {
        VerifyOutcome::Match
    } else {
        VerifyOutcome::Mismatch { computed }
    }
}

/// Walk the top-level fields of a protobuf message, rejecting any field number
/// outside `known`. When scanning a `CommitmentPayload`, the `supersedes`
/// field's bytes are scanned recursively against `CommitmentRef`'s set.
///
/// Hand-written rather than via `prost::encoding` (which prost documents as
/// not part of its public API). Only the wire types proto3 can emit are
/// accepted; groups (3, 4) and the reserved wire types are [`Malformed`].
///
/// [`Malformed`]: CannotVerify::Malformed
fn scan_known_fields(
    mut buf: &[u8],
    known: std::ops::RangeInclusive<u32>,
    in_supersedes: bool,
) -> Result<(), CannotVerify> {
    let malformed = |detail: &str| CannotVerify::Malformed {
        detail: detail.to_owned(),
    };
    while !buf.is_empty() {
        let key = read_varint(&mut buf).ok_or_else(|| malformed("truncated field key"))?;
        let wire_type = (key & 0x7) as u8;
        let tag = u32::try_from(key >> 3).map_err(|_| malformed("field number overflows u32"))?;
        if tag == 0 {
            return Err(malformed("field number 0"));
        }
        if !known.contains(&tag) {
            return Err(CannotVerify::UnknownField { tag, in_supersedes });
        }
        match wire_type {
            0 => {
                read_varint(&mut buf).ok_or_else(|| malformed("truncated varint"))?;
            }
            1 => buf = buf.get(8..).ok_or_else(|| malformed("truncated fixed64"))?,
            5 => buf = buf.get(4..).ok_or_else(|| malformed("truncated fixed32"))?,
            2 => {
                let len = read_varint(&mut buf).ok_or_else(|| malformed("truncated length"))?;
                let len = usize::try_from(len).map_err(|_| malformed("length overflows usize"))?;
                if len > buf.len() {
                    return Err(malformed("length-delimited field runs past the end"));
                }
                let (body, rest) = buf.split_at(len);
                if !in_supersedes && tag == SUPERSEDES_TAG {
                    scan_known_fields(body, COMMITMENT_REF_TAGS, true)?;
                }
                buf = rest;
            }
            _ => return Err(malformed("unsupported wire type (group or reserved)")),
        }
    }
    Ok(())
}

/// Decode one base-128 varint (at most 10 bytes), advancing `buf`.
fn read_varint(buf: &mut &[u8]) -> Option<u64> {
    let mut value: u64 = 0;
    for (i, &byte) in buf.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            *buf = &buf[i + 1..];
            return Some(value);
        }
    }
    None
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

    // --- verification (RFC-MACP-0013 §5, §6; proposed cmt_hash_006..009) ---

    fn vector_001() -> CommitmentPayload {
        CommitmentPayload {
            commitment_id: "c1".into(),
            action: "decision.approved".into(),
            authority_scope: "seam".into(),
            reason: "sealed by seam".into(),
            mode_version: "macp.mode.decision.v1".into(),
            policy_version: "1.0.0".into(),
            configuration_version: "1.0.0".into(),
            outcome_positive: true,
            supersedes: None,
        }
    }
    const HASH_001: &str =
        "sha256:9f58e9d114d11860d48aa2bcb8cda458b9618b1cc8560595a802b68c4af85d41";

    fn wire(payload: &CommitmentPayload) -> Vec<u8> {
        prost::Message::encode_to_vec(payload)
    }

    /// Append a length-delimited field `tag` carrying `body` to `buf`.
    fn push_len_field(buf: &mut Vec<u8>, tag: u32, body: &[u8]) {
        let mut key = u64::from(tag) << 3 | 2;
        loop {
            let byte = (key & 0x7f) as u8;
            key >>= 7;
            if key == 0 {
                buf.push(byte);
                break;
            }
            buf.push(byte | 0x80);
        }
        assert!(body.len() < 0x80, "test helper only encodes short bodies");
        buf.push(body.len() as u8);
        buf.extend_from_slice(body);
    }

    #[test]
    fn verify_match_on_vector_001() {
        assert_eq!(
            verify_commitment_hash(&wire(&vector_001()), HASH_001),
            VerifyOutcome::Match
        );
        assert_eq!(
            commitment_hash_of_wire(&wire(&vector_001())).unwrap(),
            HASH_001
        );
    }

    /// Proposed `cmt_hash_006_unknown_field`: field 99 set at top level.
    #[test]
    fn cmt_hash_006_unknown_field_is_cannot_verify() {
        let mut bytes = wire(&vector_001());
        push_len_field(&mut bytes, 99, b"x");
        assert_eq!(
            verify_commitment_hash(&bytes, HASH_001),
            VerifyOutcome::CannotVerify(CannotVerify::UnknownField {
                tag: 99,
                in_supersedes: false
            })
        );
        // The decoded struct would have dropped field 99 and hashed to 001:
        // the reason verification takes wire bytes.
        let decoded = <CommitmentPayload as prost::Message>::decode(bytes.as_slice()).unwrap();
        assert_eq!(commitment_hash(&decoded), HASH_001);
        // Field 10 — "the tenth field" §5 warns about — is the same outcome.
        let mut tenth = wire(&vector_001());
        push_len_field(&mut tenth, 10, b"");
        assert!(matches!(
            verify_commitment_hash(&tenth, HASH_001),
            VerifyOutcome::CannotVerify(CannotVerify::UnknownField { tag: 10, .. })
        ));
    }

    /// Proposed `cmt_hash_007_mismatch`: one hex digit differs.
    #[test]
    fn cmt_hash_007_mismatch() {
        let claimed = HASH_001.replacen("9f58", "9f59", 1);
        assert_eq!(
            verify_commitment_hash(&wire(&vector_001()), &claimed),
            VerifyOutcome::Mismatch {
                computed: HASH_001.into()
            }
        );
    }

    /// Proposed `cmt_hash_008_noncanonical_claim`: the right digest in
    /// uppercase hex is still a mismatch (§9), as are the other spellings.
    #[test]
    fn cmt_hash_008_noncanonical_claim_is_mismatch() {
        let upper = format!("sha256:{}", HASH_001["sha256:".len()..].to_uppercase());
        for claimed in [
            upper.as_str(),
            &HASH_001["sha256:".len()..],
            &format!(" {HASH_001}"),
            &format!("{HASH_001}\n"),
            &HASH_001.replacen("sha256:", "SHA256:", 1),
        ] {
            assert!(
                matches!(
                    verify_commitment_hash(&wire(&vector_001()), claimed),
                    VerifyOutcome::Mismatch { .. }
                ),
                "{claimed:?}"
            );
        }
    }

    /// Proposed `cmt_hash_009_unknown_supersedes_field`.
    #[test]
    fn cmt_hash_009_unknown_supersedes_field_is_cannot_verify() {
        let mut payload = vector_001();
        payload.supersedes = Some(CommitmentRef {
            session_id: "s0".into(),
            commitment_hash: HASH_001.into(),
        });
        let claimed = commitment_hash(&payload);
        let mut inner = prost::Message::encode_to_vec(payload.supersedes.as_ref().unwrap());
        push_len_field(&mut inner, 3, b"z");
        payload.supersedes = None;
        let mut bytes = wire(&payload);
        push_len_field(&mut bytes, SUPERSEDES_TAG, &inner);
        assert_eq!(
            verify_commitment_hash(&bytes, &claimed),
            VerifyOutcome::CannotVerify(CannotVerify::UnknownField {
                tag: 3,
                in_supersedes: true
            })
        );
    }

    /// The exact `wire_hex` / `claimed_hash` values proposed for the spec's
    /// `cmt_hash_006` and `cmt_hash_009` fixtures (computed independently and
    /// cross-checked against vector 001). Pinned here so the published
    /// fixtures and this implementation cannot drift apart.
    #[test]
    fn proposed_fixture_wire_hex_values() {
        const WIRE_006: &str = "0a02633112116465636973696f6e2e617070726f7665641a047365616d220e7365616c6564206279207365616d2a156d6163702e6d6f64652e6465636973696f6e2e76313205312e302e303a05312e302e3040019a060178";
        const WIRE_009: &str = "0a02633112116465636973696f6e2e617070726f7665641a047365616d220e7365616c6564206279207365616d2a156d6163702e6d6f64652e6465636973696f6e2e76313205312e302e303a05312e302e3040014a500a02733012477368613235363a396635386539643131346431313836306434386161326263623863646134353862393631386231636338353630353935613830326236386334616638356434311a017a";
        const CLAIMED_009: &str =
            "sha256:5b2e82b41d2a50095eef090cc4ef402e771a45e59e05895b6c77790f89fa2526";
        assert_eq!(
            verify_commitment_hash(&decode_hex(WIRE_006), HASH_001),
            VerifyOutcome::CannotVerify(CannotVerify::UnknownField {
                tag: 99,
                in_supersedes: false
            })
        );
        assert_eq!(
            verify_commitment_hash(&decode_hex(WIRE_009), CLAIMED_009),
            VerifyOutcome::CannotVerify(CannotVerify::UnknownField {
                tag: 3,
                in_supersedes: true
            })
        );
        let mut covered = vector_001();
        covered.supersedes = Some(CommitmentRef {
            session_id: "s0".into(),
            commitment_hash: HASH_001.into(),
        });
        assert_eq!(commitment_hash(&covered), CLAIMED_009);
        // Vector 001's bytes are exactly 006's minus the trailing field 99.
        assert_eq!(
            wire(&vector_001()),
            decode_hex(&WIRE_006[..WIRE_006.len() - "9a060178".len()])
        );
    }

    #[test]
    fn malformed_wire_is_cannot_verify() {
        let good = wire(&vector_001());
        let cases: Vec<Vec<u8>> = vec![
            good[..good.len() - 1].to_vec(), // truncated string body
            vec![0x0a],                      // key with no length
            vec![0x0b],                      // field 1, wire type 3 (group)
            vec![0x00, 0x00],                // field number 0
            vec![0x0a, 0x01, 0xff],          // invalid UTF-8 in commitment_id
            vec![0x40, 0x80],                // truncated varint for bool
            vec![0x0d, 0, 0, 0, 0],          // field 1 as fixed32: wrong wire type
        ];
        for bytes in cases {
            assert!(
                matches!(
                    verify_commitment_hash(&bytes, HASH_001),
                    VerifyOutcome::CannotVerify(CannotVerify::Malformed { .. })
                ),
                "{bytes:02x?}"
            );
        }
    }

    #[test]
    fn empty_wire_is_the_all_empty_payload() {
        // Zero bytes are a valid proto3 encoding of the all-default message
        // (vector 003), so this is hashable, not cannot-verify.
        assert_eq!(
            commitment_hash_of_wire(&[]).unwrap(),
            commitment_hash(&base_payload())
        );
    }

    /// Property: for any payload the label covers, the wire path agrees with
    /// the struct path, verify matches its own hash, and appending any field
    /// number outside 1..=9 flips the outcome to cannot-verify. A seeded
    /// xorshift keeps the case set deterministic without a proptest dependency.
    #[test]
    fn property_wire_path_agrees_with_struct_path() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let alphabet: Vec<char> = "ab\"\\\t\n\u{1}é—\u{1F702}0".chars().collect();
        let text = |seed: u64| -> String {
            (0..seed % 6)
                .map(|i| {
                    alphabet[(seed.rotate_left(i as u32 * 5) % alphabet.len() as u64) as usize]
                })
                .collect()
        };
        for _ in 0..512 {
            let r = next();
            let payload = CommitmentPayload {
                commitment_id: text(r),
                action: text(r >> 3),
                authority_scope: text(r >> 6),
                reason: text(r >> 9),
                mode_version: text(r >> 12),
                policy_version: text(r >> 15),
                configuration_version: text(r >> 18),
                outcome_positive: r & 1 == 1,
                supersedes: (r & 2 == 2).then(|| CommitmentRef {
                    session_id: text(r >> 21),
                    commitment_hash: text(r >> 24),
                }),
            };
            let bytes = wire(&payload);
            let expected = commitment_hash(&payload);
            assert_eq!(commitment_hash_of_wire(&bytes).unwrap(), expected);
            assert!(verify_commitment_hash(&bytes, &expected).is_match());

            let unknown = 10 + (next() % 10_000) as u32;
            let mut extended = bytes.clone();
            push_len_field(&mut extended, unknown, b"");
            assert_eq!(
                verify_commitment_hash(&extended, &expected),
                VerifyOutcome::CannotVerify(CannotVerify::UnknownField {
                    tag: unknown,
                    in_supersedes: false
                })
            );
        }
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
