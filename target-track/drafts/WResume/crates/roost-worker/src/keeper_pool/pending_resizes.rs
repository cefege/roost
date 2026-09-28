//! The keeper resizes this pool has written and not yet heard back about:
//! the in-flight table v2 keeps as `pool.pendingResizes`
//! (`apps/worker/src/keeper/keeper-pool-io.ts` `resizeCommand` /
//! `settlePendingResize`). `KeeperPool::resize` registers a sequence for the
//! life of its round trip; `terminal_pipeline` reads the start instants as
//! `KEEPER_RESIZE_PENDING` evidence.
//!
//! A sequence already in flight is refused before anything is written, as v2
//! refuses it: a second writer of the same sequence cannot prove its fate.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Resizes in flight, by channel and sequence, with the instant each started.
#[derive(Debug, Default)]
pub(crate) struct PendingResizes {
    inner: Mutex<HashMap<(u16, u64), Instant>>,
}

/// One registered resize; dropping it settles the entry, whatever the outcome.
#[derive(Debug)]
pub(crate) struct PendingResize<'table> {
    table: &'table PendingResizes,
    key: (u16, u64),
}

impl PendingResizes {
    /// Register `seq` on `channel_id`, or `None` when it is already in flight.
    pub(crate) fn begin(&self, channel_id: u16, seq: u64) -> Option<PendingResize<'_>> {
        let key = (channel_id, seq);
        let mut pending = self.lock();
        if pending.contains_key(&key) {
            return None;
        }
        pending.insert(key, Instant::now());
        Some(PendingResize { table: self, key })
    }

    /// When each resize still in flight on `channel_id` started.
    pub(crate) fn started(&self, channel_id: u16) -> Vec<Instant> {
        self.lock()
            .iter()
            .filter(|((channel, _), _)| *channel == channel_id)
            .map(|(_, started)| *started)
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<(u16, u64), Instant>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for PendingResize<'_> {
    fn drop(&mut self) {
        self.table.lock().remove(&self.key);
    }
}
