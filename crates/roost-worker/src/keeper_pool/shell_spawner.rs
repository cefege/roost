//! The session layer's PTY seam, implemented over the pool that owns the
//! socket. `session::spawn` calls it to open a channel and to end one whose
//! spawn did not complete; `keeper_pool::pool` holds the connection and
//! `super::session_seam` already answers the keeper operations. Depends on
//! `roost_protocol`'s `ChannelId` and `session::spawn`'s own traits — nothing
//! else.
//!
//! WHY A SEPARATE FILE AND NOT A THIRD TRAIT ON `KeeperPool`. `session_seam`
//! answers [`KeeperChannels`], which is what a RECOVERING session asks: what
//! does this keeper still hold, and what is its history. This answers
//! [`ShellSpawner`], which is what a NEW session asks: open this, and if that
//! fails, kill it. The two have different failure contracts — one refuses with
//! a reason, the other cannot fail at all — and a caller that got them mixed up
//! would either give up on a survivor it could have adopted, or wait for an
//! answer a kill never sends.
//! Ports v2 `apps/worker/src/session/session-spawn.ts`.

use std::sync::Arc;

use roost_protocol::wire::brand::ChannelId;

use super::KeeperPool;
use crate::session::keeper_channels::KeeperChannels;
use crate::session::sinks::ChannelBinding;
use crate::session::spawn::ShellSpawner;
use crate::shell_spec::ShellSpec;

impl ShellSpawner for KeeperPool {
    /// Open the channel and report the child pid.
    ///
    /// The id arrives from the session's own counter and goes straight to
    /// [`KeeperPool::spawn`], which refuses one the keeper already holds. The
    /// refusal is a `String` because this is a seam the session layer borrows
    /// for the duration of one spawn and does nothing with a failure except put
    /// it in `SpawnRefusal::KeeperRefused` — a second error type here would be
    /// a second thing to convert and nothing else.
    fn spawn_channel(
        &self,
        channel_id: ChannelId,
        spec: &ShellSpec,
        cols: u16,
        rows: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        self.spawn(channel_id, spec, cols, rows, binding)
            .map(|spawned| spawned.pid)
            .map_err(|error| error.to_string())
    }

    /// End the channel, on the path where a PTY exists and the spawn did not
    /// finish.
    ///
    /// NO REPLY IS WAITED FOR, and that is why this cannot be a waiting
    /// request. The daemon kills the child and owes nothing back
    /// (`roost_keeper::client::kill`, tag `0x31`), so a caller that waited
    /// would sit out its entire query timeout on EVERY close — and this is the
    /// close that runs on a failure path, where the session is already
    /// refusing. What proves the kill landed is the channel leaving
    /// `list_channels`, which is the keeper's own reaping rather than this
    /// pool's recollection of it.
    ///
    /// The channel is NOT dropped from the table here. The keeper still owns it
    /// and will still send its exit frame, and a table entry removed early is a
    /// frame with nowhere to go — the session would wait for an ending something
    /// else swallowed.
    fn kill_channel(&self, channel_id: ChannelId) {
        let Ok(narrowed) = u16::try_from(channel_id.as_u32()) else {
            // Unreachable through a session, whose counter hands out `u16` ids.
            // Reported rather than truncated: a truncated id would name a
            // DIFFERENT channel than the caller meant to end.
            tracing::error!(
                %channel_id,
                "a kill was asked for a channel the keeper cannot address"
            );
            return;
        };
        match KeeperChannels::kill_channel(self, narrowed) {
            Ok(()) => {
                tracing::info!(%channel_id, "a spawn that did not complete left no PTY behind");
            }
            Err(fault) => {
                tracing::error!(
                    %channel_id,
                    operation = fault.operation,
                    reason = %fault.reason,
                    "a PTY this spawn opened could not be ended"
                );
            }
        }
    }
}
