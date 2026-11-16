//! Reading a channel's retained history over the client: the ordered records an
//! adoption or a core re-proof replays. Ports the client half of
//! `apps/worker/src/keeper/keeper-pool-channels.ts` (`getChannelHistoryRecords`)
//! and the matching arms of `keeper-pool-lifecycle.ts`. Called by the worker's
//! keeper pool.
//!
//! THE ANSWER IS AN ORDERED BOUNDARY. The keeper appends every chunk to the
//! history before it sends the `PtyOut`, and answers on the same socket, so a
//! channel's `PtyOut` that arrives before the answer is already inside it and
//! one that arrives after is not. v2 buffers the former while the request is
//! pending and discards the buffer when the records land; here the former are
//! dropped as the wait meets them, including any a previous wait deferred.

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use super::client::KeeperClient;
use crate::client_error::ClientError;
use crate::codec::{MuxFrame, MuxFrameType};
use crate::history::HistoryRecords;

/// v2 `getHistoryRecords timed out after 3000ms`.
pub const HISTORY_TIMEOUT: Duration = Duration::from_secs(3);

/// A history answer and how many pre-boundary output bytes were dropped for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedHistory {
    pub history: HistoryRecords,
    pub dropped_output_bytes: usize,
}

impl KeeperClient {
    /// The ordered history a channel still retains, at an exact stream
    /// boundary: nothing this channel emitted before the answer is left queued.
    pub fn history_records(&self, channel_id: u16) -> Result<BoundedHistory, ClientError> {
        self.write(&empty_frame(MuxFrameType::GetHistoryRecords, channel_id)?)?;
        let (frame, dropped_output_bytes) =
            self.wait_across_history_boundary(channel_id, MuxFrameType::GetHistoryRecordsResp)?;
        // An unreadable answer is a protocol violation, not a lost keeper: v2
        // rejects the one waiter and keeps the connection.
        let history = HistoryRecords::decode(&frame.payload).map_err(|error| {
            ClientError::Protocol(format!(
                "getHistoryRecords: invalid keeper response: {error}"
            ))
        })?;
        Ok(BoundedHistory {
            history,
            dropped_output_bytes,
        })
    }

    fn wait_across_history_boundary(
        &self,
        channel_id: u16,
        expected: MuxFrameType,
    ) -> Result<(MuxFrame, usize), ClientError> {
        let mut dropped = self.drop_deferred_output(channel_id);
        let deadline = Instant::now() + HISTORY_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.history_timeout());
            }
            match self.events.recv_timeout(remaining) {
                Ok(frame) if frame.channel_id == channel_id && frame.frame_type == expected => {
                    tracing::debug!(
                        channel_id,
                        dropped,
                        "keeper client: a history boundary was reached"
                    );
                    return Ok((frame, dropped));
                }
                Ok(frame)
                    if frame.channel_id == channel_id
                        && frame.frame_type == MuxFrameType::PtyOut =>
                {
                    dropped += frame.payload.len();
                }
                Ok(frame) => self.defer(frame),
                Err(RecvTimeoutError::Timeout) => return Err(self.history_timeout()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ClientError::Io(
                        "getHistoryRecords: keeper socket closed".to_owned(),
                    ));
                }
            }
        }
    }

    /// A wedged keeper, reported as every unanswered control wait is: NOT a
    /// lost connection, so the pool keeps driving it (v2 rejects the waiter only).
    fn history_timeout(&self) -> ClientError {
        ClientError::SpawnNotAcknowledged {
            path: self.path.clone(),
            timeout: HISTORY_TIMEOUT,
        }
    }

    /// Drop this channel's output a previous wait held back: it preceded this
    /// request, so the answer already contains it.
    fn drop_deferred_output(&self, channel_id: u16) -> usize {
        let mut held = match self.deferred.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut dropped = 0;
        held.retain(|frame| {
            let before_boundary =
                frame.channel_id == channel_id && frame.frame_type == MuxFrameType::PtyOut;
            if before_boundary {
                dropped += frame.payload.len();
            }
            !before_boundary
        });
        dropped
    }
}

fn empty_frame(frame_type: MuxFrameType, channel_id: u16) -> Result<MuxFrame, ClientError> {
    MuxFrame::new(frame_type, channel_id, Vec::new())
        .map_err(|error| ClientError::Io(error.to_string()))
}
