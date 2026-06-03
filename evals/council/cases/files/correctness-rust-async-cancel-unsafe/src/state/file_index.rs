//! Append-only file-id allocator. Lock-free read path, single-writer
//! write path guarded by a `tokio::sync::Mutex` at the caller.
//!
//! ## Invariant
//!
//! Every `reserve_id()` must be followed by **exactly one** of
//! `commit(id)` or `abort(id)` before the guard is dropped. Reserved
//! IDs are removed from the free pool the moment `reserve_id` returns
//! — if the caller forgets to commit/abort, that ID is leaked for the
//! lifetime of the process. Tracked by issue INF-4129 (last incident:
//! 6.2k orphan reservations after a deploy-mid-request, 2026-04).

use std::collections::HashSet;

pub struct FileIndex {
    next_id: u64,
    pending: HashSet<u64>,
    committed: HashSet<u64>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            pending: HashSet::new(),
            committed: HashSet::new(),
        }
    }

    /// Reserve the next free ID. The caller must call either `commit`
    /// or `abort` with this exact ID before dropping the guard, or the
    /// ID is leaked.
    pub fn reserve_id(&mut self, _size_bytes: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pending.insert(id);
        id
    }

    pub fn commit(&mut self, id: u64) {
        debug_assert!(self.pending.remove(&id), "commit of unreserved id {id}");
        self.committed.insert(id);
    }

    pub fn abort(&mut self, id: u64) {
        debug_assert!(self.pending.remove(&id), "abort of unreserved id {id}");
    }
}
