//! The pool's channel table: which ids exist, what their output is delivered
//! into, and which of them have ended. `pool::KeeperPool` is its only caller,
//! and the boot reconcile reads the pool's view through it. Depends on
//! `super::PoolChannel` and `session::sinks::ChannelBinding` — nothing here.
//!
//! TWO TABLES, BECAUSE A SPAWN HAS A GAP. A binding is registered BEFORE its
//! spawn frame is written, so the first PTY bytes after the acknowledgement
//! have somewhere to go; and the acknowledgement is what carries the pid, which
//! is half of the wire pair `PoolChannel` announces. So an in-flight spawn holds
//! an output binding with no channel yet, and it is a separate table precisely
//! because it is NOT announceable: a channel the keeper has not acknowledged is
//! not a channel the keeper will reap, and announcing it would pin a PTY that
//! was never opened.
//!
//! NO LOCK IS EVER HELD ACROSS A BINDING CALL. Every method here returns owned
//! values — a cloned `Arc`, a `PoolChannel`, a vector — and the caller invokes
//! the binding after the lock is gone. A session that blocks inside `on_output`
//! must not be able to stall the dispatcher for every other session.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_keeper::frames::ChannelBinding as KeeperChannelBinding;

use super::PoolChannel;
use super::error::PoolError;
use crate::session::sinks::ChannelBinding;

/// The pool's channels. Cheap to clone the handle; the table is shared.
///
/// No `Debug`: a binding is an opaque trait object, so a derived formatter
/// would print session internals into a log line. [`super::KeeperPool`]'s own
/// `Debug` reports the counts an operator actually reads.
#[derive(Default)]
pub(crate) struct ChannelRegistry {
    inner: Mutex<Table>,
}

#[derive(Default)]
struct Table {
    /// Channels whose spawn is in flight: bound output, no acknowledged PTY.
    spawning: BTreeMap<u16, Arc<dyn ChannelBinding>>,
    /// Channels the keeper has acknowledged, in id order.
    acknowledged: BTreeMap<u16, PoolChannel>,
}

impl ChannelRegistry {
    /// Bind a channel's output before its spawn frame is written.
    ///
    /// Returns whether it displaced an acknowledged channel, which happens only
    /// when a caller deliberately respawns an id: the keeper replaces the PTY
    /// in that case, so keeping the old entry would keep announcing a pid that
    /// is no longer the channel's.
    pub(crate) fn begin_spawn(&self, channel_id: u16, output: Arc<dyn ChannelBinding>) -> bool {
        let mut table = self.lock();
        let displaced = table.acknowledged.remove(&channel_id).is_some();
        table.spawning.insert(channel_id, output);
        displaced
    }

    /// Turn an in-flight spawn into a channel once the keeper acknowledges it.
    pub(crate) fn finish_spawn(&self, channel_id: u16, pid: u32) -> Result<(), PoolError> {
        let mut table = self.lock();
        let output = table
            .spawning
            .remove(&channel_id)
            .ok_or(PoolError::Untracked(channel_id))?;
        table.acknowledged.insert(
            channel_id,
            PoolChannel::live(KeeperChannelBinding { channel_id, pid }, output),
        );
        Ok(())
    }

    /// Drop an in-flight spawn the keeper refused.
    ///
    /// The whole point: a refused spawn has no PTY, so leaving its binding
    /// registered would route a LATER frame for a recycled id into a session
    /// that was never opened, and would announce a channel nothing owns.
    pub(crate) fn abort_spawn(&self, channel_id: u16) -> bool {
        self.lock().spawning.remove(&channel_id).is_some()
    }

    /// Register a channel this worker did not spawn but now drives.
    ///
    /// The pid is the keeper's own, so the pair announced in a hello is the
    /// keeper's truth rather than this worker's recollection of it.
    pub(crate) fn adopt(&self, channel_id: u16, pid: u32, output: Arc<dyn ChannelBinding>) -> bool {
        let mut table = self.lock();
        table.spawning.remove(&channel_id);
        table
            .acknowledged
            .insert(
                channel_id,
                PoolChannel::live(KeeperChannelBinding { channel_id, pid }, output),
            )
            .is_some()
    }

    /// Where a channel's output goes, or `None` when it is unknown or ended.
    ///
    /// The ended case is why this is one question: output that arrives after the
    /// exit frame has nowhere legitimate to go, and a session that has already
    /// been closed must not be fed.
    pub(crate) fn output_for(&self, channel_id: u16) -> Option<Arc<dyn ChannelBinding>> {
        let table = self.lock();
        table
            .acknowledged
            .get(&channel_id)
            .filter(|channel| !channel.has_exited())
            .map(|channel| Arc::clone(channel.output()))
    }

    /// Claim a channel's ending, for exactly one caller.
    ///
    /// The claim IS the decision, so a connection that dies while an exit is in
    /// flight cannot produce an exit and an error for one channel: whichever
    /// arrives second finds nothing left to claim.
    pub(crate) fn claim_exit(&self, channel_id: u16) -> Option<Arc<dyn ChannelBinding>> {
        let mut table = self.lock();
        let channel = table.acknowledged.get_mut(&channel_id)?;
        if channel.has_exited() {
            return None;
        }
        channel.mark_exited();
        Some(Arc::clone(channel.output()))
    }

    /// Whether this worker knows the channel and has seen it end.
    pub(crate) fn has_exited(&self, channel_id: u16) -> bool {
        self.lock()
            .acknowledged
            .get(&channel_id)
            .is_some_and(PoolChannel::has_exited)
    }

    /// The pairs a hello announces: live channels only, in id order.
    pub(crate) fn bindings(&self) -> Vec<KeeperChannelBinding> {
        super::live_bindings(self.lock().acknowledged.values())
    }

    /// The channels with a spawn in flight, for a probe that must not call them
    /// empty.
    pub(crate) fn spawning(&self) -> Vec<u16> {
        self.lock().spawning.keys().copied().collect()
    }

    /// Take a channel out of the table entirely, for a session that has closed.
    pub(crate) fn forget(&self, channel_id: u16) -> Option<PoolChannel> {
        let mut table = self.lock();
        table.spawning.remove(&channel_id);
        table.acknowledged.remove(&channel_id)
    }

    /// Empty the table, for a connection that is gone.
    ///
    /// The channels are RETURNED rather than dropped so the caller can tell each
    /// one what happened; dropping them here would end every session silently.
    pub(crate) fn drain(&self) -> Vec<PoolChannel> {
        let mut table = self.lock();
        table.spawning.clear();
        std::mem::take(&mut table.acknowledged)
            .into_values()
            .collect()
    }

    /// A poisoned table is still the truth about the channels.
    ///
    /// A panic while holding this lock means one delivery was interrupted, not
    /// that the ids were corrupted, and refusing to serve the remaining sessions
    /// because of it would turn one bad frame into a machine with no terminals.
    fn lock(&self) -> MutexGuard<'_, Table> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
