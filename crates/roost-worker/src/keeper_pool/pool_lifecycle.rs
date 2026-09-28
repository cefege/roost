//! The pool's connection lifecycle: the keeper going away, every channel it
//! drove ending with it, the death hook the reconcile gate registers, and the
//! reconnect a later reconcile performs. Ports the close handler and
//! `setOnKeeperDeath` of `apps/worker/src/keeper/keeper-pool-lifecycle.ts` /
//! `multiplexed-client.ts`. Called by the dispatch loop (a closed connection),
//! by `KeeperPool::request` (a failed write), and by `runtime::session_reconcile`.
//!
//! A LOST KEEPER CONNECTION IS A DEAD KEEPER, and every channel it drove is
//! told `on_exit(None)`, as v2's socket-close handler does: the sessions close,
//! and the death hook drives the reconcile that respawns what the coordinator
//! still lists open under a fresh keeper.

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use roost_keeper::client::KeeperClient;

use super::pool::KeeperPool;

/// What the pool calls, once per lost connection, after its state is cleared.
pub type KeeperDeathHook = Arc<dyn Fn() + Send + Sync>;

impl KeeperPool {
    /// Register the one death hook (v2 `setOnKeeperDeath`). A second
    /// registration replaces the first, as v2's setter does.
    pub fn set_on_keeper_death(&self, hook: KeeperDeathHook) {
        *self
            .death_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// The keeper is gone: end every channel this worker drives, once, then
    /// fire the death hook.
    pub fn keeper_lost(&self, reason: String) {
        if !self.connected.swap(false, Ordering::SeqCst) {
            // Exactly once: a second report is the same event seen twice, and a
            // second ending for one channel is the defect this guards.
            tracing::debug!(%reason, "the keeper connection was already reported gone");
            return;
        }
        // Every written batch is unknowable now: its result will never arrive.
        self.pending_inputs.settle_all_disconnected();
        let channels = self.channels.drain();
        tracing::error!(
            %reason,
            channels = channels.len(),
            "keeper.died: the keeper connection is gone; every channel it drove ends"
        );
        for channel in channels {
            // A channel that already ended was told so by its own exit frame;
            // telling it again here is the double ending this pool must not do.
            if !channel.has_exited() {
                channel.output().on_exit(None);
            }
        }
        let hook = self
            .death_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match hook {
            Some(hook) => hook(),
            None => {
                tracing::warn!("keeper.died with no reconcile registered to recover its sessions")
            }
        }
    }

    /// Drive a new keeper connection from now on (v2 `ensure()` after a death):
    /// the old client is dropped, and the pool is connected again with an
    /// empty channel table.
    pub fn reconnect(&self, client: KeeperClient) {
        self.keeper.replace(client);
        self.channels.drain();
        self.connected.store(true, Ordering::SeqCst);
        tracing::info!("keeper: the pool is driving a new keeper connection");
    }
}
