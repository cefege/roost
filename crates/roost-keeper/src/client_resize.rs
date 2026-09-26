//! What a geometry change ACTUALLY did: the acknowledgement the keeper sent,
//! the refusal it sent instead, and the three answers that are neither.
//! Owned by the worker, through [`KeeperClient::resize`].
//!
//! The split exists because a resize has three outcomes and a caller must be
//! able to act on each one differently. An acknowledgement proves the PTY is
//! at a geometry; a refusal says it never will be, and names why; and the
//! unknown case is the only one that can be recovered, by asking the keeper
//! what it applied ([`KeeperClient::resize_status`]). Collapsing all three into
//! an error string is what this module replaces: a caller that read a refusal
//! as a failure retried a resize the keeper had already declined, and a caller
//! that read a timeout as a failure believed the PTY was at the geometry it had
//! asked for when nothing had proved it.
//!
//! v2's shape is `KeeperResizeResult` in
//! `apps/worker/src/keeper/protocol-terminal.ts:18-60`, and the three callers
//! that need all of it are named there.

use std::time::Duration;

use super::client::KeeperClient;
use crate::client_error::ClientError;
use crate::client_frames::WaitEnded;
use crate::codec::{MuxFrame, MuxFrameType, read_sequence, write_sequence};
use crate::payloads::{ResizeRequest, TerminalState};

/// How long a resize waits for the keeper's answer before it is unknown.
///
/// Not a failure deadline: an unanswered resize is still in flight, and the
/// caller recovers it by asking [`KeeperClient::resize_status`] rather than by
/// sending the sequence again. v2's bound is 2.5s for the same reason, and
/// this one is longer only because the daemon answers resize from live channel
/// state, which cannot take a lock.
pub const RESIZE_ACK_TIMEOUT: Duration = Duration::from_secs(10);

impl KeeperClient {
    /// Ask the keeper to resize a channel, and report what it did.
    ///
    /// Returns the keeper's answer, not the request's echo. The daemon answers a
    /// stale sequence with the geometry it is ALREADY at
    /// (`pty_channel.rs:200`), so `Applied` can carry a sequence and a size the
    /// caller never asked for, and that difference is the whole reason the
    /// answer is decoded rather than reconstructed.
    ///
    /// No `Result`: every failure of the wire is an outcome of the resize, and
    /// a caller that cannot tell them apart cannot choose between resending and
    /// asking.
    pub fn resize(&self, channel_id: u16, seq: u64, cols: u16, rows: u16) -> ResizeOutcome {
        let request = ResizeRequest { seq, cols, rows };
        let payload = match request.encode() {
            Ok(payload) => payload,
            // A geometry this build cannot encode never reached the keeper, so
            // the refusal is the honest answer and it costs no round trip.
            Err(error) => {
                tracing::warn!(
                    channel_id,
                    seq,
                    cols,
                    rows,
                    error = %error,
                    "keeper: a resize this client cannot encode"
                );
                return ResizeOutcome::Refused {
                    seq,
                    reason: ResizeRejectReason::InvalidRequest,
                };
            }
        };
        let frame = match MuxFrame::new(MuxFrameType::ResizeRequest, channel_id, payload) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(channel_id, seq, error = %error, "keeper: an unbuildable resize frame");
                return ResizeOutcome::Refused {
                    seq,
                    reason: ResizeRejectReason::InvalidRequest,
                };
            }
        };
        if self.write(&frame).is_err() {
            return ResizeOutcome::Unknown {
                seq,
                reason: ResizeUnknownReason::Disconnected,
            };
        }

        match self.wait_for_any(
            channel_id,
            RESIZE_ACK_TIMEOUT,
            &[MuxFrameType::ResizeAck, MuxFrameType::ResizeReject],
        ) {
            WaitEnded::Answered(answer) => decode_resize_answer(seq, &answer),
            WaitEnded::TimedOut => {
                tracing::warn!(channel_id, seq, "keeper: the resize answer never arrived");
                ResizeOutcome::Unknown {
                    seq,
                    reason: ResizeUnknownReason::Timeout,
                }
            }
            WaitEnded::Disconnected => ResizeOutcome::Unknown {
                seq,
                reason: ResizeUnknownReason::Disconnected,
            },
        }
    }

    /// Ask the keeper which resize it has applied, without applying one.
    ///
    /// The RECOVERY for [`KeeperClient::resize`]'s `Unknown`, and the only
    /// answer to a worker that lost an acknowledgement: it is answered from
    /// live channel state (`keeper_ops.rs:246`), which is the one source that
    /// cannot itself have been evicted, and it never re-applies geometry, so
    /// asking twice is safe.
    pub fn resize_status(&self, channel_id: u16, seq: u64) -> Result<TerminalState, ClientError> {
        let mut payload = Vec::with_capacity(8);
        write_sequence(&mut payload, seq);
        let frame = MuxFrame::new(MuxFrameType::ResizeStatus, channel_id, payload)
            .map_err(|error| ClientError::Io(error.to_string()))?;
        self.write(&frame)?;
        let answer =
            self.wait_for_reply(MuxFrameType::ResizeAck, channel_id, RESIZE_ACK_TIMEOUT)?;
        TerminalState::decode(&answer.payload)
            .map_err(|error| ClientError::Io(format!("the resize status did not decode: {error}")))
    }
}

/// Decode the keeper's answer to a resize, and never guess at it.
///
/// A payload that does not decode, or a refusal carrying a reason code from a
/// newer keeper, is `ProtocolError` rather than a reason: inventing one would
/// drive the recovery the reason names, and the recovery is destructive.
fn decode_resize_answer(seq: u64, answer: &MuxFrame) -> ResizeOutcome {
    match answer.frame_type {
        MuxFrameType::ResizeAck => match TerminalState::decode(&answer.payload) {
            Ok(state) => ResizeOutcome::Applied {
                seq: state.applied_seq,
                cols: state.cols,
                rows: state.rows,
            },
            Err(error) => {
                tracing::warn!(seq, error = %error, "keeper: an undecodable resize acknowledgement");
                ResizeOutcome::Unknown {
                    seq,
                    reason: ResizeUnknownReason::ProtocolError,
                }
            }
        },
        MuxFrameType::ResizeReject => {
            // `[seq:u64][reason:u8]`, written by the daemon's reject builder
            // (`keeper.rs:306`). The sequence in the refusal is the one being
            // refused, which is how a caller matches it to its own request.
            let refused = read_sequence(&answer.payload, 0);
            let code = answer.payload.get(8).copied();
            match (refused, code.and_then(ResizeRejectReason::from_code)) {
                (Some(refused), Some(reason)) => ResizeOutcome::Refused {
                    seq: refused,
                    reason,
                },
                _ => ResizeOutcome::Unknown {
                    seq,
                    reason: ResizeUnknownReason::ProtocolError,
                },
            }
        }
        other => {
            tracing::warn!(seq, tag = ?other, "keeper: an unanswerable resize frame");
            ResizeOutcome::Unknown {
                seq,
                reason: ResizeUnknownReason::ProtocolError,
            }
        }
    }
}

/// Every answer a resize can have. The names are v2's (`protocol-terminal.ts`),
/// because a session layer written against v2 reads these and the wire codes
/// are the contract between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeOutcome {
    /// The keeper applied a geometry, and this is the one it applied.
    Applied { seq: u64, cols: u16, rows: u16 },
    /// The keeper refused, and named why. Nothing was written to the PTY.
    Refused {
        seq: u64,
        reason: ResizeRejectReason,
    },
    /// Nobody knows: the request may have landed and the answer may be lost.
    Unknown {
        seq: u64,
        reason: ResizeUnknownReason,
    },
}

impl ResizeOutcome {
    /// The sequence this outcome is about, which is the request's own for an
    /// unknown one and the refused one for a refusal.
    pub fn seq(&self) -> u64 {
        match self {
            ResizeOutcome::Applied { seq, .. }
            | ResizeOutcome::Refused { seq, .. }
            | ResizeOutcome::Unknown { seq, .. } => *seq,
        }
    }

    /// The geometry the PTY is at, when the keeper proved it.
    pub fn applied_geometry(&self) -> Option<(u16, u16)> {
        match self {
            ResizeOutcome::Applied { cols, rows, .. } => Some((*cols, *rows)),
            _ => None,
        }
    }

    /// Whether the caller must recover this resize rather than assume it.
    pub fn is_unknown(&self) -> bool {
        matches!(self, ResizeOutcome::Unknown { .. })
    }
}

/// Why the keeper refused a geometry change, with the wire codes v2 fixes.
///
/// A refusal is a decision the keeper already made, so the reason is the whole
/// message: `channel_missing` and `channel_exited` mean the session is gone,
/// `invalid_request` means the caller is wrong, and everything else is worth
/// retrying. A client that collapses them cannot choose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeRejectReason {
    /// The channel is not on this keeper.
    ChannelMissing,
    /// The channel's child has exited.
    ChannelExited,
    /// The channel has no terminal to resize.
    TerminalMissing,
    /// The terminal refused the ioctl.
    ResizeError,
    /// The sequence was already consumed, so the keeper kept what it had.
    StaleSequence,
    /// The sequence is one the keeper has no record of.
    UnknownSequence,
    /// The request could not be encoded or was out of bounds.
    InvalidRequest,
    /// This keeper does not acknowledge resizes.
    Unsupported,
    /// The connection went before the request could be written.
    Disconnected,
}

impl ResizeRejectReason {
    /// The byte this reason travels as. Fixed by v2 at
    /// `protocol-terminal.ts:50-60`, so a keeper from either build reads the
    /// other's refusals.
    pub fn code(self) -> u8 {
        match self {
            ResizeRejectReason::ChannelMissing => 1,
            ResizeRejectReason::ChannelExited => 2,
            ResizeRejectReason::TerminalMissing => 3,
            ResizeRejectReason::ResizeError => 4,
            ResizeRejectReason::StaleSequence => 5,
            ResizeRejectReason::UnknownSequence => 6,
            ResizeRejectReason::InvalidRequest => 7,
            ResizeRejectReason::Unsupported => 8,
            ResizeRejectReason::Disconnected => 9,
        }
    }

    /// The reason a code denotes, or `None` for a code from a keeper newer than
    /// this build. `None` rather than a default on purpose: every code here
    /// drives a different recovery, and a wrong guess is a destructive one.
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => ResizeRejectReason::ChannelMissing,
            2 => ResizeRejectReason::ChannelExited,
            3 => ResizeRejectReason::TerminalMissing,
            4 => ResizeRejectReason::ResizeError,
            5 => ResizeRejectReason::StaleSequence,
            6 => ResizeRejectReason::UnknownSequence,
            7 => ResizeRejectReason::InvalidRequest,
            8 => ResizeRejectReason::Unsupported,
            9 => ResizeRejectReason::Disconnected,
            _ => return None,
        })
    }
}

/// Why a resize's outcome is unknown. Three causes, because a session layer
/// recovers them differently: a timeout is asked about, a disconnection is
/// reconnected, and a protocol error is not retried at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeUnknownReason {
    /// The keeper connection closed before the request could be written.
    Disconnected,
    /// The request may have landed; the answer did not arrive.
    Timeout,
    /// The keeper answered something this build cannot read.
    ProtocolError,
}
