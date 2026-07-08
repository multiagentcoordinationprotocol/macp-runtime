use macp_runtime::log_store::LogStore;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb;
use macp_runtime::policy::registry::PolicyRegistry;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::security::SecurityLayer;
use macp_runtime::server::MacpServer;
use macp_runtime::storage::{
    cleanup_temp_files, migrate_if_needed, FileBackend, MemoryBackend, StorageBackend,
};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tonic::transport::{Identity, Server, ServerTlsConfig};

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    #[cfg(feature = "otel")]
    {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_otlp::WithExportConfig;
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4317".into());
        let exporter = opentelemetry_otlp::new_exporter()
            .tonic()
            .with_endpoint(&otlp_endpoint);
        let provider = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(exporter)
            .install_batch(opentelemetry_sdk::runtime::Tokio)
            .expect("failed to init OTEL tracer");
        let tracer = provider.tracer("macp-runtime");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer())
            .with(otel_layer)
            .init();
        tracing::info!(
            "OpenTelemetry tracing enabled (endpoint: {})",
            otlp_endpoint
        );
    }

    #[cfg(not(feature = "otel"))]
    {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    // Validate environment configuration early
    let config_errors = validate_env_config();
    if !config_errors.is_empty() {
        for err in &config_errors {
            tracing::error!("Configuration error: {err}");
        }
        return Err(format!(
            "startup aborted: {} configuration error(s) detected",
            config_errors.len()
        )
        .into());
    }

    let addr = std::env::var("MACP_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50051".into())
        .parse()?;

    let memory_only = std::env::var("MACP_MEMORY_ONLY").ok().as_deref() == Some("1");
    let data_dir =
        PathBuf::from(std::env::var("MACP_DATA_DIR").unwrap_or_else(|_| ".macp-data".into()));
    let strict_recovery = std::env::var("MACP_STRICT_RECOVERY").ok().as_deref() == Some("1");

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
                Arc::new(macp_runtime::storage::RedisBackend::connect(&url, "macp").await?)
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

    let mut recovery_replay_mismatches: u64 = 0;
    if !memory_only {
        // Replay sessions from logs
        let session_ids = storage.list_session_ids().await?;
        let mut recovered = 0usize;
        for session_id in session_ids {
            let log_entries = match storage.load_log(&session_id).await {
                Ok(entries) => entries,
                Err(e) if strict_recovery => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("strict recovery: failed to load log for {session_id}: {e}"),
                    )
                    .into());
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "failed to load session log; skipping"
                    );
                    continue;
                }
            };
            if log_entries.is_empty() {
                continue;
            }

            match replay_session(
                &session_id,
                &log_entries,
                &mode_registry,
                Some(&policy_registry),
            ) {
                Ok(session) => {
                    // D7: warn-only divergence check against the stored
                    // snapshot BEFORE we overwrite it with the replayed state.
                    if let Ok(Some(snapshot)) = storage.load_session(&session_id).await {
                        recovery_replay_mismatches +=
                            macp_runtime::replay::validate_replay_consistency(
                                &session_id,
                                &session,
                                &snapshot,
                            ) as u64;
                    }
                    if let Err(e) = storage.save_session(&session).await {
                        if strict_recovery {
                            return Err(io::Error::other(format!(
                                "strict recovery: failed to persist recovered session {session_id}: {e}"
                            ))
                            .into());
                        }
                        tracing::warn!(
                            session_id = %session_id,
                            error = %e,
                            "failed to persist recovered session"
                        );
                    }

                    log_store.create_session_log(&session_id).await;
                    for log_entry in &log_entries {
                        log_store.append(&session_id, log_entry.clone()).await;
                    }

                    registry.insert_recovered_session(session_id, session).await;
                    recovered += 1;
                }
                Err(e) if strict_recovery => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("strict recovery: failed to replay session {session_id}: {e}"),
                    )
                    .into());
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session_id,
                        error = %e,
                        "failed to replay session; skipping"
                    );
                }
            }
        }
        if recovered > 0 {
            tracing::info!(
                count = recovered,
                strict_recovery,
                "replayed sessions from log"
            );
        }
    }

    let runtime = Arc::new(Runtime::with_registries(
        Arc::clone(&storage),
        Arc::clone(&registry),
        Arc::clone(&log_store),
        mode_registry,
        policy_registry,
    ));
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

    // Graceful shutdown: on ctrl-c stop accepting and drain in-flight
    // requests; long-lived watch streams would hold the drain open forever,
    // so a hard deadline forces exit afterwards.
    let drain_secs: u64 = std::env::var("MACP_SHUTDOWN_DRAIN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received; draining in-flight requests");
    };
    let server_future = builder
        .add_service(health_service)
        .add_service(
            pb::macp_runtime_service_server::MacpRuntimeServiceServer::new(svc)
                .max_decoding_message_size(max_payload_bytes + ENVELOPE_OVERHEAD_BYTES),
        )
        .serve_with_shutdown(addr, shutdown);

    // Background cleanup task: expire TTL-exceeded sessions and evict stale ones.
    let cleanup_interval_secs: u64 = std::env::var("MACP_CLEANUP_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let session_retention_secs: u64 = std::env::var("MACP_SESSION_RETENTION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    // Opt-in durable-data retention: 0 (default) keeps history forever.
    let disk_retention_secs: u64 = std::env::var("MACP_SESSION_DISK_RETENTION_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let cleanup_runtime = runtime.clone();
    let cleanup_handle = tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(cleanup_interval_secs));
        loop {
            interval.tick().await;
            cleanup_runtime.cleanup_expired_sessions().await;
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
        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&metrics_addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!(%metrics_addr, error = %e, "metrics endpoint failed to bind");
                    return;
                }
            };
            tracing::info!(%metrics_addr, "metrics endpoint listening (GET /metrics)");
            loop {
                let Ok((mut sock, _peer)) = listener.accept().await else {
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
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
    }

    // serve_with_shutdown resolves once the signal fired AND connections
    // drained; the timer arm is the hard deadline for connections that never
    // close (watch streams).
    let deadline = async {
        let _ = tokio::signal::ctrl_c().await;
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

    Ok(())
}
