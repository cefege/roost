//! Opening one PTY through the pool, and the check a caller's channel id must
//! pass first. `keeper_pool::pool` holds the connection and the channel table
//! this reads and writes; `session::spawn` is the caller that owns the id.
//! Depends on `super::channel_ids` for what the keeper is known to hold, and on
//! `roost_keeper::client` for the spawn frame itself.
//!
//! SPLIT OUT OF `pool` FOR THE CAP, AND THE BOUNDARY IS THE OPERATION. Opening
//! a PTY is the one pool call with a long contract of its own — the id is the
//! caller's, the binding is registered before the frame, and a failure after the
//! keeper was touched has to be undoable — and that contract reads worse as a
//! hundred lines in the middle of a file about the table.

use std::sync::Arc;

use roost_protocol::wire::brand::ChannelId;

use super::error::PoolError;
use super::pool::{KeeperPool, Spawned};
use super::spawn_spec::pty_command;
use crate::session::sinks::ChannelBinding;
use crate::shell_spec::ShellSpec;

impl KeeperPool {
    /// Open a PTY running `spec` on `channel_id`, delivering its output into
    /// `output`.
    ///
    /// THE ID IS THE CALLER'S, and it is the whole of the change from the
    /// allocator this pool used to own. `session::lifecycle::SessionManager`
    /// holds the one counter, beside the stray reaper that advances it past the
    /// keeper's own maximum after an adoption; a second copy here meant a fresh
    /// worker could mint, from this one, an id a surviving keeper still held.
    ///
    /// The output binding is registered BEFORE the spawn frame is written, so
    /// the first bytes after the acknowledgement have a session to reach. An id
    /// the keeper is already known to hold is refused before anything is
    /// written, because a colliding spawn is answered `channel_id in use` and
    /// the new terminal simply fails with a reason that names the wire rather
    /// than the collision.
    pub fn spawn(
        &self,
        channel_id: ChannelId,
        spec: &ShellSpec,
        cols: u16,
        rows: u16,
        output: Arc<dyn ChannelBinding>,
    ) -> Result<Spawned, PoolError> {
        self.require_connected()?;
        let channel_id = self.claim_channel_id(channel_id)?;
        let command = pty_command(spec);
        if command.withheld_any() {
            // Reportable because it means a resolver handed this pool a spec
            // carrying a credential; the refusal itself is already done.
            tracing::warn!(
                channel_id,
                withheld = ?command.withheld,
                executable = %spec.executable,
                "withheld keeper control credentials from a PTY environment"
            );
        }
        if self.channels.begin_spawn(channel_id, output) {
            tracing::warn!(channel_id, "respawning a channel the keeper already owns");
        }
        match self
            .keeper
            .with(|client| client.spawn(channel_id, command.command, cols, rows))
        {
            Ok(pid) => {
                if let Err(err) = self.channels.finish_spawn(channel_id, pid) {
                    // The PTY is real; only the pool's record of it is gone. The
                    // strays reaper is the designed answer to a channel nobody
                    // tracks, and saying so beats a caller that believes it has
                    // no terminal and leaves a shell running.
                    tracing::error!(
                        channel_id,
                        pid,
                        %err,
                        "the keeper opened a channel this pool can no longer track"
                    );
                }
                tracing::info!(
                    channel_id,
                    pid,
                    executable = %spec.executable,
                    cwd = %spec.cwd,
                    "the keeper opened a channel"
                );
                Ok(Spawned { channel_id, pid })
            }
            Err(err) => {
                // The keeper refused or did not answer, so there is no PTY. The
                // binding goes with it: a recycled id must not reach the session
                // that was never opened.
                self.channels.abort_spawn(channel_id);
                tracing::warn!(channel_id, %err, "the keeper did not open the channel");
                Err(PoolError::Keeper(err))
            }
        }
    }

    /// Check a caller's id against everything this pool has learned, and narrow
    /// it to what the keeper addresses channels by.
    ///
    /// Two refusals and no repair, because the pool cannot pick a different id
    /// without becoming the second allocator. An id the keeper holds would
    /// collide; an id its counter has already passed is spent even when the
    /// channel itself is gone, because that id is exactly the one a future
    /// mint could reach.
    fn claim_channel_id(&self, channel_id: ChannelId) -> Result<u16, PoolError> {
        let Some(narrowed) = u16::try_from(channel_id.as_u32()).ok() else {
            return Err(PoolError::ChannelIdTooWide(channel_id));
        };
        if self.channel_ids.refuses(narrowed) {
            let highest = self.channel_ids.highest();
            tracing::error!(
                %channel_id,
                highest,
                "a spawn was handed a channel id the keeper is already using"
            );
            return Err(PoolError::ChannelIdTaken {
                channel_id: narrowed,
                highest,
            });
        }
        Ok(narrowed)
    }
}
