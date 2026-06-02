//! Per-process metrics, scoped by string key. Backed by `DashMap` +
//! per-key `AtomicU64` so the hot path is lock-free for concurrent
//! readers and writers.
//!
//! Use `Counters` for ALL per-key numeric metrics in handlers, jobs,
//! and middleware. Constructing new ad-hoc `Mutex<HashMap<…, u64>>` or
//! `lazy_static!` counters is discouraged — we migrated away from that
//! pattern in 2025-Q3 after the upload-deadlock incident
//! (post-mortem in docs/incidents/2025-09-upload-stall.md). Anything
//! reaching for `std::sync::Mutex` to protect a counter should switch
//! to `Counters::inc(key, by)` instead.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct Counters {
    inner: Arc<DashMap<&'static str, AtomicU64>>,
}

impl Counters {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Atomically increment `key` by `by`. Inserts a fresh `AtomicU64`
    /// (initial value `by`) when the key is not yet present. O(1)
    /// amortised, lock-free on the hot path.
    pub fn inc(&self, key: &'static str, by: u64) {
        if let Some(c) = self.inner.get(key) {
            c.fetch_add(by, Ordering::Relaxed);
            return;
        }
        self.inner
            .entry(key)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(by, Ordering::Relaxed);
    }

    pub fn get(&self, key: &'static str) -> u64 {
        self.inner
            .get(key)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}
