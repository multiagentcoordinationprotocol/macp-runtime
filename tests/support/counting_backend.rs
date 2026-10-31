//! A storage backend that counts trace-anchor loads and delegates every
//! call to an inner backend (shared by the otel anchor tests).

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use macp_runtime::log_store::LogEntry;
use macp_runtime::macp_core::session::Session;
use macp_runtime::storage::StorageBackend;

/// A backend that counts trace-anchor loads, records the order of session
/// snapshot and anchor writes, and delegates everything; or, built with
/// [`Counting::failing_anchors`], fails every trace-anchor store and load.
pub struct Counting {
    inner: Arc<dyn StorageBackend>,
    anchor_loads: AtomicUsize,
    anchor_stores: AtomicUsize,
    fail_anchors: bool,
    /// `save:<id>` / `anchor:<id>`, in call order.
    writes: Mutex<Vec<String>>,
}

impl Counting {
    pub fn new(inner: Arc<dyn StorageBackend>) -> Arc<Self> {
        Self::build(inner, false)
    }

    /// Every `store_trace_anchor` and `load_trace_anchor` returns `Err`.
    pub fn failing_anchors(inner: Arc<dyn StorageBackend>) -> Arc<Self> {
        Self::build(inner, true)
    }

    fn build(inner: Arc<dyn StorageBackend>, fail_anchors: bool) -> Arc<Self> {
        Arc::new(Self {
            inner,
            anchor_loads: AtomicUsize::new(0),
            anchor_stores: AtomicUsize::new(0),
            fail_anchors,
            writes: Mutex::new(Vec::new()),
        })
    }

    pub fn stores(&self) -> usize {
        self.anchor_stores.load(Ordering::SeqCst)
    }

    pub fn writes(&self) -> Vec<String> {
        self.writes.lock().unwrap().clone()
    }

    fn failure() -> std::io::Error {
        std::io::Error::other("injected trace-anchor failure")
    }

    pub fn loads(&self) -> usize {
        self.anchor_loads.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl StorageBackend for Counting {
    async fn save_session(&self, session: &Session) -> std::io::Result<()> {
        self.writes
            .lock()
            .unwrap()
            .push(format!("save:{}", session.session_id));
        self.inner.save_session(session).await
    }
    async fn load_session(&self, id: &str) -> std::io::Result<Option<Session>> {
        self.inner.load_session(id).await
    }
    async fn load_all_sessions(&self) -> std::io::Result<Vec<Session>> {
        self.inner.load_all_sessions().await
    }
    async fn delete_session(&self, id: &str) -> std::io::Result<()> {
        self.inner.delete_session(id).await
    }
    async fn list_session_ids(&self) -> std::io::Result<Vec<String>> {
        self.inner.list_session_ids().await
    }
    async fn append_log_entry(&self, id: &str, entry: &LogEntry) -> std::io::Result<()> {
        self.inner.append_log_entry(id, entry).await
    }
    async fn load_log(&self, id: &str) -> std::io::Result<Vec<LogEntry>> {
        self.inner.load_log(id).await
    }
    async fn create_session_storage(&self, id: &str) -> std::io::Result<()> {
        self.inner.create_session_storage(id).await
    }
    async fn replace_log(&self, id: &str, entries: &[LogEntry]) -> std::io::Result<()> {
        self.inner.replace_log(id, entries).await
    }
    async fn store_trace_anchor(&self, id: &str, anchor: &[u8]) -> std::io::Result<()> {
        self.anchor_stores.fetch_add(1, Ordering::SeqCst);
        self.writes.lock().unwrap().push(format!("anchor:{id}"));
        if self.fail_anchors {
            return Err(Self::failure());
        }
        self.inner.store_trace_anchor(id, anchor).await
    }
    async fn load_trace_anchor(&self, id: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.anchor_loads.fetch_add(1, Ordering::SeqCst);
        if self.fail_anchors {
            return Err(Self::failure());
        }
        self.inner.load_trace_anchor(id).await
    }
}
