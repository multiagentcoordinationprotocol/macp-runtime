use macp_runtime::log_store::LogStore;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb;
use macp_runtime::policy::registry::PolicyRegistry;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::storage::{
    cleanup_temp_files, migrate_if_needed, FileBackend, MemoryBackend, StorageBackend,
};
use macp_runtime::telemetry::Telemetry;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tonic::transport::{Identity, Server, ServerTlsConfig};

mod shutdown;
use shutdown::ShutdownSignal;

/// Validate environment variables at startup. Returns a list of errors, if any.
fn validate_env_config() -> Vec<String> {
    let mut errors = Vec::new();

    // Validate MACP_BIND_ADDR is a valid socket address
    if let Ok(val) = std::env::var("MACP_BIND_ADDR") {
        if val.parse::<SocketAddr>().is_err() {
            errors.push(format!(
                "MACP_BIND_ADDR: '{val}' is not a valid socket address (expected host:port, e.g. 127.0.0.1:50051)"
            ));
        }
    }

    // Validate TLS cert/key paths exist if specified
    if let Ok(cert_path) = std::env::var("MACP_TLS_CERT_PATH") {
        if !std::path::Path::new(&cert_path).exists() {
            errors.push(format!(
                "MACP_TLS_CERT_PATH: file does not exist: {cert_path}"
            ));
        }
    }
    if let Ok(key_path) = std::env::var("MACP_TLS_KEY_PATH") {
        if !std::path::Path::new(&key_path).exists() {
            errors.push(format!(
                "MACP_TLS_KEY_PATH: file does not exist: {key_path}"
            ));
        }
    }

    // Validate positive integer environment variables
    for var_name in [
        "MACP_MAX_PAYLOAD_BYTES",
        "MACP_SESSION_START_LIMIT_PER_MINUTE",
        "MACP_MESSAGE_LIMIT_PER_MINUTE",
        "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE",
        "MACP_LIST_SESSIONS_MAX_PAGE_SIZE",
    ] {
        if let Ok(val) = std::env::var(var_name) {
            match val.parse::<u64>() {
                Ok(0) => {
                    errors.push(format!("{var_name}: must be a positive integer, got '0'"));
                }
                Ok(_) => {}
                Err(_) => {
                    errors.push(format!(
                        "{var_name}: '{val}' is not a valid positive integer"
                    ));
                }
            }
        }
    }

    // Cross-field: a default ListSessions page size above the max in force is
    // silently clamped down by SecurityLayer, which would discard the
    // operator's stated intent without ever telling them. Refuse to start
    // instead, naming both values.
    //
    // The comparison is against the EFFECTIVE max — the explicitly configured
    // one when set, otherwise the built-in cap. Checking only the both-set
    // case would leave the identical operator error (DEFAULT=5000 with the max
    // unset) on a silent path: clamped to the built-in 1000 with nothing but a
    // `tracing::warn!` that vanishes under `RUST_LOG=off`. The error message
    // says which of the two the max came from, so `1000` appearing in it is
    // never mistaken for a value the operator set.
    if let Ok(default_raw) = std::env::var("MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE") {
        if let Ok(default_size) = default_raw.parse::<u64>() {
            let max_raw = std::env::var("MACP_LIST_SESSIONS_MAX_PAGE_SIZE").ok();
            // "Unset" and "set but unparseable" both fall back to the built-in
            // cap, but they are not the same operator mistake — and the
            // unparseable one already gets its own error from the loop above,
            // so claiming the var is unset here contradicts it.
            let (max_size, max_source) = match max_raw
                .as_ref()
                .and_then(|raw| raw.parse::<u64>().ok())
                .filter(|&v| v > 0)
            {
                Some(max_size) => (
                    max_size,
                    "MACP_LIST_SESSIONS_MAX_PAGE_SIZE, explicitly configured".to_string(),
                ),
                None => (
                    macp_runtime::security::MAX_LIST_SESSIONS_PAGE_SIZE as u64,
                    match max_raw {
                        Some(raw) => format!(
                            "MACP_LIST_SESSIONS_MAX_PAGE_SIZE is set to '{raw}', which is not a valid positive integer, so the built-in default applies"
                        ),
                        None => "MACP_LIST_SESSIONS_MAX_PAGE_SIZE unset, so the built-in default applies".to_string(),
                    },
                ),
            };
            if default_size > max_size {
                errors.push(format!(
                    "MACP_LIST_SESSIONS_DEFAULT_PAGE_SIZE ({default_size}) must not exceed the effective maximum page size ({max_size}) [{max_source}]; raise MACP_LIST_SESSIONS_MAX_PAGE_SIZE or lower the default"
                ));
            }
        }
    }

    // Decision-receipt keys: a bad key is fatal, never a silently unsigned runtime.
    if let Err(e) = macp_runtime::receipts::ReceiptConfig::from_env() {
        errors.push(e);
    }

    // The HTTP/JSON listener (RFC-MACP-0006 §4): validated only when
    // MACP_HTTP_BIND_ADDR is set (non-empty); unset, nothing is read.
    #[cfg(feature = "http-listener")]
    if let Err(http_errors) =
        macp_runtime::http::HttpConfig::from_env(&|k: &str| std::env::var(k).ok())
    {
        errors.extend(http_errors);
    }
    // Never a silent no-op: a binary without the listener refuses a
    // configuration that asks for one.
    #[cfg(not(feature = "http-listener"))]
    if std::env::var_os("MACP_HTTP_BIND_ADDR").is_some_and(|v| !v.is_empty()) {
        errors.push(
            "MACP_HTTP_BIND_ADDR is set, but this macp-runtime binary was built without the \
             `http-listener` Cargo feature; rebuild with `--features http-listener` (or use an \
             image built with it), or unset MACP_HTTP_BIND_ADDR"
                .into(),
        );
    }

    // Validate non-negative integer environment variable
    if let Ok(val) = std::env::var("MACP_CHECKPOINT_INTERVAL") {
        if val.parse::<u64>().is_err() {
            errors.push(format!(
                "MACP_CHECKPOINT_INTERVAL: '{val}' is not a valid non-negative integer"
            ));
        }
    }

    errors
}

/// `MACP_POLICIES_DRY_RUN=1`: validate every file in `MACP_POLICIES_DIR`,
/// report each one by name, and return the process exit code — `0` if the
/// directory would load, `1` if anything in it would be rejected.
///
/// This exists because `load_from_dir` is fatal at startup and stops at the
/// first rejection: without a validate-only pass, an operator upgrading into
/// new registration constraints finds out one file at a time, in production.
/// Output goes to stdout/stderr directly rather than through `tracing`, so no
/// `RUST_LOG` filter can suppress the report.
fn policies_dry_run(policies_dir: Option<String>) -> i32 {
    let Some(dir) = policies_dir else {
        eprintln!("MACP_POLICIES_DRY_RUN=1 requires MACP_POLICIES_DIR to be set");
        return 1;
    };
    let path = std::path::Path::new(&dir);
    let outcomes = match PolicyRegistry::validate_dir(path) {
        Ok(outcomes) => outcomes,
        Err(e) => {
            eprintln!("MACP_POLICIES_DIR: {e}");
            return 1;
        }
    };

    let mut rejected = 0usize;
    for outcome in &outcomes {
        match &outcome.result {
            Ok(policy_id) => println!("OK       {} ({policy_id})", outcome.path.display()),
            Err(reason) => {
                rejected += 1;
                println!("REJECTED {}: {reason}", outcome.path.display());
            }
        }
    }
    println!(
        "{} policy file(s) checked in {}, {} rejected",
        outcomes.len(),
        path.display(),
        rejected
    );
    // A readable directory holding no `*.json` is the shape of a mis-pointed
    // MACP_POLICIES_DIR — exactly what dry-run exists to catch — and a bare
    // `0` in the summary above is easy to skim past. It is not an error:
    // `load_from_dir` starts happily on an empty directory, so dry-run must
    // agree and still exit 0.
    if outcomes.is_empty() {
        println!(
            "WARNING: no *.json files found in {} — startup would load no policies",
            path.display()
        );
    }
    if rejected > 0 {
        1
    } else {
        0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // `otel` builds: telemetry is active only with an OTLP endpoint variable
    // set (see `macp_runtime::telemetry`). Inactive, `init` installs exactly
    // the feature-off subscriber below. A misconfiguration is logged and
    // leaves telemetry off; it never aborts startup.
    #[cfg(feature = "otel")]
    let telemetry_guard = match macp_runtime::telemetry::init::init(env_filter) {
        Ok(guard) => guard,
        Err(e) => {
            tracing::error!("{e}");
            None
        }
    };

    #[cfg(not(feature = "otel"))]
    {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }
    #[cfg(not(feature = "otel"))]
    macp_runtime::telemetry::warn_if_configured_without_feature(&|k: &str| std::env::var(k).ok());

    // Validate environment configuration early
    let config_errors = validate_env_config();
    if !config_errors.is_empty() {
        for err in &config_errors {
            tracing::error!("Configuration error: {err}");
        }
        // Fold the details into the returned error too: `tracing`'s default
        // `fmt` writer is stdout and any filter can suppress it, so the abort
        // message on stderr must name the offending variables by itself.
        return Err(format!(
            "startup aborted: {} configuration error(s): {}",
            config_errors.len(),
            config_errors.join("; ")
        )
        .into());
    }

    // MACP_POLICIES_DRY_RUN=1: validate the policies directory and exit
    // without binding a port, opening storage, or replaying sessions.
    if std::env::var("MACP_POLICIES_DRY_RUN").ok().as_deref() == Some("1") {
        std::process::exit(policies_dry_run(std::env::var("MACP_POLICIES_DIR").ok()));
    }

    let addr = std::env::var("MACP_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50051".into())
        .parse()?;

    let memory_only = std::env::var("MACP_MEMORY_ONLY").ok().as_deref() == Some("1");
    let data_dir =
        PathBuf::from(std::env::var("MACP_DATA_DIR").unwrap_or_else(|_| ".macp-data".into()));
    let strict_recovery = std::env::var("MACP_STRICT_RECOVERY").ok().as_deref() == Some("1");

    // Opt-in durable-data retention: 0 (default) keeps history forever.
    // Read before the backend is built: Redis expires trace-anchor keys
    // only when sessions are garbage-collected.
    let disk_retention_secs: u64 = std::env::var("MACP_SESSION_DISK_RETENTION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let backend_name = std::env::var("MACP_STORAGE_BACKEND").unwrap_or_else(|_| "file".into());
    let storage: Arc<dyn StorageBackend> = if memory_only {
        Arc::new(MemoryBackend)
    } else {
        match backend_name.as_str() {
            "file" => {
                std::fs::create_dir_all(&data_dir)?;
                migrate_if_needed(&data_dir)?;
                cleanup_temp_files(&data_dir);
                Arc::new(FileBackend::new(data_dir.clone())?)
            }
            #[cfg(feature = "rocksdb-backend")]
            "rocksdb" => {
                let path = std::env::var("MACP_ROCKSDB_PATH")
                    .unwrap_or_else(|_| data_dir.join("rocksdb").to_string_lossy().to_string());
                Arc::new(macp_runtime::storage::RocksDbBackend::open(&path)?)
            }
            #[cfg(feature = "redis-backend")]
            "redis" => {
                let url = std::env::var("MACP_REDIS_URL")
                    .unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
                Arc::new(
                    macp_runtime::storage::RedisBackend::connect(&url, "macp")
                        .await?
                        .with_disk_retention_secs(disk_retention_secs),
                )
            }
            other => {
                return Err(format!(
                    "unknown storage backend: {other}. Valid: file, rocksdb, redis"
                )
                .into());
            }
        }
    };

    // Load persisted state into in-memory caches
    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let mode_registry = Arc::new(ModeRegistry::build_default(Arc::new(
        macp_policy::DefaultPolicyEvaluator,
    )));
    let policy_registry = Arc::new(PolicyRegistry::new());
    // RFC-MACP-0012 §9: preload governance policies from disk BEFORE session
    // recovery — replayed sessions resolve their bound policy_version against
    // the live registry, so a session governed by a file-loaded policy must
    // find it. A configured-but-broken policies dir is fatal: a runtime told
    // to preload governance must not silently start without it.
    let policies_dir = std::env::var("MACP_POLICIES_DIR").ok();
    if let Some(dir) = &policies_dir {
        let count = policy_registry
            .load_from_dir(std::path::Path::new(dir))
            .map_err(|e| format!("MACP_POLICIES_DIR: {e}"))?;
        tracing::info!(count, dir = %dir, "loaded governance policies from directory");
    }

    // Validated in validate_env_config; read again for the runtime.
    let receipt_config = macp_runtime::receipts::ReceiptConfig::from_env()?;
    if receipt_config.signed() {
        tracing::info!(
            kid = receipt_config.kid().unwrap_or_default(),
            issuer = receipt_config.issuer(),
            "decision receipts are signed"
        );
        if !storage.receipts_durable() {
            tracing::warn!(
                "the storage backend cannot persist decision receipts: a restart may \
                 serve a different signed receipt for the same session"
            );
        }
    }

    let mut recovery_replay_mismatches: u64 = 0;
    if !memory_only {
        let report = macp_runtime::recovery::recover_sessions(
            storage.as_ref(),
            &registry,
            &log_store,
            &mode_registry,
            &policy_registry,
            strict_recovery,
        )
        .await?;
        recovery_replay_mismatches = report.replay_mismatches;
    }

    // Trace anchors orphaned by a binary that predates them (its disk GC
    // deleted sessions but not their anchors): swept once, after recovery
    // and off the serving path. Only when sessions are ever GC'd, since
    // nothing is orphaned otherwise. Backends that cannot orphan one do
    // nothing.
    if !memory_only && disk_retention_secs > 0 {
        let sweep_storage = Arc::clone(&storage);
        tokio::spawn(async move {
            match sweep_storage.sweep_orphan_trace_anchors().await {
                Ok(0) => {}
                Ok(count) => tracing::info!(count, "removed orphaned trace anchors"),
                Err(e) => tracing::warn!(error = %e, "trace-anchor orphan sweep failed"),
            }
        });
    }

    #[cfg(feature = "otel")]
    let telemetry = telemetry_guard
        .as_ref()
        .map(macp_runtime::telemetry::init::Guard::telemetry)
        .unwrap_or_else(Telemetry::disabled);
    #[cfg(not(feature = "otel"))]
    let telemetry = Telemetry::disabled();
    // The gRPC SERVER span layer, only when telemetry is active; inactive it
    // is the identity layer and the server behaves exactly as without `otel`.
    #[cfg(feature = "otel")]
    let grpc_server_layer = macp_runtime::telemetry::grpc::optional_server_layer(&telemetry);
    let runtime = Arc::new(
        Runtime::with_registries(
            Arc::clone(&storage),
            Arc::clone(&registry),
            Arc::clone(&log_store),
            mode_registry,
            policy_registry,
        )
        .with_receipt_config(receipt_config)
        .with_telemetry(telemetry),
    );
    if recovery_replay_mismatches > 0 {
        tracing::warn!(
            mismatches = recovery_replay_mismatches,
            "replay/snapshot divergences detected during recovery (log is authoritative)"
        );
        runtime
            .metrics()
            .record_replay_mismatch(recovery_replay_mismatches);
    }
    let security = SecurityLayer::from_env()?;

    let allow_insecure = std::env::var("MACP_ALLOW_INSECURE").ok().as_deref() == Some("1");
    // Without configured auth, every bearer token authenticates as a
    // fully-privileged identity (dev fallback). That must be an explicit
    // local-development choice, never a silent default: refuse to start
    // unless the operator opted in via the same flag that gates plaintext.
    if !security.has_configured_auth() {
        if allow_insecure {
            tracing::warn!(
                "no authentication configured (MACP_AUTH_TOKENS_*/MACP_AUTH_ISSUER unset): \
                 running in dev-mode auth where ANY bearer token is a fully-privileged \
                 identity; permitted only because MACP_ALLOW_INSECURE=1"
            );
        } else {
            return Err(
                "no authentication configured: set MACP_AUTH_TOKENS_FILE/MACP_AUTH_TOKENS_JSON \
                 or MACP_AUTH_ISSUER (+JWKS), or explicitly opt into dev-mode auth with \
                 MACP_ALLOW_INSECURE=1 (local development only)"
                    .into(),
            );
        }
    }
    let mut svc = MacpServer::new(Arc::clone(&runtime), security);
    if policies_dir.is_some() {
        // File-loaded profile: registry is read-only over the wire
        // (register_policy=false, list_policies=true — RFC-0012 §9).
        svc = svc.with_read_only_policies();
    }
    let tls_cert = std::env::var("MACP_TLS_CERT_PATH").ok();
    let tls_key = std::env::var("MACP_TLS_KEY_PATH").ok();

    tracing::info!(%addr, "macp-runtime v{} listening", env!("CARGO_PKG_VERSION"));

    // Server resource limits (RFC-MACP-0004 §7 DoS defenses). Combined with
    // per-session locking these bound how much work one client can queue.
    let concurrency_per_conn: usize = std::env::var("MACP_CONCURRENCY_LIMIT_PER_CONNECTION")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let max_streams: u32 = std::env::var("MACP_MAX_CONCURRENT_STREAMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128);
    let request_timeout_secs: u64 = std::env::var("MACP_REQUEST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);

    let builder = Server::builder()
        .concurrency_limit_per_connection(concurrency_per_conn)
        .max_concurrent_streams(Some(max_streams))
        .timeout(std::time::Duration::from_secs(request_timeout_secs))
        .tcp_keepalive(Some(std::time::Duration::from_secs(60)))
        .http2_keepalive_interval(Some(std::time::Duration::from_secs(30)));
    // Outside `add_service`, like tonic's own layers: message-size, timeout
    // and concurrency semantics are unchanged.
    #[cfg(feature = "otel")]
    let builder = builder.layer(grpc_server_layer);
    let mut builder = match (tls_cert, tls_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert = tokio::fs::read(cert_path).await?;
            let key = tokio::fs::read(key_path).await?;
            builder.tls_config(ServerTlsConfig::new().identity(Identity::from_pem(cert, key)))?
        }
        _ if allow_insecure => {
            tracing::warn!(
                "starting without TLS because MACP_ALLOW_INSECURE=1; this is not RFC-compliant"
            );
            builder
        }
        _ => return Err("TLS is required unless MACP_ALLOW_INSECURE=1 is set".into()),
    };

    // Set up gRPC health check service
    let (health_reporter, health_service) = tonic_health::server::health_reporter();
    health_reporter
        .set_serving::<pb::macp_runtime_service_server::MacpRuntimeServiceServer<MacpServer>>()
        .await;

    // gRPC server reflection (dev/debug tooling only -- grpcurl, grpcui).
    // Opt-in via the `reflection` Cargo feature; never on in the published
    // Docker image or a default `cargo build`.
    #[cfg(feature = "reflection")]
    let reflection_service = {
        tracing::warn!(
            "gRPC server reflection is enabled (reflection feature); this exposes the full \
             service/message schema to any client and should be used for local dev/debug only"
        );
        tonic_reflection::server::Builder::configure()
            .register_encoded_file_descriptor_set(macp_runtime::FILE_DESCRIPTOR_SET)
            .build_v1()?
    };

    // Make the transport-level message bound track the configured payload
    // bound: previously tonic's 4 MB default applied BEFORE the payload-size
    // check, so MACP_MAX_PAYLOAD_BYTES above 4 MB was silently ineffective
    // and far-larger frames were decoded before rejection. Envelope overhead
    // (ids, mode, sender, metadata) gets a fixed allowance on top.
    let max_payload_bytes: usize = std::env::var("MACP_MAX_PAYLOAD_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_048_576);
    const ENVELOPE_OVERHEAD_BYTES: usize = 64 * 1024;

    // Graceful shutdown: on SIGINT or SIGTERM stop accepting and drain
    // in-flight requests; long-lived watch streams would hold the drain open
    // forever, so a hard deadline forces exit afterwards. One shared signal
    // feeds every listener (gRPC, drain deadline, metrics); a second signal
    // during the drain exits at once (`src/shutdown.rs`).
    let drain_secs: u64 = std::env::var("MACP_SHUTDOWN_DRAIN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let shutdown_signal = ShutdownSignal::install();
    let shutdown = shutdown_signal.clone().wait();
    // The HTTP listener (`http-listener` feature), bound now so a bind
    // failure is fatal before anything serves. Unset MACP_HTTP_BIND_ADDR:
    // no socket, no task, no log line.
    #[cfg(feature = "http-listener")]
    let http_listener =
        match macp_runtime::http::HttpConfig::from_env(&|k: &str| std::env::var(k).ok()) {
            Ok(Some(config)) => {
                // RFC-MACP-0006 §4.11: advertised by both bindings' GetManifest
                // and the well-known document, so set before either clone.
                if let Some(base) = &config.public_url {
                    svc = svc.with_http_endpoint(base.clone());
                }
                Some(macp_runtime::http::HttpListener::bind(&config, svc.clone()).await?)
            }
            Ok(None) => None,
            // Already reported by validate_env_config; unreachable in practice.
            Err(errors) => return Err(errors.join("; ").into()),
        };
    let builder = builder.add_service(health_service).add_service(
        pb::macp_runtime_service_server::MacpRuntimeServiceServer::new(svc)
            .max_decoding_message_size(max_payload_bytes + ENVELOPE_OVERHEAD_BYTES),
    );
    #[cfg(feature = "reflection")]
    let builder = builder.add_service(reflection_service);
    let server_future = builder.serve_with_shutdown(addr, shutdown);
    // Both listeners drain together: the combined future resolves once gRPC
    // and HTTP have each closed every connection. Inline, not spawned, so a
    // disabled listener adds no task.
    #[cfg(feature = "http-listener")]
    let server_future = {
        let http_shutdown = shutdown_signal.clone().wait();
        async move {
            let http = async move {
                if let Some(listener) = http_listener {
                    listener.serve(http_shutdown).await;
                }
            };
            tokio::pin!(server_future, http);
            // A gRPC failure (e.g. its bind) must surface at once, not wait
            // behind an HTTP listener that only stops on shutdown.
            tokio::select! {
                grpc = &mut server_future => {
                    grpc?;
                    http.await;
                    Ok(())
                }
                () = &mut http => server_future.await,
            }
        }
    };

    // Background maintenance task: expire TTL-exceeded sessions, emit synthetic
    // envelopes whose deadline has passed, and evict stale ones.
    let cleanup_interval_secs: u64 = std::env::var("MACP_CLEANUP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let session_retention_secs: u64 = std::env::var("MACP_SESSION_RETENTION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let cleanup_runtime = runtime.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(cleanup_interval_secs));
        loop {
            interval.tick().await;
            cleanup_runtime.cleanup_expired_sessions().await;
            // RFC-MACP-0010 §5.1(2)'s eager SHOULD: settle deadlines that have
            // passed without waiting for the next session-scoped message.
            // Deliberately AFTER the TTL sweep — a session whose TTL and
            // implicit-accept deadline both lapsed unobserved expires rather
            // than accepting, which is the precedence the lazy path already
            // gives (`Precheck::Expired` returns before synthesis).
            // `MACP_CLEANUP_INTERVAL_SECS` is therefore the observation-latency
            // bound; the recorded timestamp is the computed deadline either way.
            cleanup_runtime.sweep_due_synthetic_accepts().await;
            cleanup_runtime
                .evict_stale_sessions(session_retention_secs)
                .await;
            if disk_retention_secs > 0 {
                cleanup_runtime.gc_disk_sessions(disk_retention_secs).await;
            }
        }
    });

    // Prometheus metrics endpoint (opt-in via MACP_METRICS_ADDR, e.g.
    // "127.0.0.1:9464"). Deliberately dependency-free: a minimal HTTP/1.1
    // text-format responder — every counter the runtime collects was
    // previously write-only with no export surface at all.
    if let Ok(metrics_addr) = std::env::var("MACP_METRICS_ADDR") {
        let metrics_runtime = runtime.clone();
        let metrics_shutdown = shutdown_signal.clone();
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&metrics_addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(%metrics_addr, error = %e, "metrics endpoint failed to bind");
                    return;
                }
            };
            // The bound address: the configured one, unless its port is 0.
            let bound = listener
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| metrics_addr.clone());
            tracing::info!(metrics_addr = %bound, "metrics endpoint listening (GET /metrics)");
            let stop = metrics_shutdown.wait();
            tokio::pin!(stop);
            loop {
                // On shutdown stop accepting and drop the listener, closing
                // the port; responses already being written finish on their
                // own tasks.
                let accepted = tokio::select! {
                    _ = &mut stop => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((mut sock, _peer)) = accepted else {
                    continue;
                };
                let rt = metrics_runtime.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 1024];
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        sock.read(&mut buf),
                    )
                    .await;
                    let mut body = String::new();
                    for (mode, snap) in rt.metrics().snapshot() {
                        snap.prometheus_lines(&mode, &mut body);
                    }
                    body.push_str(&format!(
                        "macp_replay_mismatches_total {}\n",
                        rt.metrics().replay_mismatches()
                    ));
                    rt.metrics().receipt_prometheus_lines(&mut body);
                    // The semconv Envelope families (macp-semconv/0.1), after
                    // every existing line, which stay byte-identical.
                    rt.metrics().semconv_prometheus_lines(&mut body);
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
            drop(listener);
            tracing::info!(%metrics_addr, "metrics endpoint closed");
        });
    }

    // serve_with_shutdown resolves once the signal fired AND connections
    // drained; the timer arm is the hard deadline for connections that never
    // close (watch streams).
    let deadline = async {
        shutdown_signal.wait().await;
        tokio::time::sleep(std::time::Duration::from_secs(drain_secs)).await;
    };
    tokio::select! {
        result = server_future => {
            result?;
            tracing::info!("all connections drained");
        }
        _ = deadline => {
            tracing::warn!(
                drain_secs,
                "drain deadline reached with connections still open; forcing shutdown"
            );
        }
    }
    cleanup_handle.abort();

    // Final snapshot: persist all sessions to storage
    if !memory_only {
        for session in registry.get_all_sessions().await {
            if let Err(e) = storage.save_session(&session).await {
                tracing::warn!(
                    session_id = %session.session_id,
                    error = %e,
                    "failed to persist session on shutdown"
                );
            }
        }
    }
    tracing::info!("state persisted, goodbye");

    // Flush and shut down the OTLP providers before exiting so buffered
    // spans are exported; bounded, so a dead collector cannot hold the exit.
    #[cfg(feature = "otel")]
    if let Some(guard) = telemetry_guard {
        guard.shutdown().await;
    }

    Ok(())
}
