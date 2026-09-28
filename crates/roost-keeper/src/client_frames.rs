//! The frame demultiplexer: one channel carries both PTY output and control
//! replies, so a control wait that is waiting for its answer must be able to
//! put back what it picked up on the way. Called by [`crate::client`]'s request
//! and wait paths, and by the worker through `next_event`. Depends only on
//! `KeeperClient`'s own fields and the codec's frame type — and on nothing here.

use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use super::client::KeeperClient;
use crate::client_connect::HELLO_TIMEOUT;
use crate::client_error::ClientError;
use crate::codec::{MuxFrame, MuxFrameType};

/// How a bounded wait ended. A value rather than an error because the resize
/// family must tell a wedged keeper from a departed one, and v2 drives a
/// different recovery from each: the first is a timeout to be asked about with
/// `ResizeStatus`, the second is a resize that will never be answered.
pub(crate) enum WaitEnded {
    Answered(MuxFrame),
    TimedOut,
    Disconnected,
}

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
        self.wait_as_result(channel_id, timeout, &[expected])
    }

    /// Ask a question whose payload is empty and keep the answer's bytes.
    pub(crate) fn request_empty(
        &self,
        frame_type: MuxFrameType,
        expected: MuxFrameType,
        channel_id: u16,
        timeout: Duration,
    ) -> Result<MuxFrame, ClientError> {
        let frame = MuxFrame::new(frame_type, channel_id, Vec::new())
            .map_err(|err| ClientError::Io(err.to_string()))?;
        self.write(&frame)?;
        self.wait_for_reply(expected, channel_id, timeout)
    }

    /// Wait for whichever of the given tags the keeper chose.
    ///
    /// One wait over a SET of tags because a single write can have several
    /// legal answers, and a caller that waited for one of them would time out on
    /// the others — which are the interesting cases.
    pub(crate) fn wait_for_any(
        &self,
        channel_id: u16,
        timeout: Duration,
        accepted: &[MuxFrameType],
    ) -> WaitEnded {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return WaitEnded::TimedOut;
            }
            match self.events.recv_timeout(remaining) {
                Ok(frame)
                    if frame.channel_id == channel_id && accepted.contains(&frame.frame_type) =>
                {
                    return WaitEnded::Answered(frame);
                }
                // Not the answer. It is PTY output, an exit or a pong, and it
                // belongs to the worker -- dropping it here is how terminal
                // output disappears during a resize drag.
                Ok(frame) => self.defer(frame),
                Err(RecvTimeoutError::Timeout) => return WaitEnded::TimedOut,
                Err(RecvTimeoutError::Disconnected) => return WaitEnded::Disconnected,
            }
        }
    }

    /// Wait for whichever of the three input results the keeper chose.
    pub(crate) fn wait_for_any_input_result(
        &self,
        channel_id: u16,
        timeout: Duration,
    ) -> Result<MuxFrame, ClientError> {
        use MuxFrameType as T;
        self.wait_as_result(
            channel_id,
            timeout,
            &[T::PtyInAck, T::PtyInReject, T::PtyInAmbiguous],
        )
    }

    /// A wait with no better cause to report, reported as the one error this
    /// protocol has always reported — so a disconnect during a plain control
    /// round trip does not change the message an operator reads.
    pub(crate) fn wait_as_result(
        &self,
        channel_id: u16,
        timeout: Duration,
        accepted: &[MuxFrameType],
    ) -> Result<MuxFrame, ClientError> {
        match self.wait_for_any(channel_id, timeout, accepted) {
            WaitEnded::Answered(frame) => Ok(frame),
            WaitEnded::TimedOut | WaitEnded::Disconnected => {
                Err(ClientError::SpawnNotAcknowledged {
                    path: self.path.clone(),
                    timeout,
                })
            }
        }
    }
}
