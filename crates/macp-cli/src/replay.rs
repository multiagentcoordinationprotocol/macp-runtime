//! `macp replay`: replay a recorded session, alone or under a candidate policy
//! (RFC-MACP-0012 §8.1 what-if replay).
//!
//! The history comes from one of three places:
//!
//! - a runtime's file-backend data directory (`--data-dir`): the session's
//!   `log.jsonl`, plus its `session.json` when present;
//! - a log file (`--file`): the same JSON-lines `LogEntry` format, or a JSON
//!   array of entries;
//! - a running runtime (`--session`), through the `WhatIfReplay` RPC.
//!
//! Locally this runs [`macp_runtime::runtime::what_if_over_log`], the code the
//! RPC runs, so all three give the same `WhatIfReplayResponse`.
//!
//! Without a candidate, the recorded policy is replayed against itself, which
//! checks only that the history still replays (and, with a stored session,
//! that it ends where the session did).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use macp_conform::client;
use macp_pb::pb::{
    what_if_entry_outcome::Kind, GetPolicyRequest, GetSessionRequest, PolicyDescriptor,
    WhatIfEntryOutcome, WhatIfOutcome, WhatIfReplayRequest, WhatIfReplayResponse,
};
use macp_runtime::log_store::LogEntry;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::policy::registry::PolicyRegistry;
use macp_runtime::policy::PolicyDefinition;
use macp_runtime::registry::PersistedSession;
use macp_runtime::runtime::{recorded_policy, what_if_over_log, WhatIfError};
use macp_runtime::session::Session;
use tonic::metadata::{Ascii, MetadataValue};

use crate::tail::state_name;

/// Where the recorded history comes from.
#[derive(Debug, Clone)]
pub enum Source {
    DataDir {
        dir: PathBuf,
        session_id: String,
    },
    File(PathBuf),
    Session {
        target: String,
        token: String,
        session_id: String,
    },
}

/// Why no report was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    /// The recorded history does not replay, or does not end where the stored
    /// session did.
    Integrity(String),
    /// Nothing was replayed: unreadable input, unknown session, refused
    /// candidate, compacted history, unreachable runtime.
    Unavailable(String),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integrity(m) => write!(f, "integrity failure: {m}"),
            Self::Unavailable(m) => f.write_str(m),
        }
    }
}

/// Read a candidate policy file: a `PolicyDefinition`, as in
/// `MACP_POLICIES_DIR`.
pub fn read_policy(path: &Path) -> Result<PolicyDefinition, ReplayError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ReplayError::Unavailable(format!("{}: {e}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| {
        ReplayError::Unavailable(format!("{}: not a policy definition: {e}", path.display()))
    })
}

/// Replay `source` under `candidate`, or under its own recorded policy when
/// `candidate` is `None`.
pub async fn run(
    source: &Source,
    candidate: Option<&PolicyDefinition>,
) -> Result<WhatIfReplayResponse, ReplayError> {
    match source {
        Source::DataDir { dir, session_id } => {
            let session_dir = dir.join("sessions").join(session_id);
            let log_path = session_dir.join("log.jsonl");
            if !log_path.exists() {
                return Err(ReplayError::Unavailable(format!(
                    "no session '{session_id}' in {} (expected {})",
                    dir.display(),
                    log_path.display()
                )));
            }
            let log = read_log(&log_path)?;
            let stored = read_stored(&session_dir.join("session.json"))?;
            replay_local(session_id, &log, stored.as_ref(), candidate)
        }
        Source::File(path) => {
            let log = read_log(path)?;
            let session_id = log
                .iter()
                .find(|e| e.message_type == "SessionStart")
                .map(|e| e.session_id.clone())
                .unwrap_or_default();
            replay_local(&session_id, &log, None, candidate)
        }
        Source::Session {
            target,
            token,
            session_id,
        } => replay_remote(target, token, session_id, candidate).await,
    }
}

fn read_log(path: &Path) -> Result<Vec<LogEntry>, ReplayError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ReplayError::Unavailable(format!("{}: {e}", path.display())))?;
    if text.trim_start().starts_with('[') {
        return serde_json::from_str(&text)
            .map_err(|e| ReplayError::Integrity(format!("{}: {e}", path.display())));
    }
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(n, l)| {
            serde_json::from_str(l)
                .map_err(|e| ReplayError::Integrity(format!("{}:{}: {e}", path.display(), n + 1)))
        })
        .collect()
}

fn read_stored(path: &Path) -> Result<Option<Session>, ReplayError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(path)
        .map_err(|e| ReplayError::Unavailable(format!("{}: {e}", path.display())))?;
    let persisted: PersistedSession = serde_json::from_slice(&bytes)
        .map_err(|e| ReplayError::Integrity(format!("{}: {e}", path.display())))?;
    Ok(Some(Session::from(persisted)))
}

fn replay_local(
    session_id: &str,
    log: &[LogEntry],
    stored: Option<&Session>,
    candidate: Option<&PolicyDefinition>,
) -> Result<WhatIfReplayResponse, ReplayError> {
    let modes = ModeRegistry::build_default(Arc::new(macp_runtime::policy::DefaultPolicyEvaluator));
    let policies = PolicyRegistry::new();
    let candidate = match candidate {
        Some(c) => c.clone(),
        None => recorded_policy(log, stored, &policies).ok_or_else(|| {
            ReplayError::Unavailable(
                "the session's policy is not built in; pass it with --policy".into(),
            )
        })?,
    };
    match what_if_over_log(session_id, log, stored, &modes, &policies, &candidate) {
        Ok(run) => Ok(macp_runtime::server::what_if::response(run)),
        Err(WhatIfError::Integrity(m)) => Err(ReplayError::Integrity(m)),
        Err(WhatIfError::InvalidCandidate(m)) => Err(ReplayError::Unavailable(m)),
        Err(WhatIfError::Compacted) => Err(ReplayError::Unavailable(
            "HISTORY_UNAVAILABLE: the log no longer holds the full history (compacted)".into(),
        )),
        Err(WhatIfError::UnknownSession) => Err(ReplayError::Unavailable(format!(
            "unknown session '{session_id}'"
        ))),
    }
}

async fn replay_remote(
    target: &str,
    token: &str,
    session_id: &str,
    candidate: Option<&PolicyDefinition>,
) -> Result<WhatIfReplayResponse, ReplayError> {
    let unavailable = |s: tonic::Status| ReplayError::Unavailable(crate::tail::status_message(&s));
    let bearer = MetadataValue::try_from(format!("Bearer {token}"))
        .map_err(|_| ReplayError::Unavailable("the token is not a valid bearer value".into()))?;
    let mut client = client::connect(target)
        .await
        .map_err(ReplayError::Unavailable)?;

    let descriptor = match candidate {
        Some(c) => PolicyDescriptor {
            policy_id: c.policy_id.clone(),
            mode: c.mode.clone(),
            description: c.description.clone(),
            rules: c.rules.to_string(),
            schema_version: c.schema_version,
            registered_at_unix_ms: 0,
        },
        // The recorded policy, as the runtime's registry holds it now. The
        // recorded half replays under the bound definition either way.
        None => {
            let meta = client
                .get_session(authed(
                    &bearer,
                    tonic::Request::new(GetSessionRequest {
                        session_id: session_id.to_string(),
                    }),
                ))
                .await
                .map_err(unavailable)?
                .into_inner()
                .metadata
                .unwrap_or_default();
            let policy_id = if meta.policy_version.is_empty() {
                "policy.default".to_string()
            } else {
                meta.policy_version
            };
            client
                .get_policy(authed(
                    &bearer,
                    tonic::Request::new(GetPolicyRequest { policy_id }),
                ))
                .await
                .map_err(unavailable)?
                .into_inner()
                .policy_descriptor
                .ok_or_else(|| ReplayError::Unavailable("GetPolicy returned no policy".into()))?
        }
    };
    match client
        .what_if_replay(authed(
            &bearer,
            tonic::Request::new(WhatIfReplayRequest {
                session_id: session_id.to_string(),
                candidate: Some(descriptor),
            }),
        ))
        .await
    {
        Ok(resp) => Ok(resp.into_inner()),
        // The runtime reports a recorded replay that diverges as
        // `INTERNAL_ERROR: ...` (see `src/server.rs`'s `what_if_replay`).
        Err(s) if s.message().starts_with("INTERNAL_ERROR") => {
            Err(ReplayError::Integrity(s.message().to_string()))
        }
        Err(s) if s.code() == tonic::Code::Unimplemented => Err(ReplayError::Unavailable(format!(
            "{target} does not serve WhatIfReplay (macp-runtime 0.10.0 or later does)"
        ))),
        Err(s) => Err(unavailable(s)),
    }
}

fn authed<T>(bearer: &MetadataValue<Ascii>, mut req: tonic::Request<T>) -> tonic::Request<T> {
    req.metadata_mut().insert("authorization", bearer.clone());
    req
}

/// Whether the candidate's outcome differs from the recorded one: final
/// state, resolution, or the entry that resolved it.
pub fn outcome_changed(r: &WhatIfReplayResponse) -> bool {
    r.recorded != r.candidate
}

/// The report as text. Entries are numbered `#N` with N the 1-based accepted
/// ordinal, as `macp session tail` prints them (`history_index` + 1).
pub fn render_text(r: &WhatIfReplayResponse, with_candidate: bool) -> String {
    let mut out = String::new();
    let mut line = |k: &str, v: String| out.push_str(&format!("{k:<10} {v}\n"));
    line(
        "recorded",
        format!(
            "{:<24} {}",
            r.recorded_policy_id,
            outcome_text(r.recorded.as_ref())
        ),
    );
    if !with_candidate {
        line("integrity", "ok".into());
        return out;
    }
    line(
        "candidate",
        format!(
            "{:<24} {}",
            r.candidate_policy_id,
            outcome_text(r.candidate.as_ref())
        ),
    );
    if let Some(i) = r.first_divergence {
        let e = r.entries.iter().find(|e| e.history_index == i);
        line(
            "diverges",
            match e {
                Some(e) => format!(
                    "at #{} {} {}: recorded {}, candidate {}",
                    i + 1,
                    e.message_type,
                    e.message_id,
                    entry_text(e.recorded.as_ref()),
                    entry_text(e.candidate.as_ref())
                ),
                None => format!("at #{}", i + 1),
            },
        );
    }
    for d in &r.denials {
        let reasons = denial_reasons(&d.details);
        line(
            "denied",
            format!(
                "#{} {}: {}",
                d.history_index + 1,
                d.message_id,
                reasons.join("; ")
            ),
        );
    }
    line(
        "outcome",
        if outcome_changed(r) {
            "changed"
        } else {
            "unchanged"
        }
        .into(),
    );
    out
}

fn outcome_text(o: Option<&WhatIfOutcome>) -> String {
    let Some(o) = o else { return "-".into() };
    if o.state == 0 {
        return "no session (SessionStart refused)".into();
    }
    let mut s = state_name(o.state);
    if !o.resolved_by_message_id.is_empty() {
        s.push_str(&format!(
            " by {} {} at #{}",
            o.resolved_by_message_type,
            o.resolved_by_message_id,
            o.resolved_by_history_index + 1
        ));
    }
    s
}

fn entry_text(o: Option<&WhatIfEntryOutcome>) -> String {
    let Some(o) = o else { return "-".into() };
    match Kind::try_from(o.kind) {
        Ok(Kind::Applied) => format!("applied ({})", state_name(o.state)),
        Ok(Kind::NotOpen) => "not open".into(),
        Ok(Kind::Refused) => format!("refused {}", o.error_code),
        Ok(Kind::NoSession) => "no session".into(),
        _ => format!("unknown({})", o.kind),
    }
}

/// `CODE: message` for each structured denial in `details`.
fn denial_reasons(details: &[u8]) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_slice(details).unwrap_or_default();
    v.get("denials")
        .and_then(|d| d.as_array())
        .map(|ds| {
            ds.iter()
                .map(|d| {
                    format!(
                        "{}: {}",
                        d.get("code").and_then(|c| c.as_str()).unwrap_or("?"),
                        d.get("message").and_then(|m| m.as_str()).unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The report as JSON: the `WhatIfReplayResponse` fields, with `details`
/// parsed, the resolution decoded, and `state` named. Adds `outcome_changed`.
pub fn render_json(r: &WhatIfReplayResponse) -> serde_json::Value {
    use serde_json::{json, Value};
    let details = |b: &[u8]| -> Value {
        if b.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(b).unwrap_or(Value::Null)
        }
    };
    let state = |s: i32| -> Value {
        if s == 0 {
            Value::Null
        } else {
            state_name(s).into()
        }
    };
    let outcome = |o: Option<&WhatIfOutcome>| -> Value {
        let o = o.cloned().unwrap_or_default();
        json!({
            "state": state(o.state),
            "resolution": macp_conform::encode::commitment_to_json(&o.resolution)
                .filter(|_| !o.resolution.is_empty()),
            "resolution_base64": base64::engine::general_purpose::STANDARD.encode(&o.resolution),
            "resolved_by_history_index": (!o.resolved_by_message_id.is_empty())
                .then_some(o.resolved_by_history_index),
            "resolved_by_message_id": o.resolved_by_message_id,
            "resolved_by_message_type": o.resolved_by_message_type,
        })
    };
    let entry_outcome = |o: Option<&WhatIfEntryOutcome>| -> Value {
        let o = o.cloned().unwrap_or_default();
        let kind = match Kind::try_from(o.kind) {
            Ok(Kind::Applied) => "applied",
            Ok(Kind::NotOpen) => "not_open",
            Ok(Kind::Refused) => "refused",
            Ok(Kind::NoSession) => "no_session",
            _ => "unspecified",
        };
        json!({
            "kind": kind,
            "state": state(o.state),
            "error_code": o.error_code,
            "details": details(&o.details),
        })
    };
    json!({
        "candidate_policy_id": r.candidate_policy_id,
        "recorded_policy_id": r.recorded_policy_id,
        "recorded": outcome(r.recorded.as_ref()),
        "candidate": outcome(r.candidate.as_ref()),
        "first_divergence": r.first_divergence,
        "outcome_changed": outcome_changed(r),
        "denials": r.denials.iter().map(|d| json!({
            "history_index": d.history_index,
            "message_id": d.message_id,
            "details": details(&d.details),
        })).collect::<Vec<_>>(),
        "entries": r.entries.iter().map(|e| json!({
            "history_index": e.history_index,
            "message_id": e.message_id,
            "message_type": e.message_type,
            "recorded": entry_outcome(e.recorded.as_ref()),
            "candidate": entry_outcome(e.candidate.as_ref()),
        })).collect::<Vec<_>>(),
    })
}
