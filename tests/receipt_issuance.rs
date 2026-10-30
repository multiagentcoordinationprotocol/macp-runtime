//! Decision-receipt issuance through the kernel (RFC-MACP-0013 §13): lazy,
//! first-writer-wins, byte-stable across calls, eviction and restart.

use macp_core::receipt::{add_jwks, verify, ReceiptSigner, TrustedKeys, Verdict};
use macp_runtime::error::MacpError;
use macp_runtime::log_store::LogStore;
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::policy::registry::PolicyRegistry;
use macp_runtime::receipts::ReceiptConfig;
use macp_runtime::registry::SessionRegistry;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::SessionState;
use macp_runtime::storage::{FileBackend, MemoryBackend, StorageBackend};
use prost::Message;
use std::sync::Arc;

const MODE: &str = "macp.mode.decision.v1";

fn envelope(
    message_type: &str,
    message_id: &str,
    sid: &str,
    sender: &str,
    payload: Vec<u8>,
) -> Envelope {
    Envelope {
        macp_version: "1.0".into(),
        mode: MODE.into(),
        message_type: message_type.into(),
        message_id: message_id.into(),
        session_id: sid.into(),
        sender: sender.into(),
        timestamp_unix_ms: chrono::Utc::now().timestamp_millis(),
        payload,
    }
}

fn signer(seed: u8, kid: &str) -> ReceiptSigner {
    ReceiptSigner::from_seed(&[seed; 32], kid).unwrap()
}

fn runtime(storage: Arc<dyn StorageBackend>, config: ReceiptConfig) -> Runtime {
    Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    )
    .with_receipt_config(config)
}

fn signed_config(seed: u8, kid: &str) -> ReceiptConfig {
    ReceiptConfig::new(Some(signer(seed, kid)), Default::default(), Some("test")).unwrap()
}

/// The resolving Commitment's payload bytes.
fn commitment_wire() -> Vec<u8> {
    CommitmentPayload {
        commitment_id: "c1".into(),
        action: "decision.selected".into(),
        authority_scope: "test".into(),
        reason: "done".into(),
        mode_version: "1.0.0".into(),
        policy_version: "policy.default".into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

/// Start a decision session; resolve it unless `open`.
async fn decide(rt: &Runtime, open: bool) -> String {
    let sid = uuid::Uuid::new_v4().as_hyphenated().to_string();
    let start = SessionStartPayload {
        intent: "receipts".into(),
        participants: vec!["agent://o".into(), "agent://a".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: 60_000,
        ..Default::default()
    }
    .encode_to_vec();
    rt.process(
        &envelope("SessionStart", "m1", &sid, "agent://o", start),
        None,
    )
    .await
    .unwrap();
    if open {
        return sid;
    }
    let proposal = macp_runtime::decision_pb::ProposalPayload {
        proposal_id: "p1".into(),
        option: "deploy".into(),
        rationale: "ready".into(),
        supporting_data: vec![],
    }
    .encode_to_vec();
    rt.process(
        &envelope("Proposal", "m2", &sid, "agent://o", proposal),
        None,
    )
    .await
    .unwrap();
    let vote = macp_runtime::decision_pb::VotePayload {
        proposal_id: "p1".into(),
        vote: "APPROVE".into(),
        reason: "good".into(),
    }
    .encode_to_vec();
    rt.process(&envelope("Vote", "m3", &sid, "agent://a", vote), None)
        .await
        .unwrap();
    let result = rt
        .process(
            &envelope("Commitment", "m4", &sid, "agent://o", commitment_wire()),
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.session_state, SessionState::Resolved);
    sid
}

fn file_storage(dir: &tempfile::TempDir) -> Arc<dyn StorageBackend> {
    Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap())
}

fn json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

fn trust(rt: &Runtime) -> TrustedKeys {
    let mut trusted = TrustedKeys::new();
    add_jwks(
        &serde_json::from_str(rt.receipt_keys_json()).unwrap(),
        &mut trusted,
    )
    .unwrap();
    trusted
}

#[tokio::test]
async fn signed_receipt_attests_and_is_served_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let rt = runtime(Arc::clone(&storage), signed_config(7, "k1"));
    let sid = decide(&rt, false).await;

    // Lazy: resolving stored nothing.
    assert_eq!(storage.load_receipt(&sid).await.unwrap(), None);

    let first = rt.decision_receipt(&sid).await.unwrap();
    let receipt = json(&first);
    assert_eq!(receipt["session_id"], sid.as_str());
    assert_eq!(receipt["mode"], MODE);
    assert_eq!(receipt["issuer"], "runtime:test");
    assert_eq!(receipt["semantics_rev"]["implementation"], "macp-runtime");
    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(receipt["semantics_rev"]["rev"], session.semantics_rev);
    assert_eq!(session.resolution.as_deref(), Some(&commitment_wire()[..]));
    use base64::Engine as _;
    assert_eq!(
        receipt["commitment_wire_b64"],
        base64::engine::general_purpose::STANDARD.encode(commitment_wire())
    );
    let issued_at = receipt["issued_at"].as_str().unwrap();
    assert!(
        issued_at.ends_with('Z') && !issued_at.contains('.'),
        "{issued_at}"
    );
    assert_eq!(verify(&receipt, &trust(&rt)).verdict, Verdict::Attested);

    assert_eq!(
        storage.load_receipt(&sid).await.unwrap().as_deref(),
        Some(&first[..])
    );
    assert_eq!(rt.decision_receipt(&sid).await.unwrap(), first);
    assert_eq!(rt.metrics().receipts_issued(), (1, 0));
}

#[tokio::test]
async fn unsigned_receipt_is_integrity_only() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(file_storage(&dir), ReceiptConfig::default());
    let sid = decide(&rt, false).await;
    let receipt = json(&rt.decision_receipt(&sid).await.unwrap());
    assert_eq!(receipt["attestations"], serde_json::json!([]));
    assert_eq!(receipt["issuer"], "runtime:macp-runtime");
    assert_eq!(
        verify(&receipt, &TrustedKeys::new()).verdict,
        Verdict::IntegrityOnly
    );
    assert_eq!(rt.receipt_keys_json(), r#"{"keys":[]}"#);
    assert_eq!(rt.metrics().receipts_issued(), (0, 1));
}

#[tokio::test]
async fn ineligible_and_unknown_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let rt = runtime(Arc::clone(&storage), signed_config(7, "k1"));
    let open = decide(&rt, true).await;
    assert!(matches!(
        rt.decision_receipt(&open).await,
        Err(MacpError::ReceiptUnavailable)
    ));
    assert_eq!(storage.load_receipt(&open).await.unwrap(), None);
    assert!(matches!(
        rt.decision_receipt("00000000-0000-4000-8000-000000000000")
            .await,
        Err(MacpError::UnknownSession)
    ));
    assert_eq!(rt.metrics().receipts_issued(), (0, 0));
}

#[tokio::test]
async fn evicted_sessions_issue_and_serve_from_storage() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(file_storage(&dir), signed_config(7, "k1"));
    let served = decide(&rt, false).await;
    let never_served = decide(&rt, false).await;
    let before = rt.decision_receipt(&served).await.unwrap();

    rt.evict_stale_sessions(0).await;
    // Retention is measured from session start; make sure both are gone.
    assert!(rt.registry.get_shared(&served).await.is_none());
    assert!(rt.registry.get_shared(&never_served).await.is_none());

    assert_eq!(rt.decision_receipt(&served).await.unwrap(), before);
    let issued = json(&rt.decision_receipt(&never_served).await.unwrap());
    assert_eq!(verify(&issued, &trust(&rt)).verdict, Verdict::Attested);
    assert!(
        rt.registry.get_shared(&never_served).await.is_none(),
        "not re-inserted"
    );
}

#[tokio::test]
async fn restart_with_a_rotated_key_serves_the_first_receipt() {
    for compact in [true, false] {
        restart_serves_the_first_receipt(compact).await;
    }
}

async fn restart_serves_the_first_receipt(compact: bool) {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let rt =
        runtime(Arc::clone(&storage), signed_config(7, "k1")).with_compact_terminal_logs(compact);
    let sid = decide(&rt, false).await;
    let unserved = decide(&rt, false).await;
    let first = rt.decision_receipt(&sid).await.unwrap();
    drop(rt);

    let registry = Arc::new(SessionRegistry::new());
    let log_store = Arc::new(LogStore::new());
    let modes = ModeRegistry::build_default(Arc::new(macp_runtime::policy::DefaultPolicyEvaluator));
    let report = macp_runtime::recovery::recover_sessions(
        storage.as_ref(),
        &registry,
        &log_store,
        &modes,
        &PolicyRegistry::new(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(report.recovered, 2);
    // Recovery never issues.
    assert_eq!(
        storage.load_receipt(&sid).await.unwrap().as_deref(),
        Some(&first[..])
    );
    assert_eq!(storage.load_receipt(&unserved).await.unwrap(), None);

    let rt = Runtime::new(Arc::clone(&storage), registry, log_store)
        .with_receipt_config(signed_config(9, "k2"));
    assert_eq!(rt.decision_receipt(&sid).await.unwrap(), first);
    assert_eq!(rt.metrics().receipts_issued(), (0, 0));
}

#[tokio::test]
async fn concurrent_first_calls_issue_once() {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let rt = Arc::new(runtime(Arc::clone(&storage), signed_config(7, "k1")));
    let sid = decide(&rt, false).await;
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let (rt, sid) = (Arc::clone(&rt), sid.clone());
        calls.spawn(async move { rt.decision_receipt(&sid).await.unwrap() });
    }
    let receipts = calls.join_all().await;
    assert!(receipts.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(
        storage.load_receipt(&sid).await.unwrap().as_deref(),
        Some(&receipts[0][..])
    );
    assert_eq!(rt.metrics().receipts_issued(), (1, 0));
}

#[tokio::test]
async fn memory_backend_serves_a_stable_in_memory_receipt() {
    let storage: Arc<dyn StorageBackend> = Arc::new(MemoryBackend);
    let rt = runtime(storage, signed_config(7, "k1"));
    let sid = decide(&rt, false).await;
    let first = rt.decision_receipt(&sid).await.unwrap();
    assert_eq!(rt.decision_receipt(&sid).await.unwrap(), first);
    assert_eq!(rt.metrics().receipts_issued(), (1, 0));
}

#[tokio::test]
async fn disk_gc_deletes_the_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let rt = runtime(Arc::clone(&storage), ReceiptConfig::default());
    let sid = decide(&rt, false).await;
    rt.decision_receipt(&sid).await.unwrap();
    // Session start is "now"; a cutoff in the future needs negative retention,
    // so wait past a 0-second retention window instead.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    assert_eq!(rt.gc_disk_sessions(0).await, 1);
    assert_eq!(storage.load_receipt(&sid).await.unwrap(), None);
    assert!(matches!(
        rt.decision_receipt(&sid).await,
        Err(MacpError::UnknownSession)
    ));
}

#[tokio::test]
async fn expired_cancelled_extension_and_unhashable_sessions_get_no_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let storage = file_storage(&dir);
    let clock = Arc::new(macp_runtime::macp_core::ManualClock::new(
        chrono::Utc::now().timestamp_millis(),
    ));
    let rt = runtime(Arc::clone(&storage), signed_config(7, "k1")).with_clock(clock.clone());
    let unavailable =
        |r: Result<Arc<Vec<u8>>, MacpError>| matches!(r, Err(MacpError::ReceiptUnavailable));

    let expired = decide(&rt, true).await;
    let cancelled = decide(&rt, true).await;
    rt.cancel_session(&cancelled, "stop", "agent://o")
        .await
        .unwrap();
    clock.advance(61_000);
    assert!(unavailable(rt.decision_receipt(&expired).await));
    assert_eq!(
        rt.get_session_checked(&expired).await.unwrap().state,
        SessionState::Expired
    );
    assert!(unavailable(rt.decision_receipt(&cancelled).await));

    // Resolved sessions the kernel never produces through a standard mode.
    let mut ext = macp_runtime::session::Session::builder(
        "ext-session-000000000001",
        "ext.multi_round.v1",
        "agent://o",
    )
    .build();
    ext.state = SessionState::Resolved;
    ext.resolution = Some(br#"{"converged":true}"#.to_vec());
    rt.registry
        .insert_recovered_session(ext.session_id.clone(), ext)
        .await;
    assert!(unavailable(
        rt.decision_receipt("ext-session-000000000001").await
    ));

    let mut unhashable =
        macp_runtime::session::Session::builder("unknown-field-0000000001", MODE, "agent://o")
            .build();
    unhashable.state = SessionState::Resolved;
    // Field 111, varint 1: a field the hash label does not cover.
    let mut wire = commitment_wire();
    wire.extend_from_slice(&[0xF8, 0x06, 0x01]);
    unhashable.resolution = Some(wire);
    rt.registry
        .insert_recovered_session(unhashable.session_id.clone(), unhashable)
        .await;
    assert!(unavailable(
        rt.decision_receipt("unknown-field-0000000001").await
    ));

    for sid in [&expired, &cancelled] {
        assert_eq!(storage.load_receipt(sid).await.unwrap(), None);
    }
    assert_eq!(rt.metrics().receipts_issued(), (0, 0));
}

/// A file backend whose receipt writes fail while `fail` is set.
struct FlakyReceipts {
    inner: FileBackend,
    fail: std::sync::atomic::AtomicBool,
    /// Store receipts like a backend without receipt support.
    unsupported: bool,
}

#[async_trait::async_trait]
impl StorageBackend for FlakyReceipts {
    async fn save_session(&self, s: &macp_runtime::session::Session) -> std::io::Result<()> {
        self.inner.save_session(s).await
    }
    async fn load_session(
        &self,
        id: &str,
    ) -> std::io::Result<Option<macp_runtime::session::Session>> {
        self.inner.load_session(id).await
    }
    async fn load_all_sessions(&self) -> std::io::Result<Vec<macp_runtime::session::Session>> {
        self.inner.load_all_sessions().await
    }
    async fn delete_session(&self, id: &str) -> std::io::Result<()> {
        self.inner.delete_session(id).await
    }
    async fn list_session_ids(&self) -> std::io::Result<Vec<String>> {
        self.inner.list_session_ids().await
    }
    async fn append_log_entry(
        &self,
        id: &str,
        e: &macp_runtime::log_store::LogEntry,
    ) -> std::io::Result<()> {
        self.inner.append_log_entry(id, e).await
    }
    async fn load_log(&self, id: &str) -> std::io::Result<Vec<macp_runtime::log_store::LogEntry>> {
        self.inner.load_log(id).await
    }
    async fn create_session_storage(&self, id: &str) -> std::io::Result<()> {
        self.inner.create_session_storage(id).await
    }
    async fn replace_log(
        &self,
        id: &str,
        e: &[macp_runtime::log_store::LogEntry],
    ) -> std::io::Result<()> {
        self.inner.replace_log(id, e).await
    }
    async fn load_receipt(&self, id: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load_receipt(id).await
    }
    async fn store_receipt_if_absent(
        &self,
        id: &str,
        receipt: &[u8],
    ) -> std::io::Result<macp_runtime::storage::ReceiptWrite> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("disk full"));
        }
        if self.unsupported {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "no receipts here",
            ));
        }
        self.inner.store_receipt_if_absent(id, receipt).await
    }
    fn receipts_durable(&self) -> bool {
        self.inner.receipts_durable()
    }
}

#[tokio::test]
async fn a_failed_receipt_write_is_internal_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let flaky = Arc::new(FlakyReceipts {
        inner: FileBackend::new(dir.path().to_path_buf()).unwrap(),
        fail: std::sync::atomic::AtomicBool::new(true),
        unsupported: false,
    });
    let rt = runtime(flaky.clone(), signed_config(7, "k1"));
    let sid = decide(&rt, false).await;
    assert!(matches!(
        rt.decision_receipt(&sid).await,
        Err(MacpError::StorageFailed)
    ));
    assert_eq!(MacpError::StorageFailed.error_code(), "INTERNAL_ERROR");
    assert_eq!(rt.metrics().receipt_failures(), 1);
    assert_eq!(flaky.load_receipt(&sid).await.unwrap(), None);

    flaky.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    let receipt = rt.decision_receipt(&sid).await.unwrap();
    assert_eq!(
        flaky.load_receipt(&sid).await.unwrap().as_deref(),
        Some(&receipt[..])
    );
    assert_eq!(rt.metrics().receipts_issued(), (1, 0));
}

/// Issuing a receipt changes nothing replay sees.
#[tokio::test]
async fn replay_is_unchanged_by_issuance() {
    for config in [ReceiptConfig::default(), signed_config(7, "k1")] {
        let dir = tempfile::tempdir().unwrap();
        let storage = file_storage(&dir);
        let rt = runtime(Arc::clone(&storage), config).with_compact_terminal_logs(false);
        let sid = decide(&rt, false).await;
        let modes =
            ModeRegistry::build_default(Arc::new(macp_runtime::policy::DefaultPolicyEvaluator));
        let replayed = || async {
            let log = storage.load_log(&sid).await.unwrap();
            let s = macp_runtime::replay::replay_session(&sid, &log, &modes, None).unwrap();
            let mut v =
                serde_json::to_value(macp_runtime::registry::PersistedSession::from(&s)).unwrap();
            // A HashSet: compare it unordered.
            v["seen_message_ids"]
                .as_array_mut()
                .unwrap()
                .sort_by_key(|id| id.to_string());
            v
        };
        let before = replayed().await;
        rt.decision_receipt(&sid).await.unwrap();
        assert_eq!(replayed().await, before);
    }
}

/// A backend that keeps sessions but not receipts: the in-memory receipt is
/// the only copy, so eviction must not drop it while the session is readable
/// -- a rebuild would serve a second, differently signed receipt.
#[tokio::test]
async fn an_unpersisted_receipt_survives_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FlakyReceipts {
        inner: FileBackend::new(dir.path().to_path_buf()).unwrap(),
        fail: std::sync::atomic::AtomicBool::new(false),
        unsupported: true,
    });
    let rt = runtime(backend, signed_config(7, "k1"));
    let sid = decide(&rt, false).await;
    let first = rt.decision_receipt(&sid).await.unwrap();
    rt.evict_stale_sessions(0).await;
    assert!(rt.registry.get_shared(&sid).await.is_none());
    assert_eq!(rt.decision_receipt(&sid).await.unwrap(), first);
    assert_eq!(rt.metrics().receipts_issued(), (1, 0));
}

#[tokio::test]
async fn memory_backend_eviction_ends_the_session() {
    let rt = runtime(Arc::new(MemoryBackend), signed_config(7, "k1"));
    let sid = decide(&rt, false).await;
    rt.decision_receipt(&sid).await.unwrap();
    rt.evict_stale_sessions(0).await;
    assert!(matches!(
        rt.decision_receipt(&sid).await,
        Err(MacpError::UnknownSession)
    ));
}

#[tokio::test]
async fn suspended_and_passthrough_sessions_get_no_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime(file_storage(&dir), signed_config(7, "k1"));
    let suspended = decide(&rt, true).await;
    rt.suspend_session(&suspended, "hold", "agent://o")
        .await
        .unwrap();
    assert!(matches!(
        rt.decision_receipt(&suspended).await,
        Err(MacpError::ReceiptUnavailable)
    ));

    // Seam S-e: a passthrough (extension) mode resolves to a JSON summary,
    // never the Commitment bytes, and gets no receipt.
    let mode = "ext.receipts.passthrough.v1";
    rt.register_extension(macp_runtime::pb::ModeDescriptor {
        mode: mode.into(),
        mode_version: "1.0.0".into(),
        message_types: vec!["SessionStart".into(), "Commitment".into()],
        terminal_message_types: vec!["Commitment".into()],
        ..Default::default()
    })
    .unwrap();
    let sid = uuid::Uuid::new_v4().as_hyphenated().to_string();
    let start = SessionStartPayload {
        intent: "passthrough".into(),
        participants: vec!["agent://o".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        ttl_ms: 60_000,
        ..Default::default()
    }
    .encode_to_vec();
    for (message_type, id, payload) in [
        ("SessionStart", "m1", start),
        ("Commitment", "m2", commitment_wire()),
    ] {
        let mut env = envelope(message_type, id, &sid, "agent://o", payload);
        env.mode = mode.into();
        rt.process(&env, None).await.unwrap();
    }
    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(session.state, SessionState::Resolved);
    let summary: serde_json::Value =
        serde_json::from_slice(session.resolution.as_deref().unwrap()).unwrap();
    assert_eq!(
        summary,
        serde_json::json!({ "action": "decision.selected", "commitment_id": "c1" })
    );
    assert!(matches!(
        rt.decision_receipt(&sid).await,
        Err(MacpError::ReceiptUnavailable)
    ));
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

/// A wire id is a storage key only when it is one plain path component: a
/// planted `session.json` outside the data dir, or an alias of a real session,
/// must never be signed.
#[tokio::test]
async fn path_like_session_ids_never_reach_storage() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let rt = runtime(file_storage(&dir), signed_config(7, "k1"));
    let sid = decide(&rt, false).await;
    rt.evict_stale_sessions(0).await;
    let stored = dir.path().join("sessions").join(&sid);
    let planted = outside.path().join(&sid);
    copy_dir(&stored, &planted);

    let relative = format!(
        "{}/{}",
        "../".repeat(dir.path().components().count() + 1),
        planted.strip_prefix("/").unwrap().display()
    );
    for id in [
        planted.to_string_lossy().to_string(),
        relative,
        format!("./{sid}"),
        format!("../sessions/{sid}"),
        "..".to_string(),
        String::new(),
    ] {
        assert!(
            matches!(
                rt.decision_receipt(&id).await,
                Err(MacpError::UnknownSession)
            ),
            "{id:?} must be unknown"
        );
    }
    assert!(!planted.join("receipt.json").exists());
    assert!(!stored.join("receipt.json").exists());

    // A stored snapshot naming another session is not served under this id.
    let other = uuid::Uuid::new_v4().as_hyphenated().to_string();
    copy_dir(&stored, &dir.path().join("sessions").join(&other));
    assert!(matches!(
        rt.decision_receipt(&other).await,
        Err(MacpError::UnknownSession)
    ));

    // The real id still works, and the receipt names it.
    let receipt = json(&rt.decision_receipt(&sid).await.unwrap());
    assert_eq!(receipt["session_id"], sid);
}
