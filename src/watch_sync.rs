//! Bounded traversal + event buffering for the `WatchSessions` initial sync.
//!
//! `core.proto` requires the initial sync to carry **every** session currently
//! in the registry, so a truncating cap is not an option. The obvious way to
//! get that set is `SessionRegistry::get_all_sessions`, which deep-clones every
//! `Session` into one `Vec` — and in `watch_sessions` that `Vec` then stays
//! resident for the whole sync, which is paced by how fast the client reads,
//! times up to `MACP_MAX_CONCURRENT_STREAMS` concurrent streams. This module
//! replaces it with a single pass over the registry's *keys* followed by
//! one-at-a-time materialization, so peak resident `Session` clones is the
//! batch size instead of the registry size.
//!
//! Note what was *not* the problem: the response is a lazily polled
//! `async_stream::try_stream!`, so emission is already backpressured by HTTP/2
//! and no unbounded channel is involved on the snapshot path. The memory was in
//! the up-front clone, and that is all this module removes.
//!
//! The pieces live here rather than inline in the generator so the traversal's
//! bound is directly testable: an `async_stream` generator has no seam, and
//! `SessionRegistry` is a concrete struct with neither a trait nor a call
//! counter to instrument.

use crate::registry::SessionRegistry;
use crate::runtime::SessionLifecycleEvent;
use crate::session::Session;
use std::collections::VecDeque;
use tokio::sync::broadcast::error::TryRecvError;

/// How many sessions the initial sync deep-clones at once.
///
/// One. The sync hands each session straight to the client and drops it, so a
/// second resident clone buys nothing. Deliberately a compile-time constant
/// and **not** an environment variable: with one-at-a-time materialization
/// there is no page size to tune, and an operator-supplied knob here would be
/// re-opening the exact bound this module exists to impose.
pub(crate) const INITIAL_SYNC_BATCH: usize = 1;

/// How many lifecycle events the sync will buffer before giving up with
/// `RESOURCE_EXHAUSTED`.
///
/// The sync's emit loop cannot `recv().await` — it has its own output to
/// produce — but it also cannot ignore the bus: the lifecycle bus holds 64
/// events (`runtime::Runtime::new`), so more than that arriving during a slow
/// sync makes the first post-sync `recv()` return `Lagged`, which kills the
/// stream. Draining into a buffer fixes that, but the buffer must be bounded or
/// it simply trades this module's O(N) `Session` bound for an O(events) one.
///
/// 16x the bus capacity: enough that a sync overlapping an ordinary burst
/// survives, small enough to still be a bound. On overflow the stream reports
/// the same `RESOURCE_EXHAUSTED` it already reports for bus lag, because the
/// consequence for the client is identical — it cannot be brought up to date
/// and must reconnect and reconcile via `ListSessions`.
pub(crate) const PENDING_EVENT_LIMIT: usize = 1024;

/// A batch-yielding traversal of the registry for the initial sync.
///
/// Holds **IDs only** — there is no `Session` field. Every session it produces
/// is owned by the returned batch, which the caller is expected to consume and
/// drop before asking for the next one; that is what makes peak residency the
/// batch size rather than N.
///
/// The ID list is taken exactly once, in [`InitialSync::begin`]. It is
/// deliberately not re-derived per batch via `session_ids_after`:
/// `SessionRegistry` is a `RwLock<HashMap>` with no ordered index, so that
/// primitive scans every key on every call, and paging a whole traversal
/// through it would be ⌈N/batch⌉ full map scans — O(N²) at batch size one —
/// which both trades a memory bound for a CPU one and lengthens the very window
/// that makes the lifecycle bus lag.
///
/// One ID pass is not a snapshot; see [`InitialSync::next_batch`].
pub(crate) struct InitialSync {
    ids: std::vec::IntoIter<String>,
    batch: usize,
}

impl InitialSync {
    /// Take the registry-wide ID list. This is the one and only whole-map pass.
    ///
    /// A `batch` of 0 is treated as 1 rather than yielding empty batches
    /// forever.
    pub(crate) async fn begin(registry: &SessionRegistry, batch: usize) -> Self {
        let ids = registry.session_ids().await;
        Self {
            ids: ids.into_iter(),
            batch: batch.max(1),
        }
    }

    /// IDs not yet visited. Meaningful between batches only.
    pub(crate) fn remaining(&self) -> usize {
        self.ids.len()
    }

    /// Whether every ID from the initial pass has been visited.
    pub(crate) fn is_exhausted(&self) -> bool {
        self.remaining() == 0
    }

    /// Materialize the next batch: at most `batch` sessions, each cloned
    /// individually out of the registry.
    ///
    /// An ID whose session has been evicted since the ID pass yields `None`
    /// from `get_session` and is **skipped** — never emitted as a session-less
    /// placeholder. The loop keeps pulling IDs past those, so a short batch
    /// means the ID list ran out, not that a gap was hit; an empty batch means
    /// the traversal is finished. Only terminal sessions are ever evicted (see
    /// `Runtime::evict_stale_sessions`), so a skipped session is one the client
    /// could not have acted on anyway. The resulting anomaly — a lifecycle
    /// event for a session the client never saw created — is documented for
    /// clients in `docs/API.md`, because suppressing it would require
    /// remembering every vanished ID for the stream's lifetime, i.e. another
    /// unbounded set.
    pub(crate) async fn next_batch(&mut self, registry: &SessionRegistry) -> Vec<Session> {
        let mut out = Vec::with_capacity(self.batch.min(self.remaining()));
        for id in self.ids.by_ref() {
            if let Some(session) = registry.get_session(&id).await {
                out.push(session);
                if out.len() >= self.batch {
                    break;
                }
            }
        }
        out
    }
}

/// Why [`drain_lifecycle_events`] stopped short.
///
/// Both variants mean the same thing to the client — this stream can no longer
/// be brought up to date — and both map to `RESOURCE_EXHAUSTED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainError {
    /// The bus dropped events before the sync got to them.
    Lagged(u64),
    /// More than `limit` events arrived while the sync was still emitting.
    Overflow { limit: usize },
}

impl DrainError {
    /// The `RESOURCE_EXHAUSTED` detail message. Kept here (rather than building
    /// a `tonic::Status`) so this module stays transport-free.
    pub(crate) fn message(self) -> String {
        match self {
            Self::Lagged(skipped) => {
                format!("WatchSessions receiver fell behind by {skipped} events")
            }
            Self::Overflow { limit } => format!(
                "WatchSessions initial sync buffered more than {limit} lifecycle events; \
                 reconnect and reconcile with ListSessions"
            ),
        }
    }
}

/// Move every lifecycle event currently sitting on `rx` into `pending`, without
/// awaiting.
///
/// Called between sync batches. `pending.len()` never exceeds `limit`: the
/// limit is checked before each push, and the event that would have exceeded it
/// is dropped along with the stream it belonged to.
pub(crate) fn drain_lifecycle_events(
    rx: &mut tokio::sync::broadcast::Receiver<SessionLifecycleEvent>,
    pending: &mut VecDeque<SessionLifecycleEvent>,
    limit: usize,
) -> Result<(), DrainError> {
    loop {
        match rx.try_recv() {
            Ok(event) => {
                if pending.len() >= limit {
                    return Err(DrainError::Overflow { limit });
                }
                pending.push_back(event);
            }
            // `Closed` is not an error here: the post-sync loop sees it again on
            // its own `recv()` and ends the stream cleanly.
            Err(TryRecvError::Empty | TryRecvError::Closed) => return Ok(()),
            Err(TryRecvError::Lagged(skipped)) => return Err(DrainError::Lagged(skipped)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn sample_session(id: &str) -> Session {
        Session::builder(id, "macp.mode.decision.v1", "agent://alice")
            .ttl_expiry(i64::MAX)
            .ttl_ms(60_000)
            .started_at_unix_ms(1)
            .participants(vec!["agent://alice".into()])
            .mode_version("1.0.0")
            .configuration_version("cfg-1")
            .build()
    }

    async fn registry_with(count: usize) -> SessionRegistry {
        let registry = SessionRegistry::new();
        for i in 0..count {
            let id = format!("sess-{i:04}");
            registry
                .insert_recovered_session(id.clone(), sample_session(&id))
                .await;
        }
        registry
    }

    fn created(session_id: &str) -> SessionLifecycleEvent {
        SessionLifecycleEvent::Created {
            session_id: session_id.to_string(),
        }
    }

    /// Criterion 1 at the traversal level: N registered sessions produce N
    /// distinct sessions, each exactly once, for every batch size.
    #[tokio::test]
    async fn initial_sync_visits_every_session_exactly_once() {
        let registry = registry_with(64).await;
        for batch in [1usize, 7, 64, 200] {
            let mut sync = InitialSync::begin(&registry, batch).await;
            assert_eq!(sync.remaining(), 64);
            let mut seen: HashSet<String> = HashSet::new();
            while !sync.is_exhausted() {
                for session in sync.next_batch(&registry).await {
                    assert!(
                        seen.insert(session.session_id.clone()),
                        "{} emitted twice at batch size {batch}",
                        session.session_id
                    );
                }
            }
            assert_eq!(seen.len(), 64, "batch size {batch} dropped sessions");
        }
    }

    /// Criterion 2. What this asserts precisely: the traversal never hands out
    /// more than `batch` sessions at a time, and the caller holds exactly one
    /// batch (the previous one is dropped at the end of each loop iteration).
    /// That plus `InitialSync` having no `Session` field — it stores only the ID
    /// iterator — is the residency bound. A count of registry calls would not
    /// prove residency, so none is asserted as if it did; for the record the
    /// shape is exactly 1 `session_ids` call plus one `get_session` per ID.
    #[tokio::test]
    async fn initial_sync_batches_never_exceed_the_bound() {
        let registry = registry_with(50).await;
        for batch in [1usize, 3, 50, 500] {
            let mut sync = InitialSync::begin(&registry, batch).await;
            let mut batches = 0usize;
            let mut total = 0usize;
            while !sync.is_exhausted() {
                let chunk = sync.next_batch(&registry).await;
                assert!(
                    chunk.len() <= batch,
                    "batch of {} exceeded the bound {batch}",
                    chunk.len()
                );
                batches += 1;
                total += chunk.len();
                drop(chunk);
            }
            assert_eq!(total, 50);
            assert_eq!(
                batches,
                50usize.div_ceil(batch),
                "batch size {batch} should need exactly ceil(50/batch) batches"
            );
        }
        // The default the handler uses is the tightest possible bound.
        assert_eq!(INITIAL_SYNC_BATCH, 1);
    }

    /// A batch size of 0 must not spin forever yielding empty batches.
    #[tokio::test]
    async fn initial_sync_treats_zero_batch_as_one() {
        let registry = registry_with(3).await;
        let mut sync = InitialSync::begin(&registry, 0).await;
        let mut total = 0;
        while !sync.is_exhausted() {
            let chunk = sync.next_batch(&registry).await;
            assert_eq!(chunk.len(), 1);
            total += chunk.len();
        }
        assert_eq!(total, 3);
    }

    /// Criterion 5. A session evicted after the ID pass but before its turn is
    /// skipped, not materialized as an empty placeholder — the traversal never
    /// produces a session-less entry for the sync to emit.
    #[tokio::test]
    async fn session_evicted_mid_traversal_is_skipped() {
        let registry = registry_with(6).await;
        let mut sync = InitialSync::begin(&registry, 1).await;
        let first = sync.next_batch(&registry).await;
        assert_eq!(first.len(), 1);
        let reached = first[0].session_id.clone();

        // Evict everything the traversal has not reached yet, as the stale
        // session sweep would.
        {
            let mut guard = registry.sessions.write().await;
            guard.retain(|key, _| *key == reached);
        }

        let mut rest = Vec::new();
        while !sync.is_exhausted() {
            rest.extend(sync.next_batch(&registry).await);
        }
        assert!(
            rest.is_empty(),
            "evicted IDs must be skipped, got {} session(s)",
            rest.len()
        );
        // And the traversal terminates rather than stalling on the gap.
        assert!(sync.is_exhausted());
    }

    /// Criterion 3, the surviving half: bursts totalling far more than the bus
    /// capacity are preserved across a slow sync, so the post-sync `recv()`
    /// does not lag and the stream is not killed with `RESOURCE_EXHAUSTED`.
    #[tokio::test]
    async fn drain_preserves_more_events_than_the_bus_holds() {
        // Same capacity as the real lifecycle bus.
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let mut pending = VecDeque::new();

        for burst in 0..4 {
            for i in 0..64 {
                tx.send(created(&format!("s-{burst}-{i}"))).unwrap();
            }
            drain_lifecycle_events(&mut rx, &mut pending, PENDING_EVENT_LIMIT)
                .expect("draining between batches must neither lag nor overflow");
        }
        assert_eq!(pending.len(), 256, "every event must survive the sync");
        assert!(pending.len() <= PENDING_EVENT_LIMIT);

        // The receiver is caught up, so the live loop's first recv() succeeds
        // instead of returning Lagged.
        tx.send(created("live")).unwrap();
        assert!(matches!(
            rx.recv().await,
            Ok(SessionLifecycleEvent::Created { .. })
        ));
    }

    /// Criterion 3, the other half: the buffer's bound is asserted, not
    /// assumed. It stops *at* the limit and reports overflow rather than
    /// growing.
    #[tokio::test]
    async fn drain_never_grows_pending_past_the_limit() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let mut pending = VecDeque::new();
        let limit = 10;

        for i in 0..12 {
            tx.send(created(&format!("s{i}"))).unwrap();
        }
        let err = drain_lifecycle_events(&mut rx, &mut pending, limit)
            .expect_err("12 events into a limit of 10 must overflow");
        assert_eq!(err, DrainError::Overflow { limit });
        assert_eq!(
            pending.len(),
            limit,
            "the buffer must stop at the limit, never grow past it"
        );
        assert!(err.message().contains("10"));
    }

    /// Bus lag during the sync surfaces the existing `RESOURCE_EXHAUSTED`
    /// message rather than silently skipping the gap.
    #[tokio::test]
    async fn drain_reports_bus_lag() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(2);
        for i in 0..5 {
            tx.send(created(&format!("s{i}"))).unwrap();
        }
        let mut pending = VecDeque::new();
        let err =
            drain_lifecycle_events(&mut rx, &mut pending, 100).expect_err("the bus dropped events");
        assert!(matches!(err, DrainError::Lagged(skipped) if skipped == 3));
        assert!(err.message().contains("fell behind by 3"));
    }

    /// An idle bus drains to nothing, and a closed one is not an error — the
    /// post-sync loop is what ends the stream.
    #[tokio::test]
    async fn drain_tolerates_empty_and_closed_bus() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(64);
        let mut pending = VecDeque::new();
        assert!(drain_lifecycle_events(&mut rx, &mut pending, PENDING_EVENT_LIMIT).is_ok());
        assert!(pending.is_empty());

        tx.send(created("s1")).unwrap();
        drop(tx);
        // The buffered event is still delivered, and the close is not an error.
        assert!(drain_lifecycle_events(&mut rx, &mut pending, PENDING_EVENT_LIMIT).is_ok());
        assert_eq!(pending.len(), 1);
        assert!(drain_lifecycle_events(&mut rx, &mut pending, PENDING_EVENT_LIMIT).is_ok());
    }
}
