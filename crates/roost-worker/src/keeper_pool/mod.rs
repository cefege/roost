//! The multiplexed keeper connection and the per-channel entry it holds.
//! `session::spawn` opens channels through it, `session::sinks::ChannelBinding`
//! is what it delivers their bytes into, and the coordinator link reads the
//! live bindings to reconcile a survivor. Depends on `roost_keeper::client` for
//! the connection and `roost_keeper::frames::ChannelBinding` for the wire pair —
//! and on nothing here.
//!
//! It does NOT own the connection's own lifecycle. `runtime::keeper_boot`
//! decides whether this worker adopts a survivor or starts a keeper, and a pool
//! that decided for itself would be a second answer to "is it safe to replace
//! the thing holding this machine's terminals".

mod channels;
mod dispatch;
mod error;
mod pool;
mod spawn_spec;

pub use error::PoolError;
pub use pool::{DISPATCH_IDLE, KeeperPool, Spawned};
pub use spawn_spec::{PtyCommand, pty_command};

use std::sync::Arc;

use roost_keeper::frames::ChannelBinding as KeeperChannelBinding;

use crate::session::sinks::ChannelBinding;

/// One channel the pool is driving, and what its output is delivered into.
///
/// The three fields are the whole entry, and each answers a different question:
/// the wire pair is what the keeper is told it owns, the output binding is where
/// its bytes go, and `exited` is whether the child has ended. The last one is
/// not derivable from the other two — a killed child still has a pid on the
/// keeper until the keeper reaps it — and getting it wrong announces a dead PTY
/// as live to the next process that adopts this keeper.
pub struct PoolChannel {
    binding: KeeperChannelBinding,
    output: Arc<dyn ChannelBinding>,
    exited: bool,
}

impl PoolChannel {
    /// A channel whose child is running.
    pub fn live(binding: KeeperChannelBinding, output: Arc<dyn ChannelBinding>) -> Self {
        Self {
            binding,
            output,
            exited: false,
        }
    }

    /// The wire pair this channel is announced by.
    pub fn binding(&self) -> KeeperChannelBinding {
        self.binding.clone()
    }

    /// Where this channel's output goes.
    pub fn output(&self) -> &Arc<dyn ChannelBinding> {
        &self.output
    }

    /// Whether the child has ended.
    pub fn has_exited(&self) -> bool {
        self.exited
    }

    /// Record that the child ended. Idempotent, because a session that closes
    /// twice is a bug the caller is told about once.
    pub fn mark_exited(&mut self) {
        self.exited = true;
    }
}

impl std::fmt::Debug for PoolChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PoolChannel")
            .field("binding", &self.binding)
            .field("exited", &self.exited)
            .finish_non_exhaustive()
    }
}

/// The channels a hello announces as still live, in the order given.
///
/// An exited channel is omitted rather than announced with a flag, because the
/// keeper's hello has no such flag: a binding it is told about is a channel it
/// will refuse to reap, and announcing a dead one here is how a survivor's
/// channel list grows a process nothing owns.
pub fn live_bindings<'a>(
    channels: impl IntoIterator<Item = &'a PoolChannel>,
) -> Vec<KeeperChannelBinding> {
    channels
        .into_iter()
        .filter(|channel| !channel.has_exited())
        .map(PoolChannel::binding)
        .collect()
}
