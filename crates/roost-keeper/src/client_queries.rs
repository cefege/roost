//! The questions a worker asks a keeper ABOUT it: the geometry a surviving PTY
//! is actually at, and the two ways to retire it. Owned by the worker, through
//! [`KeeperClient`]. The history reads live in `client_history`, because they
//! are the one pair of requests that must cross an ordered output boundary.
//!
//! A test for any of these must take its expected value from the DAEMON's
//! answer, cited by line, and never by calling the method under test: a
//! fixture that asks the client what the keeper would say is `x == x`.

use std::time::Duration;

use super::client::KeeperClient;
use crate::client_error::ClientError;
use crate::codec::MuxFrameType;
use crate::payloads::TerminalState;

/// How long a question about live keeper state may take before the keeper is
/// treated as wedged rather than busy.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

impl KeeperClient {
    /// The geometry the keeper has ACTUALLY applied to a channel.
    ///
    /// Authoritative, and answered from live channel state (`keeper_ops.rs:260`)
    /// rather than from a retained record. Without it a worker cannot establish
    /// an ordered parse boundary for a surviving PTY and must either refuse to
    /// adopt or trust a session row that says nothing about what the PTY is.
    pub fn terminal_state(&self, channel_id: u16) -> Result<TerminalState, ClientError> {
        let frame = self.request_empty(
            MuxFrameType::GetTerminalState,
            MuxFrameType::GetTerminalStateResp,
            channel_id,
            QUERY_TIMEOUT,
        )?;
        TerminalState::decode(&frame.payload).map_err(|error| {
            ClientError::Io(format!(
                "the keeper's terminal state did not decode: {error}"
            ))
        })
    }

    /// Terminate one channel's child.
    ///
    /// The daemon kills it and owes NOTHING (`keeper.rs:230` returns an empty
    /// `Vec`), so there is no answer to wait for and a caller that waited for
    /// one would sit out its whole timeout on every single close. What proves
    /// the kill landed is the channel leaving `list_channels`, which is the
    /// keeper's own reaping (`keeper.rs:275`).
    pub fn kill(&self, channel_id: u16) -> Result<(), ClientError> {
        let frame = crate::codec::MuxFrame::new(MuxFrameType::KillChild, channel_id, Vec::new())
            .map_err(|error| {
                ClientError::Io(format!("a kill frame could not be built: {error}"))
            })?;
        self.write(&frame)
    }

    /// Shut the keeper down, whatever it is holding.
    ///
    /// The DESTRUCTIVE one: the daemon stops and every PTY it owned dies with it
    /// (`keeper.rs:209`, and the exit decision in `bin/roost-keeper.rs:125`).
    /// Everything in the programme that is not deliberate offline maintenance
    /// uses [`KeeperClient::shutdown_if_empty`] instead.
    pub fn shutdown(&self) -> Result<(), ClientError> {
        let frame = crate::codec::MuxFrame::new(MuxFrameType::Shutdown, 0, Vec::new())
            .map_err(|error| ClientError::Io(error.to_string()))?;
        self.write(&frame)?;
        self.wait_for_reply(MuxFrameType::ShutdownAck, 0, QUERY_TIMEOUT)?;
        Ok(())
    }

    /// Shut the keeper down ONLY if it holds no live channels.
    ///
    /// The check and the answer are one operation on the daemon side
    /// (`keeper_ops.rs:70`), so a keeper handed a new PTY cannot retire itself
    /// out from under the channel it was just given. A refusal is an ANSWER,
    /// not a failure: the keeper is alive and busy, which is the healthy case
    /// and the reason this frame exists. Both tags are therefore waited for —
    /// a caller that waited only for the ack could not see the reject, and
    /// would pay the whole timeout to learn something the daemon had already
    /// said. A timeout is NOT a refusal: it is a wedged keeper, and it is
    /// reported as the error it is.
    ///
    /// The daemon ends the connection for this exchange EITHER WAY
    /// (`server.rs:322`), so a caller that means to keep driving this keeper
    /// must reconnect rather than issue another request on this client.
    pub fn shutdown_if_empty(&self) -> Result<bool, ClientError> {
        let frame = crate::codec::MuxFrame::new(MuxFrameType::ShutdownIfEmpty, 0, Vec::new())
            .map_err(|error| ClientError::Io(error.to_string()))?;
        self.write(&frame)?;
        let answer = self.wait_as_result(
            0,
            QUERY_TIMEOUT,
            &[
                MuxFrameType::ShutdownIfEmptyAck,
                MuxFrameType::ShutdownIfEmptyReject,
            ],
        )?;
        Ok(answer.frame_type == MuxFrameType::ShutdownIfEmptyAck)
    }
}
