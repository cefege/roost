//! The session layer's view of a keeper, implemented over the pool that owns
//! the socket. `session::resume::SessionManager` is the only caller;
//! `keeper_pool::pool` holds the connection and `roost_keeper::client` speaks
//! the protocol.
//!
//! WHY A REFUSAL RATHER THAN A DERIVATION. `channel_history` cannot be
//! answered from what the keeper socket currently offers, and the two facts it
//! needs are the two that decide whether a replay is correct at all. See
//! [`NO_REPORTED_HEAD`] for the head and [`NO_REPORTED_BASE_GEOMETRY`] for the
//! base. Both are a `roost-keeper` gap, not a decision this file makes, so
//! this one operation refuses with a fault that names them and the adoption
//! ends as a respawn — which is the repair `AdoptRefusal` already documents
//! for a survivor that cannot be replayed. Filling either in from the records
//! would be worse than refusing: a head that understates the stream re-aliases
//! every absolute row address a browser holds, and a base geometry that is not
//! the oldest record's paints a screen that was never on that terminal
//! (`docs/FAILURE-INDEX.md:356`).

use std::sync::Arc;

use roost_keeper::client_resize::ResizeOutcome;
use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::payloads::TerminalState;

use super::KeeperPool;
use super::error::PoolError;
use crate::session::resume::{KeeperChannels, KeeperFault, SurvivorHistory};
use crate::session::sinks::ChannelBinding;

/// The head a retained window was cut from is not on the wire.
///
/// `GetHistoryRecordsResp` carries the records and nothing else: the daemon
/// stamps each record's `seq` from `channel.next_output_seq`, a per-RECORD
/// counter, and `keeper_ops.rs` answers the legacy `GetHistory` with the same
/// payload, so `max(seq)` over the records is a record count and not the byte
/// offset `SurvivorHistory::head_seq` is. The daemon holds the real head
/// (`Keeper::legacy_history`) and `GetHistoryResp` is specified to carry it
/// (`protocol/spec/keeper.md`, tag `0xE5`), but nothing emits that tag.
pub const NO_REPORTED_HEAD: &str =
    "this keeper does not report the head its retained window was cut from, and the records' own \
     sequences are a per-record counter rather than a byte offset, so a window that was truncated \
     cannot be told from a whole one";

/// The geometry the oldest retained record was produced at is not on the wire.
///
/// `ChannelHistory` evicts geometry records FIRST, so after any truncation the
/// marker that would establish the base is the first thing gone, and
/// `terminal_state` answers the CURRENT geometry — the wrong end of the window.
pub const NO_REPORTED_BASE_GEOMETRY: &str =
    "this keeper does not report the geometry its oldest retained record was produced at, and the \
     geometry it does report is the current one, which is the wrong end of the window to rebuild \
     a core at";

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
        self.keeper_channels().map_err(|error| KeeperFault {
            operation: "live_channels",
            reason: error.to_string(),
        })
    }

    /// REFUSED, and the refusal is the finding.
    ///
    /// Everything else in this impl is a keeper call, and this one is a gap in
    /// the keeper: see [`NO_REPORTED_HEAD`] and
    /// [`NO_REPORTED_BASE_GEOMETRY`]. A caller that would rather have a wrong
    /// history than none gets a respawn instead, which is the outcome
    /// `AdoptRefusal::Unreplayable` already exists to produce.
    fn channel_history(&self, _channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        Err(KeeperFault {
            operation: "channel_history",
            reason: format!("{NO_REPORTED_HEAD}; {NO_REPORTED_BASE_GEOMETRY}"),
        })
    }

    /// The geometry the keeper has applied, which is the only authority on
    /// where a surviving PTY is.
    fn terminal_state(&self, channel_id: u16) -> Result<TerminalState, KeeperFault> {
        self.applied_geometry(channel_id).map_err(|error| KeeperFault {
            operation: "terminal_state",
            reason: error.to_string(),
        })
    }

    /// Route this channel's output into `binding` from now on.
    ///
    /// Registration only, and that is the whole reattach on this side: the
    /// keeper streams to every channel it holds regardless of who is reading,
    /// so making the bytes land is a table insert. The ordering constraint the
    /// session layer documents — reattach BEFORE the history request, because
    /// the reattach is what establishes the ordered boundary — is why this is
    /// a distinct operation rather than a field on the adoption request.
    fn deliver_into(
        &self,
        channel_id: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<(), KeeperFault> {
        let pid = self.pid_of(channel_id)?;
        self.adopt(channel_id, pid, binding);
        tracing::info!(channel_id, pid, "keeper: a survivor's output is bound to this worker");
        Ok(())
    }

    /// Terminate this channel's child.
    ///
    /// The daemon owes nothing for a kill (`keeper_ops.rs`, tag `0x31`), so
    /// what proves it landed is the channel leaving `ListChannels` — which the
    /// next `live_channels` reports and this pool's own exit frame claims.
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault> {
        self.request(|client| client.kill(channel_id))
            .map_err(|error| KeeperFault {
                operation: "kill_channel",
                reason: error.to_string(),
            })?;
        tracing::info!(channel_id, "keeper: a child's termination was written");
        Ok(())
    }

    /// Resize this channel, returning once the keeper has acknowledged `seq`.
    ///
    /// The acknowledgement is the contract, so the three outcomes are three
    /// answers rather than one: a keeper that refused is a PTY that will not
    /// move, and a caller that read that as a success would paint a grid the
    /// PTY is not at.
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), KeeperFault> {
        let fault = |reason: String| KeeperFault {
            operation: "resize_channel",
            reason,
        };
        match self.resize(channel_id, seq, cols, rows) {
            Ok(ResizeOutcome::Applied { seq: applied, .. }) => {
                tracing::info!(channel_id, seq, applied, cols, rows, "keeper: a resize was applied");
                Ok(())
            }
            Ok(ResizeOutcome::Refused { reason, .. }) => {
                Err(fault(format!("the keeper refused seq {seq}: {reason:?}")))
            }
            Ok(ResizeOutcome::Unknown { reason, .. }) => {
                Err(fault(format!("the keeper did not answer seq {seq}: {reason:?}")))
            }
            Err(error) => Err(fault(error.to_string())),
        }
    }
}

impl KeeperPool {
    /// The pid the keeper reports for a channel, or a fault naming its absence.
    ///
    /// Taken from the keeper rather than from this pool's table, because a
    /// channel this pool never spawned has no entry to read it from, and the
    /// pid it is announcing has to be the keeper's truth.
    fn pid_of(&self, channel_id: u16) -> Result<u32, KeeperFault> {
        self.keeper_channels()
            .map_err(|error| KeeperFault {
                operation: "deliver_into",
                reason: error.to_string(),
            })?
            .into_iter()
            .find(|held| held.channel_id == channel_id)
            .map(|held| held.pid)
            .ok_or_else(|| KeeperFault {
                operation: "deliver_into",
                reason: format!("the keeper holds no channel {channel_id}, so it has no pid to \
                                announce it with"),
            })
    }
}
