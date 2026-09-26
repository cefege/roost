//! The frame demultiplexer: one channel carries both PTY output and control
//! replies, so a control wait that is waiting for its answer must be able to
//! put back what it picked up on the way. Called by [`crate::client`]'s request
//! and wait paths, and by the worker through `next_event`. Depends only on
//! `KeeperClient`'s own fields and the codec's frame type — and on nothing here.

use std::time::{Duration, Instant};

use super::client::KeeperClient;
use crate::client_connect::HELLO_TIMEOUT;
use crate::client_error::ClientError;
use crate::codec::{MuxFrame, MuxFrameType};

impl KeeperClient {
    /// Hold a frame a control wait consumed, so the worker still receives it.
    ///
    /// Never blocks and never drops: a poisoned lock is recovered rather than
    /// propagated, because losing the buffer loses terminal output, and a panic
    /// in a neighbouring task is not a reason to lose bytes.
    fn defer(&self, frame: MuxFrame) {
        match self.deferred.lock() {
            Ok(mut held) => held.push_back(frame),
            Err(poisoned) => poisoned.into_inner().push_back(frame),
        }
    }

    /// Take a frame the keeper sent that was not a reply.
    ///
    pub fn next_event(&self, wait: Duration) -> Option<MuxFrame> {
        // Deferred first, in arrival order. A frame a control wait consumed
        // arrived before the one now waiting on the socket, so reading the
        // socket first would reorder the stream the worker is parsing.
        let held = match self.deferred.lock() {
            Ok(mut held) => held.pop_front(),
            Err(poisoned) => poisoned.into_inner().pop_front(),
        };
        if held.is_some() {
            return held;
        }
        self.events.recv_timeout(wait).ok()
    }

    pub(crate) fn request<T: serde::Serialize>(
        &self,
        frame_type: MuxFrameType,
        expected: MuxFrameType,
        channel_id: u16,
        body: &T,
    ) -> Result<MuxFrame, ClientError> {
        let frame = MuxFrame::json(frame_type, channel_id, body)
            .map_err(|err| ClientError::Io(err.to_string()))?;
        self.write(&frame)?;
        self.wait_for_reply(expected, channel_id, HELLO_TIMEOUT)
    }

    pub(crate) fn request_tag(
        &self,
        frame_type: MuxFrameType,
        expected: MuxFrameType,
        channel_id: u16,
        payload: &[u8],
    ) -> Result<MuxFrameType, ClientError> {
        let frame = MuxFrame::new(frame_type, channel_id, payload.to_vec())
            .map_err(|err| ClientError::Io(err.to_string()))?;
        self.write(&frame)?;
        Ok(self
            .wait_for_reply(expected, channel_id, Duration::from_secs(10))?
            .frame_type)
    }

    /// Wait for the answer to a control frame.
    ///
    /// A timeout here is a wedged keeper, not a slow one: the control frames
    /// involved do no work beyond bookkeeping, and the only ones that can take
    /// real time are covered by their own dedicated paths.
    pub(crate) fn wait_for_reply(
        &self,
        expected: MuxFrameType,
        channel_id: u16,
        timeout: Duration,
    ) -> Result<MuxFrame, ClientError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = self.events.recv_timeout(remaining).map_err(|_| {
                ClientError::SpawnNotAcknowledged {
                    path: self.path.clone(),
                    timeout,
                }
            })?;
            if frame.frame_type == expected && frame.channel_id == channel_id {
                return Ok(frame);
            }
            // Not the answer. It is PTY output, an exit or a pong, and it
            // belongs to the worker -- dropping it here is how terminal output
            // disappears during a resize drag.
            self.defer(frame);
        }
        Err(ClientError::SpawnNotAcknowledged {
            path: self.path.clone(),
            timeout,
        })
    }

    /// Wait for whichever of the three input results the keeper chose.
    ///
    /// Separate from `wait_for_reply` because a single write has THREE legal
    /// answers — ack, reject, ambiguous — and a caller that waited for one tag
    /// would time out on the other two, which are the interesting cases.
    pub(crate) fn wait_for_any_input_result(
        &self,
        channel_id: u16,
        timeout: Duration,
    ) -> Result<MuxFrame, ClientError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = self.events.recv_timeout(remaining).map_err(|_| {
                ClientError::SpawnNotAcknowledged {
                    path: self.path.clone(),
                    timeout,
                }
            })?;
            if frame.channel_id == channel_id
                && matches!(
                    frame.frame_type,
                    MuxFrameType::PtyInAck
                        | MuxFrameType::PtyInReject
                        | MuxFrameType::PtyInAmbiguous
                )
            {
                return Ok(frame);
            }
            // Not one of the three answers. Same reason as `wait_for_reply`:
            // this is the worker's output, not this call's business.
            self.defer(frame);
        }
        Err(ClientError::SpawnNotAcknowledged {
            path: self.path.clone(),
            timeout,
        })
    }
}
