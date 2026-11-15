//! The `MACP_*` environment-variable registry: the one table every `MACP_*`
//! variable the workspace reads is declared in.
//!
//! Each entry defines a `pub const` of the same name whose value is the
//! variable's name (`env::MACP_BIND_ADDR == "MACP_BIND_ADDR"`), and a row in
//! [`REGISTRY`] carrying its default, whether it is test-only, which binary
//! reads it, and the files that read it. Non-test code reads a variable only
//! through its constant (`std::env::var(env::MACP_BIND_ADDR)`); a bare
//! `"MACP_*"` string literal outside test code is refused by
//! `tests/env_registry.rs`, which also checks every row's `read_in` against
//! the files that actually use the constant. The `kb-facts.json` manifest
//! (`docs/kb-facts.json`, `tests/kb_facts.rs`) is generated from this table.
//!
//! Lives in `macp-auth` because it is the lowest crate that reads `MACP_*`
//! variables (the security layer), and the root crate re-exports it as
//! `macp_runtime::env`. `macp-core`, `macp-modes` and `macp-policy` read no
//! `MACP_*` variable outside their tests.
//!
//! `default` is documentation: the value a read falls back to when the
//! variable is unset, as the call site implements it. `None` means unset
//! leaves the feature off (or the value absent). Changing a call site's
//! default without updating its row is caught only by review and the
//! `docs/kb-facts.json` snapshot diff, not mechanically.

/// Which program reads a variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Component {
    /// The `macp-runtime` server binary (and the library it is built on).
    Runtime,
    /// The `macp` command-line client (`crates/macp-cli`).
    Cli,
    /// Test harnesses only; never read by a shipped binary.
    Test,
}

impl Component {
    /// The manifest spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Component::Runtime => "runtime",
            Component::Cli => "cli",
            Component::Test => "test",
        }
    }
}

/// One registered `MACP_*` variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnvVar {
    /// The variable name; always equal to the constant of the same name.
    pub name: &'static str,
    /// The effective value when unset, as text; `None` = unset means off/absent.
    pub default: Option<&'static str>,
    /// Read only by tests (and CI), never by a shipped binary.
    pub test_only: bool,
    /// Which program reads it.
    pub component: Component,
    /// Repository-relative files that read it, sorted. For a non-test
    /// variable: the non-test files that use its constant. For a test-only
    /// variable: the files that read it.
    pub read_in: &'static [&'static str],
    /// One-line purpose.
    pub summary: &'static str,
}

macro_rules! registry {
    ($(
        $name:ident {
            default: $default:expr,
            component: $component:ident,
            read_in: [$($file:literal),* $(,)?],
            summary: $summary:literal $(,)?
        }
    ),* $(,)?) => {
        $(
            #[doc = $summary]
            pub const $name: &str = stringify!($name);
        )*

        /// Every registered `MACP_*` variable, sorted by name.
        pub const REGISTRY: &[EnvVar] = &[
            $(EnvVar {
                name: $name,
                default: $default,
                test_only: matches!(Component::$component, Component::Test),
                component: Component::$component,
                read_in: &[$($file),*],
                summary: $summary,
            }),*
        ];
    };
}

registry! {
    MACP_ALLOW_INSECURE {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "`1` allows plaintext listeners without TLS (development only).",
    },
    MACP_AUDIT_INFO_MODES {
        default: None,
        component: Runtime,
        read_in: ["src/runtime.rs"],
        summary: "Comma-separated mode names whose sessions are audited at `info`.",
    },
    MACP_AUDIT_INFO_POLICIES {
        default: None,
        component: Runtime,
        read_in: ["src/runtime.rs"],
        summary: "Comma-separated `policy_id`s whose sessions are audited at `info`.",
    },
    MACP_AUDIT_LEVEL {
        default: Some("debug"),
        component: Runtime,
        read_in: ["src/runtime.rs"],
        summary: "`info` logs every accepted message at `info` level.",
    },
    MACP_AUTH_AUDIENCE {
        default: Some("macp-runtime"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Expected JWT `aud` claim.",
    },
    MACP_AUTH_ISSUER {
        default: None,
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Expected JWT `iss` claim; setting it enables JWT bearer auth.",
    },
    MACP_AUTH_JWKS_JSON {
        default: None,
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Inline JWKS document used to validate JWTs.",
    },
    MACP_AUTH_JWKS_TTL_SECS {
        default: Some("300"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Cache TTL, in seconds, for a JWKS fetched from `MACP_AUTH_JWKS_URL`.",
    },
    MACP_AUTH_JWKS_URL {
        default: None,
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "JWKS endpoint URL, fetched and cached.",
    },
    MACP_AUTH_JWT_ALGS {
        default: Some("RS256,ES256"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Comma-separated JWT algorithm allowlist; HS256 needs explicit opt-in.",
    },
    MACP_AUTH_TOKENS_FILE {
        default: None,
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Path to a static bearer-token configuration file.",
    },
    MACP_AUTH_TOKENS_JSON {
        default: None,
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs"],
        summary: "Inline static bearer-token configuration (wins over the file).",
    },
    MACP_BIND_ADDR {
        default: Some("127.0.0.1:50051"),
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "gRPC listen address.",
    },
    MACP_CHECKPOINT_INTERVAL {
        default: Some("0"),
        component: Runtime,
        read_in: ["src/main.rs", "src/runtime.rs"],
        summary: "Mid-session checkpoint interval in log entries; `0` disables it.",
    },
    MACP_CLEANUP_INTERVAL_SECS {
        default: Some("60"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Background maintenance interval: TTL expiry, eviction, disk GC, due synthetic accepts.",
    },
    MACP_COMPACT_TERMINAL_LOGS {
        default: Some("1"),
        component: Runtime,
        read_in: ["src/runtime.rs"],
        summary: "`0`/`false` keeps an ended session's full log so `WhatIfReplay` can replay it.",
    },
    MACP_CONCURRENCY_LIMIT_PER_CONNECTION {
        default: Some("64"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "gRPC per-connection concurrency limit.",
    },
    MACP_CONFORMANCE_FIXTURES_DIR {
        default: None,
        component: Test,
        read_in: ["tests/conformance_loader.rs"],
        summary: "Points the conformance tests at a fixture directory instead of the vendored `tests/conformance/`.",
    },
    MACP_CONSOLE_PORT {
        default: Some("3000"),
        component: Cli,
        read_in: ["crates/macp-cli/src/stack.rs"],
        summary: "Console port `macp up` prints.",
    },
    MACP_CP_PORT {
        default: Some("3001"),
        component: Cli,
        read_in: ["crates/macp-cli/src/stack.rs"],
        summary: "Control-plane port `macp up` prints.",
    },
    MACP_DATA_DIR {
        default: Some(".macp-data"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Persistence directory for the file backend.",
    },
    MACP_ERROR_REGISTRY_PATH {
        default: None,
        component: Test,
        read_in: ["crates/macp-core/src/error.rs"],
        summary: "The spec's `registries/error-codes.md`, for the error-registry parity test.",
    },
    MACP_HOME {
        default: Some("~/.macp"),
        component: Cli,
        read_in: ["crates/macp-cli/src/stack.rs"],
        summary: "Base directory for the stack files `macp up` writes.",
    },
    MACP_HTTP_BIND_ADDR {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "HTTP/JSON listener address (`http-listener` feature); unset = no listener.",
    },
    MACP_HTTP_CORS_ORIGINS {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs"],
        summary: "Comma-separated CORS origins for the HTTP listener; unset = closed.",
    },
    MACP_HTTP_MAX_BODY_BYTES {
        default: Some("ceil(4/3 * MACP_MAX_PAYLOAD_BYTES) + 65536"),
        component: Runtime,
        read_in: ["src/http/config.rs"],
        summary: "HTTP request body cap; a value below the default is refused.",
    },
    MACP_HTTP_MAX_CONNECTIONS {
        default: Some("1024"),
        component: Runtime,
        read_in: ["src/http/config.rs"],
        summary: "Concurrent HTTP connections admitted.",
    },
    MACP_HTTP_PUBLIC_URL {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs"],
        summary: "Public HTTP base URL; its path is the route prefix, advertised in the manifest.",
    },
    MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE {
        default: Some("100"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs", "src/main.rs"],
        summary: "`ListSessions` page size when the request sends `page_size = 0`.",
    },
    MACP_LIST_SESSIONS_MAX_PAGE_SIZE {
        default: Some("1000"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs", "src/main.rs"],
        summary: "Hard cap a requested `ListSessions` `page_size` is clamped to.",
    },
    MACP_MAX_CONCURRENT_STREAMS {
        default: Some("128"),
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "HTTP/2 max concurrent streams.",
    },
    MACP_MAX_PAYLOAD_BYTES {
        default: Some("1048576"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs", "src/http/config.rs", "src/main.rs"],
        summary: "Maximum envelope payload size in bytes.",
    },
    MACP_MEMORY_ONLY {
        default: None,
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "`1` skips persistence (in-memory backend).",
    },
    MACP_MESSAGE_LIMIT_PER_MINUTE {
        default: Some("600"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs", "src/main.rs"],
        summary: "Per-sender message rate limit.",
    },
    MACP_METRICS_ADDR {
        default: None,
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Prometheus text endpoint bind address; unset = no endpoint.",
    },
    MACP_PARITY_CONTRACT {
        default: None,
        component: Test,
        read_in: ["tests/parity_contract.rs"],
        summary: "Points the parity test at a canonical `contract.json` instead of the vendored copy.",
    },
    MACP_PLAYGROUND_PORT {
        default: Some("3100"),
        component: Cli,
        read_in: ["crates/macp-cli/src/stack.rs"],
        summary: "Playground port `macp up` prints.",
    },
    MACP_POLICIES_DIR {
        default: None,
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Directory of policy JSON files preloaded at startup (makes the wire registry read-only).",
    },
    MACP_POLICIES_DRY_RUN {
        default: None,
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "`1` validates `MACP_POLICIES_DIR` and exits without starting the server.",
    },
    MACP_POLICY_SCHEMAS_DIR {
        default: None,
        component: Test,
        read_in: ["crates/macp-policy/src/registry.rs"],
        summary: "Canonical policy JSON schemas, for the enum-parity test.",
    },
    MACP_RECEIPT_PUBLIC_KEYS_PATH {
        default: None,
        component: Runtime,
        read_in: ["src/receipts.rs"],
        summary: "JWKS of retired public keys still published by `GetReceiptKeys`.",
    },
    MACP_RECEIPT_SIGNING_KEY {
        default: None,
        component: Runtime,
        read_in: ["src/receipts.rs"],
        summary: "Inline Ed25519 decision-receipt signing key (exclusive with the path).",
    },
    MACP_RECEIPT_SIGNING_KEY_PATH {
        default: None,
        component: Runtime,
        read_in: ["src/receipts.rs"],
        summary: "Ed25519 decision-receipt signing key file; unset = unsigned receipts.",
    },
    MACP_RECEIPT_SIGNING_KID {
        default: None,
        component: Runtime,
        read_in: ["src/receipts.rs"],
        summary: "Signing key `kid` (required for a PEM key).",
    },
    MACP_REDIS_URL {
        default: Some("redis://127.0.0.1:6379"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Redis connection URL (`redis-backend` feature).",
    },
    MACP_REGEN_GOLDEN {
        default: None,
        component: Test,
        read_in: ["tests/grpc_refusal_snapshot.rs", "tests/http_json.rs", "tests/kb_facts.rs"],
        summary: "`1` rewrites golden snapshot files instead of comparing against them.",
    },
    MACP_REQUEST_TIMEOUT_SECS {
        default: Some("30"),
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "Per-request timeout in seconds.",
    },
    MACP_ROCKSDB_PATH {
        default: Some("<MACP_DATA_DIR>/rocksdb"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "RocksDB database path (`rocksdb-backend` feature).",
    },
    MACP_RUNTIME_ID {
        default: Some("macp-runtime"),
        component: Runtime,
        read_in: ["src/receipts.rs"],
        summary: "Receipt issuer id (`runtime:<id>`); required when receipts are signed.",
    },
    MACP_RUNTIME_IMAGE {
        default: None,
        component: Test,
        read_in: ["crates/macp-cli/tests/stack.rs"],
        summary: "Runtime image the CLI stack test runs against.",
    },
    MACP_RUNTIME_PORT {
        default: Some("50051"),
        component: Cli,
        read_in: ["crates/macp-cli/src/stack.rs"],
        summary: "Runtime gRPC port `macp up` prints.",
    },
    MACP_SESSION_DISK_RETENTION_SECS {
        default: Some("0"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Delete terminal sessions' durable data after this age; `0` keeps it forever.",
    },
    MACP_SESSION_RETENTION_SECS {
        default: Some("3600"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Evict terminal sessions from memory at this age (durable data is kept).",
    },
    MACP_SESSION_START_LIMIT_PER_MINUTE {
        default: Some("60"),
        component: Runtime,
        read_in: ["crates/macp-auth/src/security.rs", "src/main.rs"],
        summary: "Per-sender session-start rate limit.",
    },
    MACP_SHUTDOWN_DRAIN_SECS {
        default: Some("10"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Graceful-shutdown drain deadline in seconds.",
    },
    MACP_STORAGE_BACKEND {
        default: Some("file"),
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "Storage backend: `file`, `rocksdb` or `redis`.",
    },
    MACP_STRICT_RECOVERY {
        default: None,
        component: Runtime,
        read_in: ["src/main.rs"],
        summary: "`1` fails startup on any recovery error.",
    },
    MACP_TARGET {
        default: Some("grpc://127.0.0.1:50051"),
        component: Cli,
        read_in: ["crates/macp-cli/src/main.rs"],
        summary: "Runtime endpoint the CLI connects to.",
    },
    MACP_TEST_AUTH_TOKEN {
        default: None,
        component: Test,
        read_in: ["integration_tests/src/config.rs"],
        summary: "Bearer token for hosted integration-test runs.",
    },
    MACP_TEST_BACKEND {
        default: None,
        component: Test,
        read_in: ["integration_tests/tests/tier1_protocol/test_backends.rs"],
        summary: "Storage backend the integration tests start the runtime with.",
    },
    MACP_TEST_BINARY {
        default: None,
        component: Test,
        read_in: [
            "integration_tests/src/config.rs",
            "integration_tests/tests/tier1_jwt.rs",
            "integration_tests/tests/tier1_protocol/test_backends.rs",
            "integration_tests/tests/tier1_protocol/test_decision_receipts.rs",
            "integration_tests/tests/tier1_protocol/test_handoff_implicit_accept.rs",
            "integration_tests/tests/tier1_protocol/test_limits.rs",
            "integration_tests/tests/tier1_protocol/test_list_sessions_pagination.rs",
            "integration_tests/tests/tier1_protocol/test_passive_subscribe.rs",
            "integration_tests/tests/tier1_protocol/test_persistence_replay.rs",
            "integration_tests/tests/tier1_protocol/test_promote_mode.rs",
            "integration_tests/tests/tier1_protocol/test_session_lifecycle.rs",
            "integration_tests/tests/tier1_protocol/test_startup_gate.rs",
            "integration_tests/tests/tier1_protocol/test_tls.rs",
            "integration_tests/tests/tier1_protocol/test_what_if_replay.rs",
        ],
        summary: "Runtime binary the integration tests launch.",
    },
    MACP_TEST_CLI {
        default: None,
        component: Test,
        read_in: ["integration_tests/tests/tier1_protocol/test_decision_receipts.rs"],
        summary: "CLI binary the integration tests launch.",
    },
    MACP_TEST_ENDPOINT {
        default: None,
        component: Test,
        read_in: ["integration_tests/src/config.rs"],
        summary: "Hosted runtime endpoint the integration tests target instead of a local server.",
    },
    MACP_TEST_REDIS_URL {
        default: None,
        component: Test,
        read_in: [
            "crates/macp-storage/src/storage/redis_backend.rs",
            "integration_tests/tests/tier1_protocol/test_backends.rs",
        ],
        summary: "Redis URL for the Redis backend tests.",
    },
    MACP_TEST_TLS {
        default: None,
        component: Test,
        read_in: ["integration_tests/src/config.rs"],
        summary: "Run the integration tests over TLS.",
    },
    MACP_TLS_CERT_PATH {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "TLS certificate path (PEM).",
    },
    MACP_TLS_KEY_PATH {
        default: None,
        component: Runtime,
        read_in: ["src/http/config.rs", "src/main.rs"],
        summary: "TLS private key path (PEM).",
    },
    MACP_TOKEN {
        default: None,
        component: Cli,
        read_in: ["crates/macp-cli/src/main.rs"],
        summary: "Bearer token the CLI authenticates with.",
    },
}

/// The registry row for `name`, if registered.
pub fn lookup(name: &str) -> Option<&'static EnvVar> {
    REGISTRY.iter().find(|v| v.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_unique_and_well_formed() {
        for pair in REGISTRY.windows(2) {
            assert!(
                pair[0].name < pair[1].name,
                "REGISTRY must be sorted by name with no duplicates: {} then {}",
                pair[0].name,
                pair[1].name
            );
        }
        for v in REGISTRY {
            assert!(v.name.starts_with("MACP_"), "{}", v.name);
            assert_eq!(v.test_only, v.component == Component::Test, "{}", v.name);
            assert!(
                v.read_in.windows(2).all(|p| p[0] < p[1]),
                "{}: read_in must be sorted and unique",
                v.name
            );
            assert!(
                v.test_only || !v.read_in.is_empty(),
                "{}: a non-test variable must name the files that read it",
                v.name
            );
            assert!(!v.summary.is_empty(), "{}", v.name);
        }
        assert_eq!(lookup(MACP_BIND_ADDR).unwrap().name, "MACP_BIND_ADDR");
        assert!(lookup("MACP_NOT_A_VARIABLE").is_none());
    }
}
