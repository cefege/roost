//! Every open Sync socket in the process, by the fingerprint that opened it:
//! what a key revocation closes `4001`, and what a worker delete narrows.
//!
//! Held once on `FeedRuntime` (`services.feed.open_sockets()`); a socket
//! registers in `sync_ws::socket_open::open_socket` and leaves in
//! `release_socket`. The revocation hooks are `auth::key_revocation` and the
//! worker delete in `workers::rpc`. Ports `sockets`, `closeForFingerprint` and
//! `removeWorkerFromResourceIndexes` of `apps/coord/src/sync/sync-ws-handler.ts`.
//!
//! WEAK, SO THE REGISTRY NEVER KEEPS A SOCKET ALIVE. The socket task owns its
//! link; an entry here is only a way to reach it while it lives, and a link
//! whose task already ended is simply skipped.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use crate::sync_ws::driver::{SyncLink, now_ms};
use crate::sync_ws::socket_open::REVOKED;

/// The open Sync sockets of one coordinator process.
#[derive(Debug, Default)]
pub struct OpenSyncSockets {
    inner: Mutex<Registered>,
}

#[derive(Debug, Default)]
struct Registered {
    next: u64,
    links: BTreeMap<u64, (String, Weak<SyncLink>)>,
}

impl OpenSyncSockets {
    /// Record one open socket; the handle is what `unregister` takes.
    pub fn register(&self, fingerprint: &str, link: &Arc<SyncLink>) -> u64 {
        let mut registered = self.lock();
        registered.next += 1;
        let handle = registered.next;
        registered
            .links
            .insert(handle, (fingerprint.to_owned(), Arc::downgrade(link)));
        handle
    }

    /// Forget one socket at its release.
    pub fn unregister(&self, handle: u64) {
        self.lock().links.remove(&handle);
    }

    /// Close every open socket `fingerprint` holds with `4001 revoked`
    /// (`sync-ws-handler.ts:354-361`).
    pub fn close_for_fingerprint(&self, fingerprint: &str) {
        let links = self.links_for(|owner| owner == fingerprint);
        for link in &links {
            link.deliver_with(|state| {
                state.decide_close(REVOKED, "revoked", "revocation", now_ms());
                None
            });
        }
        tracing::info!(
            event = "sync-ws",
            action = "revoked_sockets_closed",
            caller_fp = %fingerprint,
            sockets = links.len(),
            "open sync sockets closed for a revoked key"
        );
    }

    /// Drop a deleted worker from every open socket's resource index before
    /// its removal is published (`sync-ws-handler.ts:349-353`).
    pub fn remove_worker_from_resource_indexes(&self, worker_fp: &str) {
        for link in self.links_for(|_| true) {
            link.lock()
                .index
                .worker_fps
                .retain(|fp| fp.as_str() != worker_fp);
        }
        tracing::info!(
            event = "sync-ws",
            action = "worker_scope_removed",
            worker_fp,
            "a deleted worker left every sync resource index"
        );
    }

    /// The live links whose opener `matches`, collected so no link is locked
    /// while the registry is.
    fn links_for(&self, matches: impl Fn(&str) -> bool) -> Vec<Arc<SyncLink>> {
        self.lock()
            .links
            .values()
            .filter(|(owner, _)| matches(owner))
            .filter_map(|(_, link)| link.upgrade())
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Registered> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
