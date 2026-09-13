//! The handoff implicit accept, through the live `Runtime` (RFC-MACP-0010
//! §5.1) — Phase 11e's acceptance harness.
//!
//! Everything here exercises the real kernel path: `Runtime::process` ->
//! `process_message` -> `Runtime::synthesize_due_accept` -> durable append ->
//! `StreamSession` publish. The mode-level unit tests
//! (`crates/macp-modes/src/mode/handoff.rs`) pin the envelope's *contents*; the
//! replay fixtures (`src/replay.rs`) pin what a recorded history rebuilds to.
//! What only this file can prove is that the two meet — that the bytes the
//! mode says are due are the bytes the kernel writes down, in the right place,
//! at the right time, and that they replay to the session the live runtime
//! holds.
//!
//! Two backends, on purpose:
//!
//! - **`MemoryBackend`** for everything that asserts on the *shape of the log*.
//!   It does not implement `replace_log`, so terminal-session compaction fails
//!   and the full entry list survives resolution. A `FileBackend` compacts a
//!   resolved session's log down to a single checkpoint
//!   (`Runtime::maybe_compact_log`), which would erase the very ordering these
//!   tests exist to pin.
//! - **`FileBackend`** for the snapshot-agreement claims, because
//!   `MemoryBackend::save_session` is a no-op returning `Ok(())` and
//!   `load_session` always returns `None` — so those criteria would pass
//!   vacuously against it.

use macp_runtime::log_store::{EntryKind, LogEntry, LogStore};
use macp_runtime::macp_core::policy::PolicyDefinition;
use macp_runtime::pb::{CommitmentPayload, Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::session::{Session, SessionState};
use macp_runtime::storage::FileBackend;
use prost::Message;
use std::sync::Arc;

const MODE: &str = "macp.mode.handoff.v1";
const OWNER: &str = "agent://owner";
const TARGET: &str = "agent://target";
const POLICY_ID: &str = "handoff-auto-accept";
/// Short enough to keep the suite fast, long enough that ordinary scheduling
/// jitter cannot make an offer look due before the test intends it to.
const TIMEOUT_MS: i64 = 60;
const SYNTHETIC_ID: &str = "implicit-accept:h1";

struct Harness {
    rt: Runtime,
    /// Kept alive for the lifetime of the harness — dropping it deletes the
    /// backing directory out from under a `FileBackend`.
    _dir: Option<tempfile::TempDir>,
}

/// The log-shape harness: `MemoryBackend`, so a resolved session's log is not
/// compacted away before it can be asserted on.
fn make_harness() -> Harness {
    harness_over(Arc::new(macp_runtime::storage::MemoryBackend), None)
}

/// The durability harness: a real `FileBackend` over a tempdir, for the
/// snapshot-agreement criteria.
fn make_durable_harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(FileBackend::new(dir.path().to_path_buf()).unwrap());
    harness_over(storage, Some(dir))
}

fn harness_over(
    storage: Arc<dyn macp_runtime::storage::StorageBackend>,
    dir: Option<tempfile::TempDir>,
) -> Harness {
    let rt = Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    );
    rt.register_policy(PolicyDefinition {
        policy_id: POLICY_ID.into(),
        mode: MODE.into(),
        description: "implicit accept after a short timeout".into(),
        rules: serde_json::json!({
            "acceptance": { "implicit_accept_timeout_ms": TIMEOUT_MS },
            "commitment": { "authority": "initiator_only" }
        }),
        schema_version: 1,
    })
    .expect("policy registers");
    Harness { rt, _dir: dir }
}

/// Guard against the trap that made five of these tests fail on first run: a
/// backend that supports `replace_log` compacts a resolved session's log down
/// to one checkpoint, so any assertion about entry order must run against a
/// backend that cannot.
fn assert_log_is_uncompacted(entries: &[LogEntry]) {
    assert!(
        !entries
            .iter()
            .any(|e| e.entry_kind == EntryKind::Checkpoint && e.compacted_incoming_ordinals > 0),
        "this assertion needs the full log; use make_harness() (MemoryBackend)"
    );
}

fn new_sid() -> String {
    uuid::Uuid::new_v4().as_hyphenated().to_string()
}

fn env(
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

fn start_payload(max_suspend_ms: i64) -> Vec<u8> {
    SessionStartPayload {
        intent: "escalate".into(),
        participants: vec![OWNER.into(), TARGET.into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: POLICY_ID.into(),
        ttl_ms: 60_000,
        context_id: String::new(),
        extensions: std::collections::HashMap::new(),
        roots: vec![],
        max_suspend_ms,
    }
    .encode_to_vec()
}

fn commitment(mode_version: &str) -> Vec<u8> {
    CommitmentPayload {
        commitment_id: "c1".into(),
        action: "handoff.accepted".into(),
        authority_scope: "support".into(),
        reason: "bound".into(),
        mode_version: mode_version.into(),
        policy_version: POLICY_ID.into(),
        configuration_version: "cfg-1".into(),
        outcome_positive: true,
        supersedes: None,
    }
    .encode_to_vec()
}

fn offer_payload() -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffOfferPayload {
        handoff_id: "h1".into(),
        target_participant: TARGET.into(),
        scope: "support".into(),
        reason: "escalate".into(),
    }
    .encode_to_vec()
}

fn accept_payload(implicit: bool) -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffAcceptPayload {
        handoff_id: "h1".into(),
        accepted_by: TARGET.into(),
        reason: "ready".into(),
        implicit,
    }
    .encode_to_vec()
}

fn context_payload() -> Vec<u8> {
    macp_runtime::handoff_pb::HandoffContextPayload {
        handoff_id: "h1".into(),
        content_type: "text/plain".into(),
        context: b"background".to_vec(),
    }
    .encode_to_vec()
}

/// An Open handoff session at the current semantics revision with one
/// outstanding offer `h1`, bound to [`POLICY_ID`].
async fn session_with_offer(rt: &Runtime) -> String {
    let sid = new_sid();
    rt.process(
        &env("SessionStart", "start-1", &sid, OWNER, start_payload(0)),
        None,
    )
    .await
    .expect("session start");
    rt.process(
        &env("HandoffOffer", "offer-1", &sid, OWNER, offer_payload()),
        None,
    )
    .await
    .expect("offer");
    let session = rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(
        session.semantics_rev,
        macp_runtime::macp_core::session::CURRENT_SEMANTICS_REV,
        "the synthesis path is gated on rev >= 2"
    );
    sid
}

async fn log_of(rt: &Runtime, sid: &str) -> Vec<LogEntry> {
    rt.log_store.get_log(sid).await.expect("session log")
}

fn incoming(entries: &[LogEntry]) -> Vec<&LogEntry> {
    assert_log_is_uncompacted(entries);
    entries
        .iter()
        .filter(|e| e.entry_kind == EntryKind::Incoming)
        .collect()
}

fn synthetic_of(entries: &[LogEntry]) -> &LogEntry {
    let found: Vec<&LogEntry> = entries
        .iter()
        .filter(|e| e.message_id == SYNTHETIC_ID)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "exactly one synthetic entry must be in the log"
    );
    found[0]
}

/// `received_at_ms` of the accepted `HandoffOffer` entry — the recorded
/// acceptance clock the deadline is measured from. Read back out of the log
/// rather than from a clock in the test, so the expected `D` is derived the
/// same way the runtime derives it.
fn offer_received_at(entries: &[LogEntry]) -> i64 {
    entries
        .iter()
        .find(|e| e.message_type == "HandoffOffer")
        .expect("offer entry")
        .received_at_ms
}

/// Sleep past the bound timeout with margin, so the accept is unambiguously
/// due by the time the trigger is processed.
async fn sleep_past_the_deadline() {
    tokio::time::sleep(std::time::Duration::from_millis(TIMEOUT_MS as u64 + 40)).await;
}

/// The four fields `assert_replay_equivalence` compares, re-implemented here.
///
/// Not because that helper is dormant — corrected 2026-09-11, it runs on every
/// conformance fixture — but because it is private to
/// `tests/conformance_loader.rs` **and** no fixture exercises an implicit
/// accept. None can be added either: `tests/conformance/` is vendored from the
/// spec repo and CI byte-diffs it, failing on any EXTRA local fixture. So the
/// replay-equivalence criterion is discharged here, by intent rather than by
/// letter.
fn assert_replay_matches(live: &Session, replayed: &Session) {
    assert_eq!(replayed.state, live.state, "state");
    assert_eq!(replayed.resolution, live.resolution, "resolution");
    assert_eq!(
        replayed.mode_state, live.mode_state,
        "mode_state must be byte-identical"
    );
    assert_eq!(
        replayed.seen_message_ids, live.seen_message_ids,
        "dedup sets must agree"
    );
}

/// Replay the live log, **forcing every entry through dispatch** wherever the
/// log still contains a `SessionStart`.
///
/// Checkpoints are stripped first, and that is not a detail. Resolving a
/// session makes the runtime write a checkpoint carrying the whole serialized
/// session (`force_insert_checkpoint`, or a compacting backend's replacement
/// entry), and `replay_session` resumes from the newest checkpoint it finds.
/// A replay that starts there re-dispatches *nothing* — it deserializes the
/// answer. Measured: with the checkpoint left in, dropping the synthetic
/// entry from the log entirely leaves this file's replay assertions green.
/// Stripping checkpoints is what makes them a claim about the log.
///
/// When stripping would leave no `SessionStart` — a compacting backend's
/// terminal log is a single checkpoint and nothing else — the log is replayed
/// as-is, because that genuinely is all a restart would have.
async fn replay_live_log(h: &Harness, sid: &str) -> Session {
    let full = log_of(&h.rt, sid).await;
    let stripped: Vec<LogEntry> = full
        .iter()
        .filter(|e| e.entry_kind != EntryKind::Checkpoint)
        .cloned()
        .collect();
    let entries = if stripped.iter().any(|e| e.message_type == "SessionStart") {
        stripped
    } else {
        full
    };
    replay_session(
        sid,
        &entries,
        h.rt.mode_registry(),
        Some(h.rt.policy_registry()),
    )
    .expect("the live log must replay")
}

// ---------------------------------------------------------------------------
// Criterion 1 — the synthetic enters history before the trigger.
// ---------------------------------------------------------------------------

/// RFC-MACP-0010 §5.1(2): the synthetic `HandoffAccept` is appended to
/// accepted history **before** the message whose evaluation depends on it.
///
/// "Before" is asserted positionally, on adjacent accepted entries, because
/// that is the only thing that makes the ordering claim falsifiable: an
/// implementation that appended the synthetic *after* the commitment would
/// still end with both entries in the log and the session resolved.
#[tokio::test]
async fn lazy_synthesis_enters_history_before_the_trigger() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;

    let result =
        h.rt.process(
            &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
            None,
        )
        .await
        .expect("the commitment resolves once the accept is in history");
    assert_eq!(result.session_state, SessionState::Resolved);

    let entries = log_of(&h.rt, &sid).await;
    let accepted = incoming(&entries);
    let ids: Vec<&str> = accepted.iter().map(|e| e.message_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["start-1", "offer-1", SYNTHETIC_ID, "commit-1"],
        "the synthetic must sit immediately before the trigger"
    );

    let syn = synthetic_of(&entries);
    assert_eq!(syn.entry_kind, EntryKind::Incoming);
    assert_eq!(syn.message_type, "HandoffAccept");
    assert_eq!(
        syn.sender, TARGET,
        "the accept is the target's, not the runtime's"
    );
    let payload =
        macp_runtime::handoff_pb::HandoffAcceptPayload::decode(&*syn.raw_payload).unwrap();
    assert_eq!(payload.handoff_id, "h1");
    assert_eq!(payload.accepted_by, TARGET);
    assert!(
        payload.implicit,
        "implicit = true is what makes it synthetic"
    );
    assert_eq!(payload.reason, "implicit accept (timeout)");

    // And the session state the accept produced, not just the bytes.
    let live = h.rt.get_session_checked(&sid).await.unwrap();
    let state: serde_json::Value = serde_json::from_slice(&live.mode_state).unwrap();
    assert_eq!(state["offers"]["h1"]["disposition"], "Accepted");
    assert_eq!(state["offers"]["h1"]["accepted_by"], TARGET);
    assert!(live.seen_message_ids.contains(SYNTHETIC_ID));
}

// ---------------------------------------------------------------------------
// Criterion 2 — the recorded clocks are the deadline, not the observation.
// ---------------------------------------------------------------------------

/// RFC-MACP-0010 §5.1(3) and the `Mode::due_synthetic_envelope` kernel
/// contract, step 2: the entry's `timestamp_unix_ms` **and** its
/// `received_at_ms` are both the computed deadline `D`, never the time the
/// runtime happened to notice.
///
/// Strict equality on both, against a `D` derived from the offer entry's own
/// recorded `received_at_ms` — and asserted on the stamped values directly
/// rather than inferred from a replay, which would pass under a wall-clock
/// stamp too. `received_at_ms` is the value replay feeds back as the dispatch
/// clock, so this is the field Phase 12's byte-identity rests on.
#[tokio::test]
async fn synthetic_timestamp_is_the_deadline_not_observation_time() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;
    // Well past the deadline, so an observation-time stamp would differ from D
    // by far more than any clock jitter.
    tokio::time::sleep(std::time::Duration::from_millis(TIMEOUT_MS as u64 + 400)).await;

    h.rt.process(
        &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
        None,
    )
    .await
    .expect("resolves");

    let entries = log_of(&h.rt, &sid).await;
    let expected_d = offer_received_at(&entries) + TIMEOUT_MS;
    let syn = synthetic_of(&entries);
    assert_eq!(
        syn.timestamp_unix_ms, expected_d,
        "envelope clock must be D"
    );
    assert_eq!(syn.received_at_ms, expected_d, "entry clock must be D");
    // Stated as the third equality too, because the trait contract is the
    // three-way one and a future refactor could satisfy either half alone.
    assert_eq!(syn.received_at_ms, syn.timestamp_unix_ms);

    // The commitment that observed it was accepted much later — i.e. D is not
    // simply "whatever the clock said".
    let commit = entries
        .iter()
        .find(|e| e.message_id == "commit-1")
        .expect("commitment entry");
    assert!(
        commit.received_at_ms > expected_d + 100,
        "the trigger must be well after D for this test to mean anything \
         (trigger {} vs D {expected_d})",
        commit.received_at_ms
    );
}

/// The suspended variant: `D` counts only unsuspended time, so a pause that
/// falls inside the window pushes `D` out by its width — and the pause must be
/// *completed* before the synthesis is asked for, which the kernel guarantees
/// by refusing to synthesize for a non-`Open` session.
#[tokio::test]
async fn synthetic_timestamp_excludes_a_pause_inside_the_window() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;

    // Pause immediately, well before the timeout could elapse.
    h.rt.suspend_session(&sid, "hold", OWNER)
        .await
        .expect("suspend");
    tokio::time::sleep(std::time::Duration::from_millis(TIMEOUT_MS as u64 + 40)).await;
    h.rt.resume_session(&sid, "go", OWNER)
        .await
        .expect("resume");
    // Not due yet: almost all the elapsed wall time was suspended.
    let session = h.rt.get_session_checked(&sid).await.unwrap();
    assert!(
        session.accumulated_suspended_ms >= TIMEOUT_MS,
        "the pause must dominate the window for this test to bite"
    );

    sleep_past_the_deadline().await;
    h.rt.process(
        &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
        None,
    )
    .await
    .expect("resolves once enough unsuspended time has elapsed");

    let entries = log_of(&h.rt, &sid).await;
    let syn = synthetic_of(&entries);
    let naive = offer_received_at(&entries) + TIMEOUT_MS;
    assert_eq!(syn.received_at_ms, syn.timestamp_unix_ms);
    assert!(
        syn.timestamp_unix_ms > naive,
        "the pause inside the window must push D out (D {} vs naive {naive})",
        syn.timestamp_unix_ms
    );
    // D is the walked deadline: the pause is excluded exactly once, so D lands
    // within the resume..trigger run rather than at some arbitrary later time.
    let resume_at = entries
        .iter()
        .find(|e| e.message_type == "SessionResume")
        .expect("resume entry")
        .received_at_ms;
    assert!(
        syn.timestamp_unix_ms >= resume_at,
        "with the whole pre-pause run shorter than the timeout, D must fall after the resume"
    );
    // The synthetic sits after the resume entry in the log even though its
    // stamp may precede later internal entries — nothing orders by
    // `received_at_ms`, and this pins that the position is emission order.
    let syn_idx = entries
        .iter()
        .position(|e| e.message_id == SYNTHETIC_ID)
        .unwrap();
    let resume_idx = entries
        .iter()
        .position(|e| e.message_type == "SessionResume")
        .unwrap();
    assert!(syn_idx > resume_idx);

    // And it replays.
    let live = h.rt.get_session_checked(&sid).await.unwrap();
    assert_replay_matches(&live, &replay_live_log(&h, &sid).await);
}

// ---------------------------------------------------------------------------
// Criterion 3 — the live history replays to the live session.
// ---------------------------------------------------------------------------

/// The determinism claim the whole design rests on: replaying the log the live
/// runtime just wrote rebuilds the session the live runtime is holding —
/// `state`, `resolution`, `mode_state` byte for byte, and the dedup set.
///
/// This is the criterion that would fail if the kernel appended an envelope it
/// had not dispatched, dispatched one it had not appended, or stamped the
/// entry with a clock replay cannot reproduce.
#[tokio::test]
async fn live_history_replays_byte_identically() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;
    h.rt.process(
        &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
        None,
    )
    .await
    .expect("resolves");

    let live = h.rt.get_session_checked(&sid).await.unwrap();
    let replayed = replay_live_log(&h, &sid).await;
    assert_eq!(live.state, SessionState::Resolved);
    assert_replay_matches(&live, &replayed);
    assert!(replayed.seen_message_ids.contains(SYNTHETIC_ID));
}

// ---------------------------------------------------------------------------
// Criterion 4 — any session-scoped message triggers synthesis.
// ---------------------------------------------------------------------------

/// §5.1(2) binds "any subsequent message", not just `Commitment`. The retired
/// interim path fired only inside `Commitment` handling; the kernel seam runs
/// ahead of every session-scoped message.
///
/// `HandoffContext` is the probe because it is accepted at any disposition, so
/// the trigger itself succeeds and cannot be confused with the rejected-trigger
/// case.
#[tokio::test]
async fn non_commitment_message_triggers_synthesis() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;

    h.rt.process(
        &env("HandoffContext", "ctx-1", &sid, OWNER, context_payload()),
        None,
    )
    .await
    .expect("context is accepted at any disposition");

    let entries = log_of(&h.rt, &sid).await;
    let ids: Vec<&str> = incoming(&entries)
        .iter()
        .map(|e| e.message_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["start-1", "offer-1", SYNTHETIC_ID, "ctx-1"],
        "a non-Commitment trigger must synthesize, and synthesize first"
    );

    let live = h.rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(
        live.state,
        SessionState::Open,
        "the session is still open — synthesis is not resolution"
    );
    assert_replay_matches(&live, &replay_live_log(&h, &sid).await);
}

// ---------------------------------------------------------------------------
// Criterion 5 — history order settles the race.
// ---------------------------------------------------------------------------

/// §5.1(4): a late explicit `HandoffAccept` loses to the synthetic that was
/// already due. The explicit message synthesizes first, then finds the offer
/// no longer `Offered` and is rejected — and the offer stands *implicitly*
/// accepted, not explicitly.
///
/// The `outcome_reason` assertion is the one that matters: without it the test
/// would pass against an implementation that let the explicit accept win, since
/// either path leaves the offer `Accepted`.
#[tokio::test]
async fn late_explicit_accept_loses_to_history_order() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;

    let err =
        h.rt.process(
            &env(
                "HandoffAccept",
                "acc-1",
                &sid,
                TARGET,
                accept_payload(false),
            ),
            None,
        )
        .await
        .expect_err("the offer is no longer Offered by the time this is dispatched");
    assert_eq!(err.to_string(), "InvalidPayload");

    let entries = log_of(&h.rt, &sid).await;
    let ids: Vec<&str> = incoming(&entries)
        .iter()
        .map(|e| e.message_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["start-1", "offer-1", SYNTHETIC_ID],
        "the synthetic is in history; the losing explicit accept is not"
    );

    let live = h.rt.get_session_checked(&sid).await.unwrap();
    let state: serde_json::Value = serde_json::from_slice(&live.mode_state).unwrap();
    assert_eq!(state["offers"]["h1"]["disposition"], "Accepted");
    assert_eq!(
        state["offers"]["h1"]["outcome_reason"], "implicit accept (timeout)",
        "the implicit accept must own the outcome, not the late explicit one"
    );
    assert_replay_matches(&live, &replay_live_log(&h, &sid).await);
}

// ---------------------------------------------------------------------------
// Criterion 6 — the reserved id cannot be squatted to block synthesis.
// ---------------------------------------------------------------------------

/// The 11c client boundary reserves the `implicit-accept:` namespace. This is
/// the reason it had to: a client that could put the deterministic id into the
/// dedup set first would make the synthetic a permanent duplicate and strand
/// the session, since no `Commitment` could ever find an accepted offer.
///
/// The squat is rejected (`InvalidEnvelope`), the rejection consumes no dedup
/// slot, and the deadline flow then commits normally.
#[tokio::test]
async fn squatter_cannot_block_synthesis() {
    let h = make_harness();
    let sid = session_with_offer(&h.rt).await;

    let err =
        h.rt.process(
            &env(
                "HandoffContext",
                SYNTHETIC_ID,
                &sid,
                OWNER,
                context_payload(),
            ),
            None,
        )
        .await
        .expect_err("the reserved namespace is closed to clients");
    assert_eq!(err.to_string(), "InvalidEnvelope");
    let live = h.rt.get_session_checked(&sid).await.unwrap();
    assert!(
        !live.seen_message_ids.contains(SYNTHETIC_ID),
        "a rejected squat must not consume the id the runtime needs"
    );

    sleep_past_the_deadline().await;
    let result =
        h.rt.process(
            &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
            None,
        )
        .await
        .expect("the squat must not have stranded the session");
    assert_eq!(result.session_state, SessionState::Resolved);

    let entries = log_of(&h.rt, &sid).await;
    synthetic_of(&entries);
    let live = h.rt.get_session_checked(&sid).await.unwrap();
    assert_replay_matches(&live, &replay_live_log(&h, &sid).await);
}

// ---------------------------------------------------------------------------
// Criterion 9 — the freeze-profile carve-out, bounded.
// ---------------------------------------------------------------------------

/// The invariant-preservation criterion for the carve-out in
/// `Runtime::synthesize_due_accept`'s rustdoc: a trigger that synthesizes and
/// is then **rejected by the mode** leaves the dedup half of the freeze-profile
/// invariant completely intact, leaves exactly one synthetic entry in history,
/// and leaves the durable snapshot in step with memory.
///
/// The rejection used is a `Commitment` with a mismatched `mode_version`, which
/// fails `validate_commitment_payload_for_session` — one member of the wide
/// rejection class (a late explicit accept/decline, an unknown `handoff_id` in
/// a `HandoffContext`, a duplicate offer, and any version mismatch all reach
/// the same place).
///
/// Part (c) is the one that fails if `synthesize_due_accept`'s
/// `save_session_to_storage` is dropped: the trigger's own save is downstream
/// of the mode call that rejected it, so nothing else would write the snapshot.
#[tokio::test]
async fn rejected_trigger_leaves_dedup_intact_and_snapshot_current() {
    let h = make_durable_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;

    // Synthesizes, then is rejected on the bound-version check.
    let err =
        h.rt.process(
            &env("Commitment", "commit-1", &sid, OWNER, commitment("9.9.9")),
            None,
        )
        .await
        .expect_err("mode_version does not match the session binding");
    assert_eq!(err.to_string(), "InvalidPayload");

    let live = h.rt.get_session_checked(&sid).await.unwrap();

    // (b) exactly one synthetic entry, and the rejected trigger is not in the
    //     log. Asserted here, before the session resolves and its log is
    //     compacted to a checkpoint.
    let entries = log_of(&h.rt, &sid).await;
    synthetic_of(&entries);
    let ids: Vec<&str> = incoming(&entries)
        .iter()
        .map(|e| e.message_id.as_str())
        .collect();
    assert_eq!(ids, vec!["start-1", "offer-1", SYNTHETIC_ID]);

    // (c) the durable snapshot agrees with memory. Nothing but
    //     `synthesize_due_accept`'s own save can have written it: the trigger
    //     was rejected, so `process_message` returned before its save.
    let snapshot =
        h.rt.storage
            .load_session(&sid)
            .await
            .expect("snapshot load")
            .expect("a snapshot must exist");
    assert_eq!(
        snapshot.mode_state, live.mode_state,
        "the synthesis must persist its own snapshot"
    );
    assert_eq!(snapshot.seen_message_ids, live.seen_message_ids);
    assert!(snapshot.seen_message_ids.contains(SYNTHETIC_ID));

    // (a) the dedup half of the invariant is untouched, in both directions.
    assert!(
        !live.seen_message_ids.contains("commit-1"),
        "a rejected message must not consume its dedup slot"
    );
    let result =
        h.rt.process(
            &env("Commitment", "commit-1", &sid, OWNER, commitment("1.0.0")),
            None,
        )
        .await
        .expect("the same message_id must still be usable after a rejection");
    assert_eq!(result.session_state, SessionState::Resolved);
    // Still exactly one synthetic: the second trigger must not re-emit.
    let live = h.rt.get_session_checked(&sid).await.unwrap();
    assert_eq!(
        live.seen_message_ids
            .iter()
            .filter(|id| id.as_str() == SYNTHETIC_ID)
            .count(),
        1
    );

    // ...and the log agrees with both.
    assert_replay_matches(&live, &replay_live_log(&h, &sid).await);
}

/// The snapshot claim again, with the sharpest possible contrast: the session
/// is asserted to have *no* prior snapshot carrying the synthetic, so the only
/// write that can have produced one is the synthesis's own.
///
/// Kept separate from the criterion-9 test above so that reordering that
/// test's parts can never accidentally let a later successful message write
/// the snapshot and mask a missing `save_session_to_storage`.
#[tokio::test]
async fn snapshot_is_current_immediately_after_the_rejected_trigger() {
    let h = make_durable_harness();
    let sid = session_with_offer(&h.rt).await;
    sleep_past_the_deadline().await;

    h.rt.process(
        &env("Commitment", "commit-1", &sid, OWNER, commitment("9.9.9")),
        None,
    )
    .await
    .expect_err("rejected");

    let live = h.rt.get_session_checked(&sid).await.unwrap();
    let snapshot =
        h.rt.storage
            .load_session(&sid)
            .await
            .expect("snapshot load")
            .expect("a snapshot must exist");
    assert_eq!(
        snapshot.mode_state, live.mode_state,
        "the synthesis's own save is the only thing that can have written this"
    );
    assert!(snapshot.seen_message_ids.contains(SYNTHETIC_ID));
    assert_eq!(snapshot.seen_message_ids, live.seen_message_ids);

    // The replayed log agrees with the snapshot too — i.e. no divergence for
    // `replay::validate_replay_consistency` to warn about at startup.
    let replayed = replay_live_log(&h, &sid).await;
    assert_eq!(replayed.mode_state, snapshot.mode_state);
    assert_replay_matches(&live, &replayed);
}
