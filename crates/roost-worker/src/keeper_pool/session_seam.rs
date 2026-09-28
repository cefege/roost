//! The session layer's view of a keeper, implemented over the pool that owns
//! the socket. `session::resume`, `session::core_reprove`, `session::resize`
//! and `session::input_write` are the callers; `keeper_pool::pool` holds the
//! connection and `roost_keeper::client` speaks the protocol. Ports the
//! channel half of `apps/worker/src/keeper/keeper-pool-channels.ts` and
//! `keeper-pool-io.ts` as the session layer sees them.

use std::sync::Arc;

use roost_keeper::client_resize::ResizeOutcome;
use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::payloads::TerminalState;

use super::KeeperPool;
use super::error::PoolError;
use super::pool_history::Reattach;
use crate::session::keeper_channels::{
    KeeperChannels, KeeperFault, KeeperInputCommand, SurvivorHistory,
};
use crate::session::sinks::ChannelBinding;

impl KeeperPool {
    /// The geometry the keeper has actually applied to a channel.
    ///
    /// Asked of the keeper rather than remembered, because the applied
    /// sequence survives the worker that set it: a resize answered before a
    /// restart is still the PTY's geometry after one.
    pub fn applied_geometry(&self, channel_id: u16) -> Result<TerminalState, PoolError> {
        self.request(|client| client.terminal_state(channel_id))
    }
}

impl KeeperChannels for KeeperPool {
    /// The channels this keeper still holds, with each one's child pid.
    ///
    /// `ListChannels` is the keeper's own reaping, so a channel that has left
    /// this list is gone — which is what makes it the right precondition for an
    /// adoption rather than this pool's recollection of its own table.
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault> {
        self.keeper_channels()
            .map_err(|error| fault("live_channels", &error))
    }

    /// The ordered history, read at the keeper's boundary: the head, the base
    /// geometry and the records up to the answer (v2 `getHistoryRecords`).
    fn channel_history(&self, channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        self.history_at_boundary(channel_id, None)
            .map(SurvivorHistory::from)
            .map_err(|error| fault("channel_history", &error))
    }

    /// v2 `reattach` then `getHistoryRecords`, as one step at one boundary:
    /// everything the survivor emits after the history's head reaches
    /// `binding`, and nothing inside the history reaches it twice.
    fn reattach_with_history(
        &self,
        channel_id: u16,
        pid: u32,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<SurvivorHistory, KeeperFault> {
        self.history_at_boundary(channel_id, Some(Reattach { pid, binding }))
            .map(SurvivorHistory::from)
            .map_err(|error| fault("reattach_with_history", &error))
    }

    /// The geometry the keeper has applied, which is the only authority on
    /// where a surviving PTY is.
    fn terminal_state(&self, channel_id: u16) -> Result<TerminalState, KeeperFault> {
        self.applied_geometry(channel_id)
            .map_err(|error| fault("terminal_state", &error))
    }

    /// Terminate this channel's child.
    ///
    /// The daemon owes nothing for a kill (`keeper_ops.rs`, tag `0x31`), so
    /// what proves it landed is the channel leaving `ListChannels` — which the
    /// next `live_channels` reports and this pool's own exit frame claims.
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault> {
        self.request(|client| client.kill(channel_id))
            .map_err(|error| fault("kill_channel", &error))?;
        tracing::info!(channel_id, "keeper: a child's termination was written");
        Ok(())
    }

    /// Resize this channel and hand back the keeper's own answer.
    ///
    /// The three outcomes stay three answers: a refusal is a PTY that will not
    /// move and names why, and an unknown is the one case a caller recovers
    /// from history instead of assuming. `Err` is a request that was never
    /// written: no connection, or the same sequence already in flight.
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, KeeperFault> {
        let outcome = self
            .resize(channel_id, seq, cols, rows)
            .map_err(|error| fault("resize_channel", &error))?;
        tracing::info!(
            channel_id,
            seq,
            cols,
            rows,
            ?outcome,
            "keeper: a resize was answered"
        );
        Ok(outcome)
    }

    /// One acknowledged batch, written under a worker-owned keeper sequence.
    fn begin_input(&self, channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand {
        self.begin_acknowledged_input(channel_id, bytes)
    }

    /// The unacknowledged legacy frame.
    fn write_legacy_input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), KeeperFault> {
        self.input(channel_id, bytes)
            .map_err(|error| fault("write_legacy_input", &error))
    }
}

fn fault(operation: &'static str, error: &PoolError) -> KeeperFault {
    KeeperFault {
        operation,
        reason: error.to_string(),
    }
}
