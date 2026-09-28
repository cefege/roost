//! The keeper seam the session layer drives: the operations it performs on a
//! keeper, the fault each one can return, and a survivor's ordered history.
//! `keeper_pool` implements [`KeeperChannels`]; `session::resume`,
//! `session::lifecycle` and `runtime::adoption` call it. Depends on
//! `roost_keeper` for the frame and history vocabulary.

use std::pin::Pin;
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
    /// Resize this channel and report the keeper's own answer: applied (with
    /// the sequence and geometry it applied), refused (with its reason), or
    /// unknown. `Err` is a request that never reached the keeper, which is the
    /// only case a caller may report as pre-write (v2 `admission.written`).
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<roost_keeper::client_resize::ResizeOutcome, KeeperFault>;
    /// Begin one acknowledged input batch (v2 `beginInput`). The admission half
    /// is decided before this returns; the result half settles once the keeper
    /// answers, and a written batch whose answer is lost is ambiguous. BLOCKING:
    /// the write takes the keeper connection, so async callers run it on a
    /// blocking thread.
    fn begin_input(&self, channel_id: u16, bytes: Vec<u8>) -> KeeperInputCommand;
    /// Unacknowledged input (v2 `pool.input`, the legacy `onBinary` path). `Err`
    /// is a write that never reached the keeper.
    fn write_legacy_input(&self, channel_id: u16, bytes: &[u8]) -> Result<(), KeeperFault>;
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

/// Why an acknowledged batch provably never reached the keeper socket. v2
/// `KeeperWriteRejection`; v2's `unsupported` has no form here because a keeper
/// without acknowledged input fails `KeeperClient::hello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputNotWritten {
    Disconnected,
    InvalidRequest,
    QueueFull,
}

impl InputNotWritten {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::InvalidRequest => "invalid_request",
            Self::QueueFull => "queue_full",
        }
    }
}

/// What the keeper said, or failed to say, about one written batch. v2
/// `KeeperInputResult`. `Reject` is the keeper proving nothing reached the PTY;
/// an ambiguous `written: None` is a batch whose count is unknown (timeout,
/// disconnect, a result that contradicts the request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeeperInputResult {
    Ack {
        written: u32,
    },
    Reject {
        reason: String,
    },
    Ambiguous {
        written: Option<u32>,
        reason: String,
    },
}

/// A two-phase keeper input command. v2 `KeeperCommand<KeeperInputResult>`.
pub struct KeeperInputCommand {
    /// `Err` proves the request never reached the socket; `Ok` proves it did.
    pub admission: Result<(), InputNotWritten>,
    pub result: Pin<Box<dyn Future<Output = KeeperInputResult> + Send>>,
}

impl KeeperInputCommand {
    /// A command refused before the socket, whose result is that same refusal.
    pub fn not_written(reason: InputNotWritten) -> Self {
        Self {
            admission: Err(reason),
            result: Box::pin(std::future::ready(KeeperInputResult::Reject {
                reason: reason.as_str().to_owned(),
            })),
        }
    }
}

impl std::fmt::Debug for KeeperInputCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeeperInputCommand")
            .field("admission", &self.admission)
            .finish_non_exhaustive()
    }
}
