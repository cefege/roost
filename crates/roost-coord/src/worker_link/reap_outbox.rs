//! The orphan-PTY kills one worker link carries: registered with
//! `LiveOrphanKills` when the link claims its generation, and detached when it
//! ends.
//!
//! Owned by `worker_link::link_session`, which drains it once the generation is
//! routable and sends each kill through `workers::send::reap_orphan_pty`. The
//! delivery half of v2's `dispatchSnapshotOrphanReaps`
//! (`apps/coord/src/events/event-transaction.ts:329-348`).

use std::sync::{Arc, Mutex, PoisonError};

use roost_protocol::wire::WorkerFp;

use crate::terminal_screen::orphan_kills::{LiveOrphanKills, PendingKill};

/// This link's kill outbox, and the registration that owns it.
///
/// Attach at the hello, detach at the end, and the detach is not tidiness: a
/// link that ended while still attached would collect kills into a channel
/// nobody reads, and a reconnecting worker would find it empty and believe it
/// owed nothing.
#[derive(Debug)]
pub(super) struct ReapOutbox {
    worker_fp: WorkerFp,
    outbox: Arc<Mutex<Vec<PendingKill>>>,
    kills: Arc<LiveOrphanKills>,
}

impl ReapOutbox {
    /// Register this link and take everything the worker is already owed.
    ///
    /// The owed kills are MOVED into this outbox rather than copied, so one that
    /// is delivered is delivered once.
    pub(super) fn attach(kills: &Arc<LiveOrphanKills>, worker_fp: WorkerFp) -> Self {
        let outbox = Arc::new(Mutex::new(Vec::new()));
        let owed = kills.attach(&worker_fp, Arc::clone(&outbox));
        if !owed.is_empty() {
            tracing::info!(%worker_fp, owed = owed.len(), "worker link: took the reaps a reconnect owed");
        }
        outbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(owed);
        Self {
            worker_fp,
            outbox,
            kills: Arc::clone(kills),
        }
    }

    /// Take the kills handed over since the last drain.
    pub(super) fn take_pending(&self) -> Vec<PendingKill> {
        std::mem::take(&mut *self.outbox.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for ReapOutbox {
    /// In one place, so no exit path can forget it; identity-stamped, so a
    /// superseded link cannot detach its replacement.
    fn drop(&mut self) {
        self.kills.detach(&self.worker_fp, &self.outbox);
        tracing::debug!(worker_fp = %self.worker_fp, "worker link: reap outbox detached");
    }
}
