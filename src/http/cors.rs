//! CORS for browser clients (RFC-MACP-0006 §4.10), hand-written.
//!
//! The allowlist is `MACP_HTTP_CORS_ORIGINS`, empty by default, which closes
//! the binding to cross-origin access. Origins are compared after
//! [`canonical_origin`]: scheme and host lowercased, a default port (`:443`
//! on `https`, `:80` on `http`) dropped, because a browser serializes an
//! origin without one.
//!
//! - **Preflight.** Every `OPTIONS` request is answered `204`, before
//!   routing and authentication. From a listed origin it carries exactly
//!   `Access-Control-Allow-Origin: <origin>`, `Access-Control-Allow-Methods:
//!   POST`, `Access-Control-Allow-Headers: Authorization, Content-Type,
//!   Last-Event-ID, traceparent, tracestate`, `Access-Control-Max-Age: 600`
//!   and `Vary: Origin`; from any other origin (including `Origin: null`,
//!   a malformed origin, several `Origin` headers, or none) it carries no
//!   `Access-Control-Allow-*` header at all, so the browser blocks the request.
//! - **Actual responses** to a listed origin (every status, request-level
//!   errors, transport statuses and SSE streams alike, since the headers go
//!   on the response head) add `Access-Control-Allow-Origin: <origin>`,
//!   `Access-Control-Expose-Headers: MACP-Error-Code`. With an allowlist
//!   configured, every actual response (listed or not) carries
//!   `Vary: Origin`, for shared-cache safety.
//! - **Never** `Access-Control-Allow-Credentials`: the binding uses no
//!   cookies (§4.6).
//!
//! `tower-http`'s `CorsLayer` is not used: it answers a denied preflight
//! `200` and still sends `Access-Control-Allow-*` headers.

use http::header::{
    HeaderMap, HeaderValue, ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
    ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, ORIGIN,
    VARY,
};
use http::{Response, StatusCode};

use super::errors::empty;
use super::server::Body;

/// The request headers a listed origin may send (§4.10).
const ALLOW_HEADERS: &str = "Authorization, Content-Type, Last-Event-ID, traceparent, tracestate";
/// How long a browser may cache a preflight answer, in seconds.
const MAX_AGE: &str = "600";

/// `raw` as a comparable origin: `scheme://host[:port]`, scheme and host
/// lowercased, a default port dropped. `None` unless `raw` is an `http` or
/// `https` origin with a host and no userinfo, path, query or fragment; with
/// `allow_trailing_slash` a single `/` path is accepted and dropped (an
/// operator's `https://app.example/` in configuration). `null` is never an
/// origin here.
pub(crate) fn canonical_origin(raw: &str, allow_trailing_slash: bool) -> Option<String> {
    let (scheme, authority, rest) = super::config::split_origin(raw)?;
    match rest {
        "" => {}
        "/" if allow_trailing_slash => {}
        _ => return None,
    }
    let parsed: http::uri::Authority = authority.to_ascii_lowercase().parse().ok()?;
    let default_port = if scheme == "https" { 443 } else { 80 };
    Some(match parsed.port_u16() {
        Some(port) if port != default_port => format!("{scheme}://{}:{port}", parsed.host()),
        _ => format!("{scheme}://{}", parsed.host()),
    })
}

/// The request's `Origin` value, to echo, when it names a listed origin.
/// `None` for no `Origin`, several, `null`, a malformed one, or one not on
/// `allowed` (canonical origins, as [`canonical_origin`] builds them).
pub(crate) fn allowed_origin(allowed: &[String], headers: &HeaderMap) -> Option<HeaderValue> {
    if allowed.is_empty() {
        return None;
    }
    let mut values = headers.get_all(ORIGIN).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return None;
    };
    let origin = canonical_origin(value.to_str().ok()?, false)?;
    allowed.contains(&origin).then(|| value.clone())
}

/// The answer to an `OPTIONS` request: `204`, with the preflight headers
/// only when `origin` is listed.
pub(crate) fn preflight(origin: Option<HeaderValue>) -> Response<Body> {
    let mut resp = empty(StatusCode::NO_CONTENT);
    if let Some(origin) = origin {
        let headers = resp.headers_mut();
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOW_HEADERS),
        );
        headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static(MAX_AGE));
        headers.insert(VARY, HeaderValue::from_static("Origin"));
    }
    resp
}

/// Add the actual-response headers for a listed `origin`. `Vary: Origin`
/// is added separately by [`vary_origin`], to every response.
pub(crate) fn decorate(resp: &mut Response<Body>, origin: HeaderValue) {
    let headers = resp.headers_mut();
    headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("MACP-Error-Code"),
    );
}

/// Mark an actual response as varying by `Origin`. With an allowlist
/// configured, whether `Allow-Origin` is present depends on the request's
/// origin, so every response, listed or not, carries `Vary: Origin`;
/// otherwise a shared cache filled by an unlisted caller (e.g. the
/// well-known document, which has no `Cache-Control`) would serve a listed
/// browser origin a copy without `Allow-Origin` (Fetch, "CORS protocol and
/// HTTP caches").
pub(crate) fn vary_origin(resp: &mut Response<Body>) {
    resp.headers_mut()
        .append(VARY, HeaderValue::from_static("Origin"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(entries: &[&str]) -> Vec<String> {
        entries
            .iter()
            .map(|e| canonical_origin(e, true).unwrap())
            .collect()
    }

    fn origin_headers(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append(ORIGIN, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn names(resp: &Response<Body>) -> Vec<String> {
        let mut n: Vec<_> = resp.headers().keys().map(|k| k.to_string()).collect();
        n.sort();
        n
    }

    #[test]
    fn canonical_origins() {
        for (raw, slash, want) in [
            ("https://app.example", false, Some("https://app.example")),
            ("HTTPS://App.Example", false, Some("https://app.example")),
            (
                "https://app.example:443",
                false,
                Some("https://app.example"),
            ),
            ("http://app.example:80", false, Some("http://app.example")),
            (
                "https://app.example:80",
                false,
                Some("https://app.example:80"),
            ),
            (
                "http://localhost:3000",
                false,
                Some("http://localhost:3000"),
            ),
            ("http://[::1]:8080", false, Some("http://[::1]:8080")),
            ("https://app.example/", true, Some("https://app.example")),
            ("https://app.example/", false, None),
            ("https://app.example//", true, None),
            ("https://app.example/x", true, None),
            ("https://u@app.example", false, None),
            ("null", false, None),
            ("ftp://app.example", false, None),
            ("app.example", false, None),
            ("https://", false, None),
        ] {
            assert_eq!(
                canonical_origin(raw, slash).as_deref(),
                want,
                "{raw} {slash}"
            );
        }
    }

    #[test]
    fn origin_matching() {
        let allowed = list(&["https://app.example/", "http://localhost:3000"]);
        for (values, listed) in [
            (&["https://app.example"][..], true),
            (&["https://APP.example"], true),
            (&["https://app.example:443"], true),
            (&["http://localhost:3000"], true),
            // Scheme and port are part of the origin.
            (&["http://app.example"], false),
            (&["https://app.example:8443"], false),
            (&["http://localhost:3001"], false),
            (&["http://localhost"], false),
            // A subdomain or suffix is another origin.
            (&["https://evil.app.example"], false),
            (&["https://app.example.evil"], false),
            (&["null"], false),
            (&["https://app.example/"], false),
            (&[""], false),
            (&[], false),
            // Several Origin headers are not an origin.
            (&["https://app.example", "https://app.example"], false),
        ] {
            let got = allowed_origin(&allowed, &origin_headers(values));
            assert_eq!(got.is_some(), listed, "{values:?}");
            if let Some(echo) = got {
                assert_eq!(echo, values[0], "echoed verbatim");
            }
        }
        // An empty allowlist denies everything.
        assert!(allowed_origin(&[], &origin_headers(&["https://app.example"])).is_none());
    }

    #[test]
    fn listed_preflight_carries_exactly_the_section_4_10_headers() {
        let resp = preflight(Some(HeaderValue::from_static("https://app.example")));
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            names(&resp),
            [
                "access-control-allow-headers",
                "access-control-allow-methods",
                "access-control-allow-origin",
                "access-control-max-age",
                "vary",
            ]
        );
        let h = resp.headers();
        assert_eq!(h[ACCESS_CONTROL_ALLOW_ORIGIN], "https://app.example");
        assert_eq!(h[ACCESS_CONTROL_ALLOW_METHODS], "POST");
        assert_eq!(h[ACCESS_CONTROL_ALLOW_HEADERS], ALLOW_HEADERS);
        assert_eq!(h[ACCESS_CONTROL_MAX_AGE], "600");
        assert_eq!(h[VARY], "Origin");
        let allowed: Vec<_> = ALLOW_HEADERS.split(", ").collect();
        assert_eq!(
            allowed,
            [
                "Authorization",
                "Content-Type",
                "Last-Event-ID",
                "traceparent",
                "tracestate"
            ]
        );
    }

    #[test]
    fn unlisted_preflight_is_204_with_no_headers() {
        let resp = preflight(None);
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(names(&resp).is_empty(), "{:?}", names(&resp));
    }

    #[test]
    fn actual_responses_keep_their_status_and_headers() {
        let mut resp =
            super::super::errors::RequestError::new("SESSION_NOT_FOUND", "x").into_response();
        resp.headers_mut()
            .insert(VARY, HeaderValue::from_static("Accept"));
        decorate(&mut resp, HeaderValue::from_static("https://app.example"));
        vary_origin(&mut resp);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let h = resp.headers();
        assert_eq!(h[ACCESS_CONTROL_ALLOW_ORIGIN], "https://app.example");
        assert_eq!(h[ACCESS_CONTROL_EXPOSE_HEADERS], "MACP-Error-Code");
        assert_eq!(h["macp-error-code"], "SESSION_NOT_FOUND");
        let vary: Vec<_> = h.get_all(VARY).iter().collect();
        assert_eq!(vary, ["Accept", "Origin"], "an existing Vary is kept");
        assert!(!h.contains_key("access-control-allow-credentials"));
    }
}
