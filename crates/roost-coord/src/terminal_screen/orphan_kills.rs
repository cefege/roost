//! Where a force-closed PTY's kill goes: to the link for THAT worker when it is
//! connected, and into a record that worker drains when it is not.
//!
//! Owned by the terminal domain, beside the [`OrphanPtyKill`] seam it
//! implements. `connection.rs` is the only thing that registers a link, because
//! the link is what it owns.
//!
//! **KEYED BY FINGERPRINT, NOT BY SOCKET, AND THAT IS THE WHOLE DESIGN.** The
//! reap runs when a snapshot commits, and the worker that owns the dead PTY is
//! by definition OFFLINE at that moment — that is why the PTY was force-closed.
//! So the kill has to WAIT FOR RECONNECT, and binding it to "the socket that
//! happened to commit the snapshot" is a kill aimed at the wrong link: the call
//! site says so outright, *"a socket that may not be the one that announced the
//! session"*. A per-fingerprint registry is the shape that mechanism implies,
//! and it is also the only one that can hold two workers' links at once.
//!
//! **NOT A SECOND `PendingPublicationStore`, and the reason is duplicate
//! semantics.** That store is keyed `(worker_fp, client_seq)` and holds a
//! retained event publication awaiting a connection-generation race; this is
//! keyed by worker and holds a browser command awaiting the worker's return.
//! Re-publishing an event is a duplicate the unique index has to catch; re-sending
//! a kill is idempotent. Putting an idempotent payload and an at-least-once one
//! behind one bound and one drain would size that bound for whichever is less
//! safe, so they stay apart.
//!
//! BOUNDED PER WORKER, because this is on the commit path. At the cap the OLDEST
//! kill is dropped: the newest is the one whose session row most recently became
//! wrong.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use roost_protocol::wire::WorkerFp;

use crate::terminal_screen::live_effects::OrphanPtyKill;

/// How many un-delivered kills one worker may accumulate.
pub const MAX_RECORDED_KILLS_PER_WORKER: usize = 1_024;

/// One kill owed to a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingKill {
    /// The worker that owns the dead PTY.
    pub worker_fp: WorkerFp,
    /// The session whose PTY has to die.
    pub session_id: String,
}

/// The link a connected worker's kills travel on.
type Outbox = Arc<Mutex<Vec<PendingKill>>>;

/// The kills this coordinator owes, and the links that can carry them.
#[derive(Debug, Default)]
pub struct LiveOrphanKills {
    /// One outbox per CONNECTED worker. A worker absent here is owed kills.
    links: Mutex<HashMap<WorkerFp, Outbox>>,
    /// Kills owed per worker, bounded each.
    owed: Mutex<HashMap<WorkerFp, VecDeque<PendingKill>>>,
}

impl LiveOrphanKills {
    /// Nothing owed, nothing connected.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a worker's link and hand it everything already owed to it.
    ///
    /// The drain is the other half of `kill`: a worker that was offline when its
    /// PTY died gets its kills at reconnect rather than never. Returns what was
    /// delivered so the caller can report a non-empty drain.
    pub fn attach(&self, worker_fp: &WorkerFp, outbox: Outbox) -> Vec<PendingKill> {
        if let Ok(mut links) = self.links.lock() {
            links.insert(worker_fp.clone(), outbox);
        }
        self.take_owed(worker_fp)
    }

    /// Forget a worker's link, so its later kills are recorded again.
    ///
    /// Called when a link ends. Without it a dead link would keep collecting
    /// kills into an outbox nobody reads, and a reconnecting worker would find
    /// it empty and believe it owed nothing. Only THIS link's outbox is removed:
    /// a superseded socket closes after its replacement attached, and removing
    /// by fingerprint alone would detach the replacement.
    pub fn detach(&self, worker_fp: &WorkerFp, outbox: &Outbox) {
        if let Ok(mut links) = self.links.lock()
            && links
                .get(worker_fp)
                .is_some_and(|attached| Arc::ptr_eq(attached, outbox))
        {
            links.remove(worker_fp);
        }
    }

    /// How many workers have a live link.
    pub fn connected_workers(&self) -> usize {
        self.links.lock().map_or(0, |links| links.len())
    }

    /// Kills still owed, across every worker.
    pub fn owed_count(&self) -> usize {
        self.owed
            .lock()
            .map_or(0, |owed| owed.values().map(VecDeque::len).sum())
    }

    /// The kills owed to one worker, removed from the record.
    fn take_owed(&self, worker_fp: &WorkerFp) -> Vec<PendingKill> {
        let Ok(mut owed) = self.owed.lock() else {
            return Vec::new();
        };
        owed.remove(worker_fp)
            .map_or_else(Vec::new, |mut kills| kills.drain(..).collect())
    }

    /// Hand a kill to that worker's link, or record it.
    fn deliver_or_record(&self, kill: PendingKill) {
        let outbox = self
            .links
            .lock()
            .ok()
            .and_then(|links| links.get(&kill.worker_fp).cloned());
        if let Some(outbox) = outbox {
            outbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(kill);
            return;
        }
        let Ok(mut owed) = self.owed.lock() else {
            return;
        };
        let queue = owed.entry(kill.worker_fp.clone()).or_default();
        if queue.len() >= MAX_RECORDED_KILLS_PER_WORKER {
            queue.pop_front();
        }
        queue.push_back(kill);
    }
}

impl OrphanPtyKill for LiveOrphanKills {
    fn kill(&self, worker_fp: &WorkerFp, session_id: &str) {
        self.deliver_or_record(PendingKill {
            worker_fp: worker_fp.clone(),
            session_id: session_id.to_owned(),
        });
    }
}
