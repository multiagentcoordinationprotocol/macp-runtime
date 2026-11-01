//! HTTP request headers → the gRPC `MetadataMap` a `MacpServer` handler
//! reads (RFC-MACP-0006 §4.6, §4.11).
//!
//! Only three headers cross over, each copied verbatim (every value, no
//! normalisation):
//!
//! - `authorization`: the binding's only credential. The bearer scheme stays
//!   the exact-match `Bearer ` of `macp-auth` on both bindings, so a header
//!   one binding refuses (`bearer x`, `Basic …`) the other refuses too.
//! - `traceparent`, `tracestate`: W3C trace context, per request.
//!
//! Nothing else is copied. In particular `x-macp-token`, the runtime's own
//! gRPC token header, is not part of the HTTP binding and is not honoured
//! (§4.6: "a client MUST NOT rely on one"), and no `Cookie` is ever read, so
//! a cookie cannot authenticate or carry state (§4.6).

use http::header::{HeaderMap, HeaderName};
use tonic::metadata::MetadataMap;

/// The request headers that become gRPC metadata, by lowercase name.
pub(crate) const FORWARDED: [&str; 3] = ["authorization", "traceparent", "tracestate"];

/// The metadata for a request with `headers`.
pub(crate) fn metadata_from_headers(headers: &HeaderMap) -> MetadataMap {
    let mut forwarded = HeaderMap::new();
    for name in FORWARDED {
        for value in headers.get_all(name) {
            forwarded.append(HeaderName::from_static(name), value.clone());
        }
    }
    MetadataMap::from_headers(forwarded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn only_the_forwarded_headers_cross_over_verbatim() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("bearer lower"));
        h.append("authorization", HeaderValue::from_static("Bearer second"));
        h.insert("traceparent", HeaderValue::from_static("00-abc-def-01"));
        h.insert("tracestate", HeaderValue::from_static("k=v"));
        h.insert("x-macp-token", HeaderValue::from_static("tok"));
        h.insert("cookie", HeaderValue::from_static("session=1"));
        h.insert("x-other", HeaderValue::from_static("1"));
        let m = metadata_from_headers(&h);
        let auth: Vec<_> = m.get_all("authorization").iter().collect();
        assert_eq!(auth.len(), 2);
        assert_eq!(auth[0], "bearer lower", "no scheme normalisation");
        assert_eq!(auth[1], "Bearer second");
        assert_eq!(m.get("traceparent").unwrap(), "00-abc-def-01");
        assert_eq!(m.get("tracestate").unwrap(), "k=v");
        for dropped in ["x-macp-token", "cookie", "x-other"] {
            assert!(m.get(dropped).is_none(), "{dropped}");
        }
        assert_eq!(m.len(), 4);
    }
}
