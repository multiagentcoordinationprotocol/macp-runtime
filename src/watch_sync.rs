//! Bounded traversal + event buffering for the `WatchSessions` initial sync.
//!
//! `core.proto` requires the initial sync to carry **every** session currently
//! in the registry, so a truncating cap is not an option. The obvious way to
//! get that set is `SessionRegistry::get_all_sessions`, which deep-clones every
//! `Session` into one `Vec` — and in `watch_sessions` that `Vec` then stays
//! resident for the whole sync, which is paced by how fast the client reads,
//! times up to `MACP_MAX_CONCURRENT_STREAMS` concurrent streams. This module
//! replaces it with a snapshot of the registry's shared **handles**
//! (`Arc<Mutex<Session>>`), locked and cloned one at a time, so peak resident
//! `Session` clones is one instead of the registry size.
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

use crate::registry::{SessionRegistry, SharedSession};
use crate::runtime::SessionLifecycleEvent;
use crate::session::Session;
use std::collections::VecDeque;
use tokio::sync::broadcast::error::TryRecvError;

/// How many lifecycle events the sync will buffer before giving up with
/// `RESOURCE_EXHAUSTED`.
///
/// The sync's emit loop cannot `recv().await` — it has its own output to
/// produce — but it also cannot ignore the bus: the lifecycle bus holds 64
/// events (`runtime::Runtime::new`), so more than that arriving during a slow
/// sync makes the first post-sync `recv()` return `Lagged`, which kills the
/// stream. Draining into a buffer fixes that, but the buffer must be bounded or
/// it simply trades this module's O(1) `Session` bound for an O(events) one.
///
/// 16x the bus capacity: enough that a sync overlapping an ordinary burst
/// survives, small enough to still be a bound. On overflow the stream reports
/// the same `RESOURCE_EXHAUSTED` it already reports for bus lag, because the
/// consequence for the client is identical — it cannot be brought up to date
/// and must reconnect and reconcile via `ListSessions`.
pub(crate) const PENDING_EVENT_LIMIT: usize = 1024;

/// A one-session-at-a-time traversal of the registry for the initial sync.
///
/// Holds **handles only** — there is no `Session` field, so the residency bound
/// is structural rather than asserted. [`InitialSync::next_session`] hands out
/// a single owned `Session`, which the caller emits and drops before asking for
/// the next; peak residency is therefore one clone regardless of registry size.
///
/// The handle snapshot is taken exactly once, in [`InitialSync::begin`], and it
/// really is a snapshot of the session *set*: an `Arc` keeps its session
/// reachable after the registry entry is gone, so eviction mid-traversal can
/// neither skip a session nor produce a session-less entry. Pinning costs one
/// pointer per session and blocks nothing — `Runtime::evict_stale_sessions` and
/// `Runtime::gc_disk_sessions` both take the map write lock and remove
/// unconditionally, along with the log cache and stream bus; only the evicted
/// `Session`'s deallocation waits for this stream to pass it.
///
/// Deliberately not paged through `SessionRegistry::session_ids_after`:
/// `SessionRegistry` is a `RwLock<HashMap>` with no ordered index, so that
/// primitive scans every key on every call, and paging a whole traversal
/// through it would be N full map scans — O(N²) — which both trades a memory
/// bound for a CPU one and lengthens the very window that makes the lifecycle
/// bus lag.
pub(crate) struct InitialSync {
    handles: std::vec::IntoIter<SharedSession>,
}

impl InitialSync {
    /// Take the registry-wide handle snapshot. This is the one and only
    /// whole-map pass.
    pub(crate) async fn begin(registry: &SessionRegistry) -> Self {
        Self {
            handles: registry.shared_sessions().await.into_iter(),
        }
    }

    /// Sessions not yet visited. Meaningful between sessions only.
    pub(crate) fn remaining(&self) -> usize {
        self.handles.len()
    }

    /// Lock and clone the next snapshotted session, or `None` once the
    /// traversal is finished.
    ///
    /// Every handle yields a session — the snapshot owns them — so `None` means
    /// exhausted, never "this one vanished".
    pub(crate) async fn next_session(&mut self) -> Option<Session> {
        let handle = self.handles.next()?;
        let session = handle.lock().await;
        Some(session.clone())
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
/// Called between sync sessions. `pending.len()` never exceeds `limit`: the
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
    /// distinct sessions, each exactly once, and then the traversal ends.
    ///
    /// Criterion 2 is discharged by construction rather than by assertion:
    /// `InitialSync` has no `Session` field, and `next_session` returns one
    /// owned `Session` which this loop drops each iteration, so peak residency
    /// is one clone. A count of registry calls would not prove that; for the
    /// record the shape is exactly one `shared_sessions` call plus one mutex
    /// lock per session.
    #[tokio::test]
    async fn initial_sync_visits_every_session_exactly_once() {
        let registry = registry_with(64).await;
        let mut sync = InitialSync::begin(&registry).await;
        assert_eq!(sync.remaining(), 64);

        let mut seen: HashSet<String> = HashSet::new();
        while let Some(session) = sync.next_session().await {
            assert!(
                seen.insert(session.session_id.clone()),
                "{} emitted twice",
                session.session_id
            );
        }
        assert_eq!(seen.len(), 64, "the traversal dropped sessions");
        assert_eq!(sync.remaining(), 0);
        assert!(sync.next_session().await.is_none(), "must stay exhausted");
    }

    /// An empty registry yields an empty sync rather than stalling.
    #[tokio::test]
    async fn initial_sync_of_an_empty_registry_is_empty() {
        let registry = registry_with(0).await;
        let mut sync = InitialSync::begin(&registry).await;
        assert_eq!(sync.remaining(), 0);
        assert!(sync.next_session().await.is_none());
    }

    /// The snapshot is a snapshot. Evicting every unreached session mid-way, as
    /// the stale-session sweep does, changes nothing the traversal emits: the
    /// handles keep those sessions reachable, so there is no skipped session and
    /// no session-less placeholder to report to the client.
    #[tokio::test]
    async fn eviction_mid_traversal_does_not_perturb_the_sync() {
        let registry = registry_with(6).await;
        let mut sync = InitialSync::begin(&registry).await;
        let first = sync.next_session().await.expect("first session");

        // Drop every registry entry, including the ones not yet reached.
        registry.sessions.write().await.clear();
        assert!(registry.get_session(&first.session_id).await.is_none());

        let mut seen: HashSet<String> = HashSet::new();
        seen.insert(first.session_id);
        while let Some(session) = sync.next_session().await {
            assert!(seen.insert(session.session_id.clone()));
        }
        assert_eq!(
            seen.len(),
            6,
            "the handle snapshot must survive registry eviction"
        );
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
                .expect("draining between sessions must neither lag nor overflow");
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
