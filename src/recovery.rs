//! Startup recovery: rebuild every stored session from its append-only log
//! (RFC-MACP-0003), the loop `main` runs before serving. Recovery replays and
//! re-saves sessions; it never issues decision receipts or touches any other
//! runtime-originated state.

use std::io;

use crate::log_store::LogStore;
use crate::mode_registry::ModeRegistry;
use crate::policy::registry::PolicyRegistry;
use crate::registry::SessionRegistry;
use crate::replay::{replay_session, validate_replay_consistency};
use crate::storage::StorageBackend;

/// What [`recover_sessions`] did.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Sessions replayed into the registry.
    pub recovered: usize,
    /// Replay/snapshot divergences found (the log is authoritative).
    pub replay_mismatches: u64,
}

/// Replay every session in `storage` into `registry` and `log_store`.
/// With `strict`, any per-session failure aborts recovery; otherwise it is
/// logged and the session skipped.
pub async fn recover_sessions(
    storage: &dyn StorageBackend,
    registry: &SessionRegistry,
    log_store: &LogStore,
    mode_registry: &ModeRegistry,
    policy_registry: &PolicyRegistry,
    strict: bool,
) -> io::Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    let session_ids = storage.list_session_ids().await?;
    for session_id in session_ids {
        let log_entries = match storage.load_log(&session_id).await {
            Ok(entries) => entries,
            Err(e) if strict => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("strict recovery: failed to load log for {session_id}: {e}"),
                ));
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
            mode_registry,
            Some(policy_registry),
        ) {
            Ok(session) => {
                // D7: warn-only divergence check against the stored
                // snapshot BEFORE we overwrite it with the replayed state.
                if let Ok(Some(snapshot)) = storage.load_session(&session_id).await {
                    report.replay_mismatches +=
                        validate_replay_consistency(&session_id, &session, &snapshot) as u64;
                }
                if let Err(e) = storage.save_session(&session).await {
                    if strict {
                        return Err(io::Error::other(format!(
                            "strict recovery: failed to persist recovered session {session_id}: {e}"
                        )));
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
                report.recovered += 1;
            }
            Err(e) if strict => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("strict recovery: failed to replay session {session_id}: {e}"),
                ));
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
    if report.recovered > 0 {
        tracing::info!(
            count = report.recovered,
            strict_recovery = strict,
            "replayed sessions from log"
        );
    }
    Ok(report)
}
