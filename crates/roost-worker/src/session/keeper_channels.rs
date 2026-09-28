//! The keeper seam the session layer drives: the operations it performs on a
//! keeper, the fault each one can return, and a survivor's ordered history.
//! `keeper_pool` implements [`KeeperChannels`]; `session::resume`,
//! `session::lifecycle` and `runtime::adoption` call it. Depends on
//! `roost_keeper` for the frame and history vocabulary.

use std::sync::Arc;

use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;

use super::sinks::ChannelBinding;

/// Why a keeper operation could not be performed. Its own type rather than the
/// keeper client's, because the obligation differs by operation: a failed
/// channel list adopts nothing, a failed history must become a respawn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the keeper refused `{operation}`: {reason}")]
pub struct KeeperFault {
    pub operation: &'static str,
    pub reason: String,
}

/// The keeper operations the session layer performs, and nothing else — its
/// whole view of a keeper, and a trait because `keeper_pool` owns the socket: a
/// session with its own connection would be a second owner of a machine's PTYs.
pub trait KeeperChannels: Send + Sync {
    /// The channels this keeper still holds, and each one's child pid.
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault>;
    /// One channel's ordered history: bytes, geometry markers and the head.
    fn channel_history(&self, channel_id: u16) -> Result<SurvivorHistory, KeeperFault>;
    /// The geometry the keeper has actually applied to this channel.
    fn terminal_state(&self, channel_id: u16) -> Result<TerminalState, KeeperFault>;
    /// Deliver this channel's output into `binding` from now on. The reattach is
    /// what establishes the keeper's ordered boundary, so it MUST precede the
    /// history request.
    fn deliver_into(
        &self,
        channel_id: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<(), KeeperFault>;
    /// Terminate this channel's child.
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault>;
    /// Resize this channel, returning once the keeper has ACKNOWLEDGED `seq`.
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), KeeperFault>;
}

/// One channel's ordered history: output and geometry records oldest first,
/// the head the keeper has emitted to, and the geometry the oldest retained
/// record was produced at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurvivorHistory {
    pub records: Vec<HistoryRecord>,
    /// The highest sequence the keeper has emitted, retained or not. Taken from
    /// the keeper rather than summed from `records`: eviction makes the sum
    /// smaller than the stream, and a head that understates it re-aliases every
    /// absolute history address a browser already holds.
    pub head_seq: u64,
    /// The geometry the oldest retained record was produced at. Required, not
    /// optional: the keeper evicts geometry records first, so the first retained
    /// record can be output whose parser context is gone, and replaying it at
    /// the wrong width paints a screen that was never on that terminal.
    pub base_cols: u16,
    pub base_rows: u16,
}

impl SurvivorHistory {
    /// The retained output bytes, oldest first and contiguous.
    pub fn window(&self) -> Vec<u8> {
        let mut window = Vec::new();
        for record in &self.records {
            if let HistoryRecord::Output { bytes, .. } = record {
                window.extend_from_slice(bytes);
            }
        }
        window
    }

    /// Whether the keeper has evicted bytes this worker can no longer see. Under
    /// eviction the first replayed byte can be the tail of a sequence whose
    /// introducer was overwritten, and a cold core would print that remnant as
    /// literal text and stick.
    pub fn evicted(&self) -> bool {
        self.head_seq > self.window().len() as u64
    }
}
