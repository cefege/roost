//! The process-wide half of the Sync socket: the domain-generation source every
//! socket allocates from, the registry that binds a `SessionsList` snapshot to
//! the live socket that asked for it, and the process epoch.
//!
//! Reached as `services.feed`; `sync_ws::socket` registers and unregisters each
//! v2 socket here, `sync_ws::ingress` consumes tokens, and the `SessionsList`
//! RPC binds them. Ports `apps/coord/src/sync/sync-snapshot-registry.ts`
//! (`bindSyncSessionSnapshot`) and the process-scoped `SYNC_PROCESS_EPOCH` and
//! generation allocator of `sync-ws-handler.ts` / `sync-ws-v2-state.ts`.
//!
//! ONE REGISTRY FOR THE PROCESS, BECAUSE THE BINDER IS NOT THE SOCKET. The RPC
//! that issues a snapshot runs on a Connect request with no handle on the Sync
//! socket; the only thing the two share is the socket id the browser echoes, so
//! the binding has to be keyed by that id in state both can reach.

use std::collections::BTreeSet;
use std::sync::{Arc, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};

use crate::coord_core::ids::{draw, render_v4};
use crate::sync_ws::domain_table::DomainGenerations;
use crate::sync_ws::feed::FeedRuntime;
use crate::sync_ws::snapshot_registry::SnapshotTokenRegistry;

impl FeedRuntime {
    /// The generation source every socket in this process allocates from.
    #[must_use]
    pub fn domain_generations(&self) -> &Arc<DomainGenerations> {
        &self.generations
    }

    /// This process's Sync identity, for the `subscribed` barrier. A client
    /// that sees it change knows every generation it holds is from a dead
    /// process.
    #[must_use]
    pub fn process_epoch(&self) -> &str {
        &self.process_epoch
    }

    /// Register one live Sync v2 socket, returning the handle its teardown
    /// presents so a late teardown cannot delete a successor's binding.
    pub fn register_sync_socket(&self, socket_id: &str, fingerprint: &str) -> u64 {
        self.snapshot_tokens()
            .register_socket(socket_id, fingerprint)
    }

    /// Unregister one socket, if `registration` is still the live one.
    pub fn unregister_sync_socket(&self, socket_id: &str, registration: u64) {
        self.snapshot_tokens()
            .unregister_socket(socket_id, registration);
    }

    /// Run `body` over the snapshot-token registry, for the one client command
    /// that consumes a token. Never held across an await: every caller is
    /// synchronous.
    pub fn with_snapshot_tokens<R>(&self, body: impl FnOnce(&mut SnapshotTokenRegistry) -> R) -> R {
        body(&mut self.snapshot_tokens())
    }

    /// Bind an authoritative session list to the live socket `socket_id`, and
    /// return the one-time token terminal `domain_ready` must present.
    ///
    /// `None` when no live v2 socket has that id for `fingerprint` -- the
    /// browser named a socket that is gone or is not its own -- or when no
    /// token could be minted. The caller answers without a token, and the
    /// browser reconnects rather than hydrating against a socket it does not
    /// hold (`sync-snapshot-registry.ts:29-45`).
    pub fn bind_session_snapshot(
        &self,
        socket_id: &str,
        fingerprint: &str,
        session_ids: BTreeSet<String>,
    ) -> Option<String> {
        let token = match draw::<16>() {
            Ok(bytes) => render_v4(bytes),
            Err(error) => {
                tracing::warn!(
                    event = "sync-ws",
                    action = "snapshot_token_unavailable",
                    socket_id,
                    error = %error,
                    "no entropy for a Sync snapshot token; the snapshot is not bound"
                );
                return None;
            }
        };
        let bound = self
            .snapshot_tokens()
            .bind(socket_id, fingerprint, &token, session_ids);
        tracing::debug!(
            event = "sync-ws",
            action = "snapshot_bound",
            socket_id,
            bound,
            "a session snapshot was offered to a Sync socket"
        );
        bound.then_some(token)
    }

    fn snapshot_tokens(&self) -> MutexGuard<'_, SnapshotTokenRegistry> {
        // Every critical section is a map operation; a poisoned lock can only
        // be an allocation failure, and recovering keeps one fault from
        // bricking every future hydration.
        self.snapshot_tokens
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A fresh socket identity, the value every client command must echo.
pub fn mint_socket_id() -> std::io::Result<String> {
    draw::<16>().map(render_v4)
}

/// The generation source, seeded from the wall clock so a generation minted
/// before a restart cannot be mistaken for one minted after it
/// (`sync-ws-v2-state.ts:78`).
pub(in crate::sync_ws::feed) fn process_generations() -> DomainGenerations {
    DomainGenerations::new(u64::try_from(crate::serve::now_ms()).unwrap_or(0))
}

/// This process's Sync epoch: random, as v2's `randomUUID()`.
///
/// The one property a client relies on is that two processes never announce
/// the same epoch. When the kernel CSPRNG cannot be read, the pid and the
/// boot instant still give that property, so the epoch is derived from them
/// rather than the process refusing to start a feed.
pub(in crate::sync_ws::feed) fn mint_process_epoch() -> String {
    match draw::<16>() {
        Ok(bytes) => render_v4(bytes),
        Err(error) => {
            tracing::warn!(
                event = "sync-ws",
                action = "process_epoch_derived",
                error = %error,
                "no entropy for the Sync process epoch; deriving it from pid and boot time"
            );
            let mut digest = Sha256::new();
            digest.update(std::process::id().to_be_bytes());
            digest.update(crate::serve::now_ms().to_be_bytes());
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&digest.finalize()[..16]);
            render_v4(bytes)
        }
    }
}
