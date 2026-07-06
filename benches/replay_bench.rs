//! D1 (plans/current/phase-d-hardening.md): recovery and throughput baselines.
//!
//! Run with `cargo bench`. These exist to give the per-session locking rework
//! (D2) and the RocksDB sync-write change (B3) a measured before/after:
//! - replay time vs log size (100 / 1k / 10k entries)
//! - checkpoint-based vs full replay
//! - kernel message throughput, single session vs across sessions (the
//!   cross-session number is the one D2's per-session locking should move)

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use prost::Message;
use std::sync::Arc;

use macp_runtime::log_store::{EntryKind, LogEntry, LogStore};
use macp_runtime::mode_registry::ModeRegistry;
use macp_runtime::pb::{Envelope, SessionStartPayload};
use macp_runtime::registry::SessionRegistry;
use macp_runtime::replay::replay_session;
use macp_runtime::runtime::Runtime;
use macp_runtime::storage::MemoryBackend;

const MODE: &str = "ext.multi_round.v1";
const SID: &str = "11111111-1111-4111-8111-111111111111";

fn start_payload() -> Vec<u8> {
    SessionStartPayload {
        intent: "bench".into(),
        participants: vec!["agent://a".into(), "agent://b".into()],
        mode_version: "1.0.0".into(),
        configuration_version: "cfg-1".into(),
        policy_version: String::new(),
        ttl_ms: 3_600_000,
        context_id: String::new(),
        extensions: Default::default(),
        roots: vec![],
        max_suspend_ms: 0,
    }
    .encode_to_vec()
}

fn contribute_payload(v: &str) -> Vec<u8> {
    format!("{{\"value\":\"{v}\"}}").into_bytes()
}

fn incoming(message_id: &str, message_type: &str, sender: &str, payload: Vec<u8>) -> LogEntry {
    LogEntry {
        message_id: message_id.into(),
        received_at_ms: 1_700_000_000_000,
        sender: sender.into(),
        message_type: message_type.into(),
        raw_payload: payload,
        entry_kind: EntryKind::Incoming,
        session_id: SID.into(),
        mode: MODE.into(),
        macp_version: "1.0".into(),
        timestamp_unix_ms: 1_700_000_000_000,
        bound_mode_version: None,
        semantics_rev: 1,
        bound_max_suspend_ms: None,
        compacted_incoming_ordinals: 0,
    }
}

/// A log with `n` accepted Contribute messages after SessionStart.
fn log_of(n: usize) -> Vec<LogEntry> {
    let mut entries = vec![incoming("m0", "SessionStart", "agent://a", start_payload())];
    for i in 0..n {
        // Alternate values so the session never converges/resolves mid-log.
        let v = if i.is_multiple_of(2) { "x" } else { "y" };
        entries.push(incoming(
            &format!("m{}", i + 1),
            "Contribute",
            if i.is_multiple_of(2) {
                "agent://a"
            } else {
                "agent://b"
            },
            contribute_payload(v),
        ));
    }
    entries
}

fn registry() -> ModeRegistry {
    ModeRegistry::build_default(Arc::new(macp_policy::DefaultPolicyEvaluator))
}

fn bench_replay_vs_log_size(c: &mut Criterion) {
    let reg = registry();
    let mut group = c.benchmark_group("replay_from_start");
    for n in [100usize, 1_000, 10_000] {
        let entries = log_of(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &entries, |b, entries| {
            b.iter(|| replay_session(SID, entries, &reg, None).unwrap());
        });
    }
    group.finish();
}

fn bench_kernel_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .build()
        .unwrap();

    fn make_runtime() -> Runtime {
        Runtime::new(
            Arc::new(MemoryBackend),
            Arc::new(SessionRegistry::new()),
            Arc::new(LogStore::new()),
        )
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
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
            // Live-path benches: TTL is computed from this timestamp against
            // wall clock, so it must be "now" (replay benches use the fixed
            // historical timeline instead).
            timestamp_unix_ms: now_ms(),
            payload,
        }
    }

    // Single hot session: serialization within a session is required by
    // RFC-0001 §8.1 — this is the floor.
    c.bench_function("send_single_session", |b| {
        let runtime = make_runtime();
        rt.block_on(async {
            runtime
                .process(
                    &env("SessionStart", "m0", SID, "agent://a", start_payload()),
                    None,
                )
                .await
                .unwrap();
        });
        let mut i = 0u64;
        b.iter(|| {
            i += 1;
            let sender = if i.is_multiple_of(2) {
                "agent://a"
            } else {
                "agent://b"
            };
            let v = if i.is_multiple_of(2) { "x" } else { "y" };
            rt.block_on(async {
                runtime
                    .process(
                        &env(
                            "Contribute",
                            &format!("b{i}"),
                            SID,
                            sender,
                            contribute_payload(v),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
            });
        });
    });

    // Spread across 8 sessions: today this serializes on the global registry
    // write lock; D2's per-session locking should move this number, not the
    // single-session one.
    c.bench_function("send_across_8_sessions", |b| {
        let runtime = make_runtime();
        let sids: Vec<String> = (0..8)
            .map(|i| format!("22222222-1111-4111-8111-11111111111{i}"))
            .collect();
        rt.block_on(async {
            for (i, sid) in sids.iter().enumerate() {
                runtime
                    .process(
                        &env(
                            "SessionStart",
                            &format!("s{i}"),
                            sid,
                            "agent://a",
                            start_payload(),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
            }
        });
        let mut i = 0u64;
        b.iter(|| {
            i += 1;
            let sid = &sids[(i % 8) as usize];
            let sender = if i.is_multiple_of(2) {
                "agent://a"
            } else {
                "agent://b"
            };
            let v = if i.is_multiple_of(2) { "x" } else { "y" };
            rt.block_on(async {
                runtime
                    .process(
                        &env(
                            "Contribute",
                            &format!("b{i}"),
                            sid,
                            sender,
                            contribute_payload(v),
                        ),
                        None,
                    )
                    .await
                    .unwrap();
            });
        });
    });
}

fn bench_kernel_throughput_file_backend(c: &mut Criterion) {
    // The variant that matters for D2: with a real (fsyncing) backend, the
    // global registry write lock serializes every session's append. Compare
    // send_file_single_session vs send_file_across_8_sessions — under
    // per-session locking the cross-session case should approach 8x
    // parallel fsync; under the global lock they are the same.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .build()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn macp_runtime::storage::StorageBackend> =
        Arc::new(macp_runtime::storage::FileBackend::new(dir.path().to_path_buf()).unwrap());
    let runtime = Arc::new(Runtime::new(
        storage,
        Arc::new(SessionRegistry::new()),
        Arc::new(LogStore::new()),
    ));

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
    let env = |message_type: &str, message_id: &str, sid: &str, sender: &str, payload: Vec<u8>| {
        Envelope {
            macp_version: "1.0".into(),
            mode: MODE.into(),
            message_type: message_type.into(),
            message_id: message_id.into(),
            session_id: sid.into(),
            sender: sender.into(),
            timestamp_unix_ms: now_ms(),
            payload,
        }
    };

    let sids: Vec<String> = (0..8)
        .map(|i| format!("33333333-1111-4111-8111-11111111111{i}"))
        .collect();
    rt.block_on(async {
        for (i, sid) in sids.iter().enumerate() {
            runtime
                .process(
                    &env(
                        "SessionStart",
                        &format!("fs{i}"),
                        sid,
                        "agent://a",
                        start_payload(),
                    ),
                    None,
                )
                .await
                .unwrap();
        }
    });

    // 8 concurrent sends to 8 DIFFERENT sessions per iteration.
    let mut i = 0u64;
    c.bench_function("send_file_8_concurrent_across_sessions", |b| {
        b.iter(|| {
            i += 1;
            rt.block_on(async {
                let mut handles = Vec::new();
                for (k, sid) in sids.iter().enumerate() {
                    let runtime = Arc::clone(&runtime);
                    let e = env(
                        "Contribute",
                        &format!("f{i}-{k}"),
                        sid,
                        if (i + k as u64).is_multiple_of(2) {
                            "agent://a"
                        } else {
                            "agent://b"
                        },
                        contribute_payload(if (i + k as u64).is_multiple_of(2) {
                            "x"
                        } else {
                            "y"
                        }),
                    );
                    handles.push(tokio::spawn(async move {
                        runtime.process(&e, None).await.unwrap()
                    }));
                }
                for h in handles {
                    h.await.unwrap();
                }
            });
        });
    });
}

criterion_group!(
    benches,
    bench_replay_vs_log_size,
    bench_kernel_throughput,
    bench_kernel_throughput_file_backend
);
criterion_main!(benches);
