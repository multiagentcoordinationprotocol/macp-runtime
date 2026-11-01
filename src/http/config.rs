//! Startup configuration of the HTTP listener, read once from the
//! environment and validated before anything binds.
//!
//! The listener exists only when `MACP_HTTP_BIND_ADDR` is set (an empty value
//! counts as unset); then every other `MACP_HTTP_*` variable is validated and
//! any error is fatal at startup. With it unset nothing here is read, so an
//! unset listener has no configuration surface at all.
//!
//! | Variable | Default | Meaning |
//! |----------|---------|---------|
//! | `MACP_HTTP_BIND_ADDR` | unset (listener off) | Socket address of the HTTP listener; must not collide with `MACP_BIND_ADDR` |
//! | `MACP_HTTP_PUBLIC_URL` | unset | The binding's `{base}` (RFC-MACP-0006 §4.1): absolute, no trailing slash, no query or fragment, `https` unless `MACP_ALLOW_INSECURE=1`. Its path is the route prefix |
//! | `MACP_HTTP_CORS_ORIGINS` | empty (closed) | Comma list of `scheme://host[:port]` origins allowed cross-origin (§4.10); `*` is refused |
//! | `MACP_HTTP_MAX_BODY_BYTES` | `ceil(4/3 · MACP_MAX_PAYLOAD_BYTES) + 65536` | Request body cap (§4.9); a value below that floor is refused |
//! | `MACP_HTTP_MAX_CONNECTIONS` | `1024` | Concurrent connections the accept loop admits |
//!
//! Reused exactly as the gRPC listener reads them (same defaults, same
//! fallback on an unparseable value): `MACP_TLS_CERT_PATH`/`MACP_TLS_KEY_PATH`,
//! `MACP_ALLOW_INSECURE`, `MACP_MAX_PAYLOAD_BYTES`, `MACP_REQUEST_TIMEOUT_SECS`
//! and `MACP_MAX_CONCURRENT_STREAMS` (the HTTP/2 stream bound).
//! `MACP_SHUTDOWN_DRAIN_SECS` is applied by `main`, to both listeners at once.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

/// Default `MACP_HTTP_MAX_CONNECTIONS`.
pub const DEFAULT_MAX_CONNECTIONS: usize = 1024;
/// Room for the rest of a request around a base64 payload at the limit.
pub const BODY_OVERHEAD_BYTES: u64 = 64 * 1024;

const DEFAULT_GRPC_BIND_ADDR: &str = "127.0.0.1:50051";
const DEFAULT_MAX_PAYLOAD_BYTES: u64 = 1_048_576;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_CONCURRENT_STREAMS: u32 = 128;

/// PEM files of the listener's TLS identity (shared with gRPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// The validated listener configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HttpConfig {
    pub bind_addr: SocketAddr,
    /// `MACP_HTTP_PUBLIC_URL` as configured, if any.
    pub public_url: Option<String>,
    /// The path of `public_url`: `""` or `/seg[/seg...]`, never ending in `/`.
    pub prefix: String,
    /// Allowed CORS origins, scheme and host lowercased.
    pub cors_origins: Vec<String>,
    pub max_body_bytes: usize,
    pub max_connections: usize,
    /// `None` only when `MACP_ALLOW_INSECURE=1` (plaintext).
    pub tls: Option<TlsPaths>,
    pub request_timeout: Duration,
    pub max_concurrent_streams: u32,
}

impl HttpConfig {
    /// Read and validate the configuration through `get` (an environment
    /// lookup). `Ok(None)` when `MACP_HTTP_BIND_ADDR` is unset or empty; every
    /// problem found is reported, each prefixed with its variable name.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Option<Self>, Vec<String>> {
        let Some(bind_raw) = get("MACP_HTTP_BIND_ADDR").filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let mut errors = Vec::new();
        let allow_insecure = get("MACP_ALLOW_INSECURE").as_deref() == Some("1");

        let bind_addr = match bind_raw.parse::<SocketAddr>() {
            Ok(addr) => {
                let grpc = get("MACP_BIND_ADDR")
                    .unwrap_or_else(|| DEFAULT_GRPC_BIND_ADDR.into())
                    .parse::<SocketAddr>()
                    .ok();
                if let Some(grpc) = grpc.filter(|g| collides(*g, addr)) {
                    errors.push(format!(
                        "MACP_HTTP_BIND_ADDR: '{bind_raw}' collides with the gRPC listener's \
                         address {grpc} (MACP_BIND_ADDR); the HTTP listener needs its own port"
                    ));
                }
                Some(addr)
            }
            Err(_) => {
                errors.push(format!(
                    "MACP_HTTP_BIND_ADDR: '{bind_raw}' is not a valid socket address \
                     (expected host:port, e.g. 127.0.0.1:8080 or [::1]:8080)"
                ));
                None
            }
        };

        let (public_url, prefix) = match get("MACP_HTTP_PUBLIC_URL") {
            None => (None, String::new()),
            Some(raw) => match parse_public_url(&raw, allow_insecure) {
                Ok(prefix) => (Some(raw), prefix),
                Err(e) => {
                    errors.push(format!("MACP_HTTP_PUBLIC_URL: {e}"));
                    (None, String::new())
                }
            },
        };

        let cors_origins = match get("MACP_HTTP_CORS_ORIGINS") {
            None => Vec::new(),
            Some(raw) => match parse_cors_origins(&raw) {
                Ok(list) => list,
                Err(e) => {
                    errors.push(format!("MACP_HTTP_CORS_ORIGINS: {e}"));
                    Vec::new()
                }
            },
        };

        let max_payload = get("MACP_MAX_PAYLOAD_BYTES")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_PAYLOAD_BYTES);
        let max_body_bytes = match body_floor(max_payload) {
            None => {
                errors.push(format!(
                    "MACP_HTTP_MAX_BODY_BYTES: the body cap derived from MACP_MAX_PAYLOAD_BYTES \
                     ({max_payload}) overflows"
                ));
                0
            }
            Some(floor) => match get("MACP_HTTP_MAX_BODY_BYTES") {
                None => floor,
                Some(raw) => match raw.parse::<u64>() {
                    Ok(v) if v >= floor => v,
                    Ok(v) => {
                        errors.push(format!(
                            "MACP_HTTP_MAX_BODY_BYTES: {v} is below the floor of {floor} \
                             (ceil(4/3 x MACP_MAX_PAYLOAD_BYTES {max_payload}) + {BODY_OVERHEAD_BYTES}), \
                             so a payload_b64 at the payload limit could not reach the payload \
                             check (RFC-MACP-0006 section 4.9)"
                        ));
                        0
                    }
                    Err(_) => {
                        errors.push(format!(
                            "MACP_HTTP_MAX_BODY_BYTES: '{raw}' is not a valid positive integer"
                        ));
                        0
                    }
                },
            },
        };
        let max_body_bytes = usize::try_from(max_body_bytes).unwrap_or(usize::MAX);

        let max_connections = match get("MACP_HTTP_MAX_CONNECTIONS") {
            None => DEFAULT_MAX_CONNECTIONS,
            Some(raw) => match raw.parse::<usize>() {
                Ok(v) if (1..=tokio::sync::Semaphore::MAX_PERMITS).contains(&v) => v,
                _ => {
                    errors.push(format!(
                        "MACP_HTTP_MAX_CONNECTIONS: '{raw}' is not a valid positive integer"
                    ));
                    0
                }
            },
        };

        // Same rule as the gRPC listener: both PEM paths give TLS; otherwise
        // plaintext only under MACP_ALLOW_INSECURE=1.
        let tls = match (get("MACP_TLS_CERT_PATH"), get("MACP_TLS_KEY_PATH")) {
            (Some(cert), Some(key)) => Some(TlsPaths {
                cert: cert.into(),
                key: key.into(),
            }),
            _ => {
                if !allow_insecure {
                    errors.push(
                        "MACP_HTTP_BIND_ADDR: the HTTP listener requires TLS \
                         (MACP_TLS_CERT_PATH and MACP_TLS_KEY_PATH) unless MACP_ALLOW_INSECURE=1 \
                         is set"
                            .into(),
                    );
                }
                None
            }
        };

        let request_timeout = Duration::from_secs(
            get("MACP_REQUEST_TIMEOUT_SECS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS),
        );
        let max_concurrent_streams = get("MACP_MAX_CONCURRENT_STREAMS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_CONCURRENT_STREAMS);

        match bind_addr {
            Some(bind_addr) if errors.is_empty() => Ok(Some(Self {
                bind_addr,
                public_url,
                prefix,
                cors_origins,
                max_body_bytes,
                max_connections,
                tls,
                request_timeout,
                max_concurrent_streams,
            })),
            _ => Err(errors),
        }
    }
}

/// True when binding `http` would fight the gRPC listener for `grpc`'s port.
/// Port 0 (ephemeral) never collides.
fn collides(grpc: SocketAddr, http: SocketAddr) -> bool {
    grpc.port() != 0
        && grpc.port() == http.port()
        && (grpc.ip() == http.ip() || grpc.ip().is_unspecified() || http.ip().is_unspecified())
}

/// `ceil(4/3 · payload) + 64 KiB`, the §4.9 floor of the body cap.
pub fn body_floor(max_payload: u64) -> Option<u64> {
    max_payload
        .checked_mul(4)
        .map(|v| v.div_ceil(3))
        .and_then(|v| v.checked_add(BODY_OVERHEAD_BYTES))
}

/// Split `scheme://authority<rest>`; `None` unless the scheme is `http` or
/// `https` (case-insensitive) and the authority is a non-empty host without
/// userinfo.
fn split_origin(raw: &str) -> Option<(String, &str, &str)> {
    let (scheme, after) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let end = after.find('/').unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    // Let `http` judge the host and port syntax.
    let parsed: http::uri::Authority = authority.parse().ok()?;
    if parsed.host().is_empty() {
        return None;
    }
    Some((scheme, authority, rest))
}

/// Validate `MACP_HTTP_PUBLIC_URL` and return its path (the route prefix).
fn parse_public_url(raw: &str, allow_insecure: bool) -> Result<String, String> {
    if raw.contains('?') || raw.contains('#') {
        return Err(format!("'{raw}' must not carry a query or fragment"));
    }
    if raw
        .chars()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
    {
        return Err(format!("'{raw}' must not contain whitespace"));
    }
    let Some((scheme, _, path)) = split_origin(raw) else {
        return Err(format!(
            "'{raw}' is not an absolute http(s) URL (expected https://host[:port][/prefix])"
        ));
    };
    if scheme != "https" && !allow_insecure {
        return Err(format!(
            "'{raw}' must use https unless MACP_ALLOW_INSECURE=1 is set"
        ));
    }
    if path.ends_with('/') || raw.ends_with('/') {
        return Err(format!("'{raw}' must not end with a trailing slash"));
    }
    if path.contains("//") {
        return Err(format!("'{raw}' has an empty path segment"));
    }
    Ok(path.to_string())
}

/// Validate the comma list of `MACP_HTTP_CORS_ORIGINS`.
fn parse_cors_origins(raw: &str) -> Result<Vec<String>, String> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in raw.split(',').map(str::trim) {
        if entry == "*" {
            return Err(
                "'*' is refused: list each allowed origin explicitly (RFC-MACP-0006 section 4.10)"
                    .into(),
            );
        }
        if entry.is_empty() {
            return Err(format!("'{raw}' has an empty entry"));
        }
        match split_origin(entry) {
            Some((scheme, authority, "")) => {
                let origin = format!("{scheme}://{}", authority.to_ascii_lowercase());
                if !out.contains(&origin) {
                    out.push(origin);
                }
            }
            _ => {
                return Err(format!(
                    "'{entry}' is not an origin (expected scheme://host[:port], no path)"
                ))
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(vars: &[(&str, &str)]) -> Result<Option<HttpConfig>, Vec<String>> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        HttpConfig::from_env(&move |k| map.get(k).cloned())
    }

    fn ok(vars: &[(&str, &str)]) -> HttpConfig {
        let mut all = vec![
            ("MACP_HTTP_BIND_ADDR", "127.0.0.1:8080"),
            ("MACP_ALLOW_INSECURE", "1"),
        ];
        all.extend_from_slice(vars);
        cfg(&all).expect("valid").expect("enabled")
    }

    fn err(vars: &[(&str, &str)]) -> String {
        let mut all = vec![
            ("MACP_HTTP_BIND_ADDR", "127.0.0.1:8080"),
            ("MACP_ALLOW_INSECURE", "1"),
        ];
        // Later entries override earlier ones.
        for (k, v) in vars {
            all.retain(|(ek, _)| ek != k);
            all.push((k, v));
        }
        cfg(&all).expect_err("invalid").join("; ")
    }

    #[test]
    fn unset_or_empty_bind_addr_is_off_and_reads_nothing_else() {
        assert_eq!(cfg(&[]), Ok(None));
        assert_eq!(cfg(&[("MACP_HTTP_BIND_ADDR", "")]), Ok(None));
        // Other HTTP variables are not judged while the listener is off.
        assert_eq!(cfg(&[("MACP_HTTP_MAX_CONNECTIONS", "nope")]), Ok(None));
    }

    #[test]
    fn defaults() {
        let c = ok(&[]);
        assert_eq!(c.bind_addr, "127.0.0.1:8080".parse().unwrap());
        assert_eq!(c.prefix, "");
        assert_eq!(c.public_url, None);
        assert!(c.cors_origins.is_empty());
        assert_eq!(
            c.max_body_bytes as u64,
            (4 * 1_048_576u64).div_ceil(3) + 65536
        );
        assert_eq!(c.max_connections, 1024);
        assert_eq!(c.tls, None);
        assert_eq!(c.request_timeout, Duration::from_secs(30));
        assert_eq!(c.max_concurrent_streams, 128);
    }

    #[test]
    fn reused_variables_follow_grpc() {
        let c = ok(&[
            ("MACP_TLS_CERT_PATH", "/c.pem"),
            ("MACP_TLS_KEY_PATH", "/k.pem"),
            ("MACP_REQUEST_TIMEOUT_SECS", "7"),
            ("MACP_MAX_CONCURRENT_STREAMS", "9"),
            ("MACP_MAX_PAYLOAD_BYTES", "3"),
        ]);
        assert_eq!(
            c.tls,
            Some(TlsPaths {
                cert: "/c.pem".into(),
                key: "/k.pem".into()
            })
        );
        assert_eq!(c.request_timeout, Duration::from_secs(7));
        assert_eq!(c.max_concurrent_streams, 9);
        assert_eq!(c.max_body_bytes, 4 + 65536);
        // Unparseable reused values fall back exactly as main.rs does.
        let c = ok(&[("MACP_REQUEST_TIMEOUT_SECS", "x")]);
        assert_eq!(c.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn bind_addr_validation() {
        assert!(err(&[("MACP_HTTP_BIND_ADDR", "nope")]).contains("not a valid socket address"));
        assert!(err(&[("MACP_HTTP_BIND_ADDR", "127.0.0.1:50051")]).contains("collides"));
        assert!(err(&[
            ("MACP_HTTP_BIND_ADDR", "0.0.0.0:9000"),
            ("MACP_BIND_ADDR", "127.0.0.1:9000")
        ])
        .contains("collides"));
        assert_eq!(
            ok(&[("MACP_HTTP_BIND_ADDR", "[::1]:8080")]).bind_addr,
            "[::1]:8080".parse().unwrap()
        );
        ok(&[("MACP_HTTP_BIND_ADDR", "127.0.0.2:50051")]);
        ok(&[
            ("MACP_HTTP_BIND_ADDR", "127.0.0.1:0"),
            ("MACP_BIND_ADDR", "127.0.0.1:0"),
        ]);
    }

    #[test]
    fn public_url_validation() {
        assert_eq!(
            ok(&[("MACP_HTTP_PUBLIC_URL", "https://h.example")]).prefix,
            ""
        );
        let c = ok(&[("MACP_HTTP_PUBLIC_URL", "https://h.example:8443/api/macp")]);
        assert_eq!(c.prefix, "/api/macp");
        assert_eq!(
            c.public_url.as_deref(),
            Some("https://h.example:8443/api/macp")
        );
        assert_eq!(
            ok(&[("MACP_HTTP_PUBLIC_URL", "https://h/macp/v1")]).prefix,
            "/macp/v1"
        );
        assert_eq!(
            ok(&[("MACP_HTTP_PUBLIC_URL", "http://[::1]:8080/x")]).prefix,
            "/x"
        );
        for (raw, needle) in [
            ("https://h/", "trailing slash"),
            ("https://h/a/", "trailing slash"),
            ("https://h/a?x=1", "query or fragment"),
            ("https://h#f", "query or fragment"),
            ("/relative", "absolute"),
            ("h.example", "absolute"),
            ("ftp://h", "absolute"),
            ("https://", "absolute"),
            ("https://u@h", "absolute"),
            ("https://h/a//b", "empty path segment"),
            ("https://h/a b", "whitespace"),
        ] {
            let e = err(&[("MACP_HTTP_PUBLIC_URL", raw)]);
            assert!(
                e.contains("MACP_HTTP_PUBLIC_URL") && e.contains(needle),
                "{raw}: {e}"
            );
        }
        // http needs MACP_ALLOW_INSECURE=1.
        let e = err(&[
            ("MACP_ALLOW_INSECURE", "0"),
            ("MACP_TLS_CERT_PATH", "/c"),
            ("MACP_TLS_KEY_PATH", "/k"),
            ("MACP_HTTP_PUBLIC_URL", "http://h"),
        ]);
        assert!(e.contains("must use https"), "{e}");
    }

    #[test]
    fn cors_validation() {
        assert_eq!(
            ok(&[(
                "MACP_HTTP_CORS_ORIGINS",
                "https://App.Example, http://localhost:3000,https://app.example"
            )])
            .cors_origins,
            vec!["https://app.example", "http://localhost:3000"]
        );
        assert!(ok(&[("MACP_HTTP_CORS_ORIGINS", " ")])
            .cors_origins
            .is_empty());
        for (raw, needle) in [
            ("*", "'*' is refused"),
            ("https://a,*", "'*' is refused"),
            ("https://a,,https://b", "empty entry"),
            ("https://a/", "not an origin"),
            ("https://a/path", "not an origin"),
            ("a.example", "not an origin"),
            ("null", "not an origin"),
        ] {
            let e = err(&[("MACP_HTTP_CORS_ORIGINS", raw)]);
            assert!(
                e.contains("MACP_HTTP_CORS_ORIGINS") && e.contains(needle),
                "{raw}: {e}"
            );
        }
    }

    #[test]
    fn body_cap_validation() {
        let floor = body_floor(1_048_576).unwrap();
        assert_eq!(floor, 1_398_102 + 65_536);
        assert_eq!(
            ok(&[("MACP_HTTP_MAX_BODY_BYTES", &floor.to_string())]).max_body_bytes as u64,
            floor
        );
        let e = err(&[("MACP_HTTP_MAX_BODY_BYTES", &(floor - 1).to_string())]);
        assert!(e.contains("below the floor"), "{e}");
        assert!(err(&[("MACP_HTTP_MAX_BODY_BYTES", "big")]).contains("not a valid positive"));
        assert!(err(&[("MACP_HTTP_MAX_BODY_BYTES", "0")]).contains("below the floor"));
        assert_eq!(body_floor(u64::MAX), None);
        assert_eq!(body_floor(1), Some(2 + 65536));
        assert_eq!(body_floor(3), Some(4 + 65536));
    }

    #[test]
    fn max_connections_validation() {
        assert_eq!(ok(&[("MACP_HTTP_MAX_CONNECTIONS", "5")]).max_connections, 5);
        for raw in ["0", "-1", "x", ""] {
            let e = err(&[("MACP_HTTP_MAX_CONNECTIONS", raw)]);
            assert!(e.contains("MACP_HTTP_MAX_CONNECTIONS"), "{raw}: {e}");
        }
    }

    #[test]
    fn plaintext_requires_allow_insecure() {
        let e = err(&[("MACP_ALLOW_INSECURE", "0")]);
        assert!(e.contains("requires TLS"), "{e}");
        // One PEM path alone is not TLS (same rule as gRPC).
        let e = err(&[("MACP_ALLOW_INSECURE", "0"), ("MACP_TLS_CERT_PATH", "/c")]);
        assert!(e.contains("requires TLS"), "{e}");
    }

    #[test]
    fn every_error_is_reported_at_once() {
        let errors = cfg(&[
            ("MACP_HTTP_BIND_ADDR", "bad"),
            ("MACP_HTTP_PUBLIC_URL", "https://h/"),
            ("MACP_HTTP_CORS_ORIGINS", "*"),
            ("MACP_HTTP_MAX_CONNECTIONS", "0"),
        ])
        .unwrap_err();
        assert_eq!(errors.len(), 5, "{errors:?}"); // + TLS required
    }
}
