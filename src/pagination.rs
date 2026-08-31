//! Page-token codec for `ListSessions` keyset pagination.
//!
//! A page token carries one thing: the last session ID emitted by the previous
//! page. The next page is the keyset scan `session_id > cursor` in ascending
//! byte order (see `SessionRegistry::session_ids_after`).
//!
//! # Why this format is a free choice
//!
//! `macp-proto`'s `core.proto:416-419` declares `page_token` **opaque and
//! implementation-defined** — a client must pass `next_page_token` back
//! verbatim and must not parse it. Nothing on the wire constrains the internal
//! shape, so the plaintext is simply `"v1:" + <session_id>`, base64url-encoded
//! without padding so it survives any transport that treats the token as a URL
//! or header component. The `v1:` prefix is what makes a future format change
//! *detectable and rejectable* rather than silently misinterpreted: a token
//! minted by a different scheme fails the prefix check and yields
//! `INVALID_ARGUMENT` instead of being decoded as a nonsense cursor.
//!
//! # Why there is no TTL
//!
//! The proto's "tokens are short-lived and implementation-defined; a stale
//! token yields INVALID_ARGUMENT" constrains what a client may *assume*, not
//! what a server must implement. A keyset cursor is a position in a total
//! order, not a handle into server-side state, so it cannot go stale — it stays
//! meaningful indefinitely, including when the session it names has since been
//! removed. Adding a TTL would invent a real failure mode (a client paging a
//! large registry over a slow link starts failing mid-traversal) in exchange
//! for zero correctness gain, plus a third configuration knob. The
//! `INVALID_ARGUMENT`-on-bad-token path the proto requires is fully exercised
//! by the malformed/oversized/wrong-version cases below.
//!
//! # Why there is no MAC — and the condition that voids this reasoning
//!
//! The threat a signature would address is a caller forging a cursor to reach
//! data it should not see. There is nothing to protect today: `list_sessions`
//! performs **no per-caller filtering** — `src/server.rs` binds the
//! authenticated identity as `let _identity` and discards it — and
//! `docs/deployment.md` ("Observation-surface authorization") documents this as
//! intentional: `ListSessions` and `WatchSessions` return metadata for *all*
//! sessions to any authenticated identity. A forged cursor therefore yields a
//! strict subset of what the caller can already obtain by paging from the
//! start; it cannot be used to probe for anything otherwise unreachable.
//! Signing would add key management for no gain.
//!
//! **If per-caller filtering is ever added to `ListSessions`, this conclusion
//! is void.** The cursor would then become attacker-controllable positioning
//! into a filtered set, and the no-MAC decision must be re-analyzed before that
//! filtering ships.
//!
//! # Never log the token
//!
//! A page token is attacker-chosen arbitrary bytes that reach the handler at up
//! to the configured gRPC decode limit (≈1.06 MiB by default; `src/main.rs`
//! sets `max_decoding_message_size(max_payload_bytes + 64 KiB)`), because the
//! length check here runs only *after* tonic has materialized the field. An
//! embedded newline in a raw-token log line lets a caller forge log records.
//! Any diagnostic on this path emits the [`PageTokenError`] discriminant and
//! nothing else — never the token, never a prefix of it, never anything
//! derived from its content.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Version prefix of the plaintext cursor. Bump only alongside a decoder that
/// still rejects (never reinterprets) the previous version.
const TOKEN_VERSION_PREFIX: &str = "v1:";

/// Maximum accepted encoded-token length, in bytes.
///
/// Checked *before* decoding so the decode allocation is bounded (< 768 bytes
/// of plaintext). Session IDs are UUIDs or base64url tokens; 1024 leaves room
/// for legacy and non-conforming IDs many times over.
pub(crate) const MAX_PAGE_TOKEN_CHARS: usize = 1024;

/// Why a page token was rejected.
///
/// The variants exist for **tests and tracing only**. The handler collapses
/// every one of them into a single opaque `Status` message so the token is not
/// an oracle for which check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageTokenError {
    /// Longer than [`MAX_PAGE_TOKEN_CHARS`]; rejected without decoding.
    TooLong,
    /// Not valid base64url (no padding).
    NotBase64,
    /// Decoded bytes are not valid UTF-8.
    NotUtf8,
    /// Missing the `v1:` version prefix (truncated, foreign, or future format).
    WrongVersion,
    /// Prefix present but the cursor after it is empty.
    EmptyCursor,
}

/// Encode a session ID as an opaque continuation token.
pub(crate) fn encode_page_token(session_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{TOKEN_VERSION_PREFIX}{session_id}"))
}

/// Decode a continuation token back to the cursor session ID.
///
/// Checks run in a fixed order — length, base64, UTF-8, version prefix, empty
/// cursor — with the length check first so no oversized input is ever decoded.
///
/// There is deliberately **no existence check** on the returned cursor: it is a
/// position in the ID order, not a handle to a session. Requiring the session
/// to still exist would break exactly the deleted-cursor case keyset paging
/// exists to handle.
pub(crate) fn decode_page_token(token: &str) -> Result<String, PageTokenError> {
    // 1. Length first: bounds the decode allocation below.
    if token.len() > MAX_PAGE_TOKEN_CHARS {
        return Err(PageTokenError::TooLong);
    }
    // 2. base64url, no padding.
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| PageTokenError::NotBase64)?;
    // 3. UTF-8 — checked, never `from_utf8_unchecked`.
    let plaintext = String::from_utf8(bytes).map_err(|_| PageTokenError::NotUtf8)?;
    // 4. Version prefix: also what catches truncation and foreign formats.
    let cursor = plaintext
        .strip_prefix(TOKEN_VERSION_PREFIX)
        .ok_or(PageTokenError::WrongVersion)?;
    // 5. An empty cursor is indistinguishable from "first page"; accepting it
    //    would silently restart a traversal the client believes is continuing.
    if cursor.is_empty() {
        return Err(PageTokenError::EmptyCursor);
    }
    Ok(cursor.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_round_trips_session_id() {
        let ids: Vec<String> = vec![
            // UUID v4
            "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
            // 22-char base64url token
            "AAECAwQFBgcICQoLDA0ODw".to_string(),
            // legacy, non-conforming ID
            "legacy_session_1".to_string(),
            // multi-byte UTF-8
            "session-ünïcøde-☃-世界".to_string(),
            // 200 chars
            "x".repeat(200),
        ];
        for id in &ids {
            let token = encode_page_token(id);
            assert_eq!(decode_page_token(&token), Ok(id.clone()), "id = {id}");
        }
    }

    #[test]
    fn token_is_not_the_bare_session_id() {
        for id in [
            "3f2504e0-4f89-41d3-9a0c-0305e82c3301",
            "AAECAwQFBgcICQoLDA0ODw",
            "legacy_session_1",
        ] {
            assert_ne!(encode_page_token(id), id);
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        // Valid base64url, but the plaintext is not a token.
        let garbage = URL_SAFE_NO_PAD.encode("just some bytes");
        assert_eq!(
            decode_page_token(&garbage),
            Err(PageTokenError::WrongVersion)
        );
        assert!(decode_page_token("").is_err());
    }

    #[test]
    fn decode_rejects_non_base64() {
        for token in ["not a token!", "***", "abc=", "a b c", "%%%%"] {
            assert_eq!(
                decode_page_token(token),
                Err(PageTokenError::NotBase64),
                "token literal rejected for the wrong reason"
            );
        }
    }

    #[test]
    fn decode_rejects_truncated_token() {
        let token = encode_page_token("3f2504e0-4f89-41d3-9a0c-0305e82c3301");
        // Truncating the *encoded* form either breaks base64 or drops the
        // prefix; either way it must not decode to a usable cursor.
        for cut in 1..token.len() {
            let truncated = &token[..cut];
            assert_ne!(
                decode_page_token(truncated),
                Ok("3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string()),
                "truncation at {cut} produced the original cursor"
            );
        }
        // Note the assertion above is "never the original cursor", not "always
        // an error": chopping bytes off the *end* of an encoded token can leave
        // a well-formed shorter cursor (e.g. `v1:3f2504e0`). That is harmless —
        // a cursor is a position in the ID order, not a handle — and it still
        // yields a correct, merely differently-positioned page. The truncation
        // that must be *rejected* is the one that damages the version prefix.
        for plaintext in ["v", "v1"] {
            let short = URL_SAFE_NO_PAD.encode(plaintext);
            assert_eq!(
                decode_page_token(&short),
                Err(PageTokenError::WrongVersion),
                "plaintext {plaintext:?}"
            );
        }
        // Front-truncation corrupts the base64 alignment, so the decoded bytes
        // are no longer valid UTF-8 (and could not carry the prefix
        // regardless).
        assert_ne!(
            decode_page_token(&token[1..]),
            Ok("3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string())
        );
    }

    #[test]
    fn decode_rejects_oversized_token_without_decoding() {
        // 2 MiB of valid base64url alphabet: the length branch must fire, which
        // proves no decode buffer was allocated for it.
        let huge = "A".repeat(2 * 1024 * 1024);
        assert_eq!(decode_page_token(&huge), Err(PageTokenError::TooLong));
        // The boundary itself is accepted through to the later checks.
        let at_limit = "A".repeat(MAX_PAGE_TOKEN_CHARS);
        assert_ne!(decode_page_token(&at_limit), Err(PageTokenError::TooLong));
        let over_limit = "A".repeat(MAX_PAGE_TOKEN_CHARS + 1);
        assert_eq!(decode_page_token(&over_limit), Err(PageTokenError::TooLong));
    }

    #[test]
    fn decode_rejects_wrong_version_prefix() {
        for plaintext in ["v2:abc", "v:abc", "1:abc", "abc", "V1:abc", " v1:abc"] {
            let token = URL_SAFE_NO_PAD.encode(plaintext);
            assert_eq!(
                decode_page_token(&token),
                Err(PageTokenError::WrongVersion),
                "plaintext {plaintext:?} was not rejected as a wrong version"
            );
        }
    }

    #[test]
    fn decode_rejects_valid_base64_of_invalid_utf8() {
        // Lone continuation byte / truncated multi-byte sequences.
        for bytes in [
            vec![0x76, 0x31, 0x3a, 0xff],       // "v1:" + 0xFF
            vec![0xc3, 0x28],                   // bad two-byte sequence
            vec![0xed, 0xa0, 0x80],             // UTF-16 lone surrogate D800
            vec![0x76, 0x31, 0x3a, 0xe2, 0x28], // "v1:" + truncated three-byte
        ] {
            let token = URL_SAFE_NO_PAD.encode(&bytes);
            assert_eq!(
                decode_page_token(&token),
                Err(PageTokenError::NotUtf8),
                "bytes {bytes:?} were not rejected as invalid UTF-8"
            );
        }
    }

    #[test]
    fn decode_rejects_empty_cursor_after_prefix() {
        let token = URL_SAFE_NO_PAD.encode("v1:");
        assert_eq!(decode_page_token(&token), Err(PageTokenError::EmptyCursor));
    }

    #[test]
    fn decode_never_panics_on_adversarial_input() {
        // Deterministic corpus: every call must *return*, whatever it returns.
        let seed = encode_page_token("3f2504e0-4f89-41d3-9a0c-0305e82c3301");
        let seed_bytes = seed.as_bytes();
        let mut cases: Vec<String> = Vec::new();

        // Bit flips across the whole seed token (8 per byte).
        for i in 0..seed_bytes.len() {
            for bit in 0..8u32 {
                let mut b = seed_bytes.to_vec();
                b[i] ^= 1 << bit;
                cases.push(String::from_utf8_lossy(&b).into_owned());
            }
        }
        // Truncation at every length.
        for cut in 0..=seed_bytes.len() {
            cases.push(String::from_utf8_lossy(&seed_bytes[..cut]).into_owned());
        }
        // Prefix and suffix junk.
        for junk in [
            "", "=", "==", "!", "\n", "\r\n", "\0", "v1:", "AAAA", "../", "%00", "\u{feff}",
        ] {
            cases.push(format!("{junk}{seed}"));
            cases.push(format!("{seed}{junk}"));
            cases.push(junk.to_string());
        }
        // Embedded NULs and lone-surrogate byte sequences, base64-wrapped and raw.
        for bytes in [
            vec![0x76, 0x31, 0x3a, 0x00, 0x41],
            vec![0x00; 64],
            vec![0xed, 0xa0, 0x80],
            vec![0xed, 0xbf, 0xbf],
            vec![0xff; 128],
            vec![0x76, 0x31, 0x3a, 0xed, 0xa0, 0xbd, 0xed, 0xb8, 0x80],
        ] {
            cases.push(URL_SAFE_NO_PAD.encode(&bytes));
            cases.push(String::from_utf8_lossy(&bytes).into_owned());
        }
        // Length extremes around the cap.
        for len in [0usize, 1, 2, 3, 4, 1023, 1024, 1025, 4096] {
            cases.push("A".repeat(len));
            cases.push("~".repeat(len.min(256)));
        }
        // Repeated structural fragments.
        for n in 1..200usize {
            cases.push(format!("v1:{}", "A".repeat(n)));
            cases.push(URL_SAFE_NO_PAD.encode("v1:".repeat(n)));
            cases.push(URL_SAFE_NO_PAD.encode("\u{0}".repeat(n)));
        }

        assert!(
            cases.len() >= 1000,
            "corpus too small: {} cases",
            cases.len()
        );
        for case in &cases {
            // The assertion is that this call returns at all.
            let _ = decode_page_token(case);
        }

        // And the seed itself still round-trips after all that.
        assert_eq!(
            decode_page_token(&seed),
            Ok("3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string())
        );
    }
}
