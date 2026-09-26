//! What the keeper sends that is not an answer to a request: PTY bytes and
//! exits, delivered to the session each belongs to. `pool::KeeperPool` starts
//! the loop in its constructor; nothing else drives it. Depends on the pool and
//! the keeper's frame types — nothing here.
//!
//! THE LOOP OWNS THE EVENT STREAM. The client hands a frame to whoever is
//! waiting for it and drops the rest here, so there is exactly one reader: a
//! second one would steal a reply from the request that is blocked on it, and a
//! request that loses its reply looks precisely like a wedged keeper.
//!
//! A REQUEST'S ANSWER IS NEVER DISPATCHED. A frame that answers a request is
//! consumed inside the call, while the connection handle is held, and the
//! dispatcher cannot run at the same time. So every answer seen here belongs to
//! a request that already returned — a late reply to a call that timed out — and
//! it is reported rather than dropped silently, because "the keeper answered a
//! request nobody is waiting for" is the first sign of a sequence bug.

use std::sync::Weak;
use std::time::Duration;

use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::ExitFrame;

use super::DISPATCH_IDLE;
use super::pool::KeeperPool;

/// Deliver until the pool is gone.
pub(super) fn dispatch_loop(pool: &Weak<KeeperPool>) {
    while let Some(pool) = pool.upgrade() {
        let delivered = pool.dispatch_ready();
        // The strong reference is dropped before the sleep: a pool nobody holds
        // must be able to end, and sleeping with it alive is what would keep a
        // retired worker from ever stopping this thread.
        drop(pool);
        if delivered == 0 {
            std::thread::sleep(DISPATCH_IDLE);
        }
    }
    tracing::debug!("the keeper dispatch loop stopped");
}

impl KeeperPool {
    /// Deliver everything that has already arrived, and report how much.
    ///
    /// Non-blocking by construction: the drain holds the connection handle for
    /// exactly the frames already queued, and never waits inside it. A loop
    /// that held the handle across a wait would put every keystroke behind the
    /// keeper's silence.
    pub(crate) fn dispatch_ready(&self) -> usize {
        let frames = self.keeper.with(|client| {
            let mut arrived = Vec::new();
            while let Some(frame) = client.next_event(Duration::ZERO) {
                arrived.push(frame);
            }
            arrived
        });
        let delivered = frames.len();
        for frame in frames {
            self.route(frame);
        }
        delivered
    }

    /// Hand one frame to the session it belongs to.
    fn route(&self, frame: MuxFrame) {
        match frame.frame_type {
            MuxFrameType::PtyOut => match self.channels.output_for(frame.channel_id) {
                Some(output) => output.on_output(&frame.payload),
                // Bytes for a channel this worker does not drive: an adopted
                // keeper's channel before adoption, or the tail after an exit.
                None => tracing::debug!(
                    channel_id = frame.channel_id,
                    bytes = frame.payload.len(),
                    "the keeper sent output for a channel this worker does not drive"
                ),
            },
            MuxFrameType::Exit => {
                let exit_code = match frame.parse_json::<ExitFrame>() {
                    Some(exit) => exit.exit_code,
                    // The keeper said the child ended and the payload did not
                    // decode. Reporting that as exit 0 would be inventing an
                    // answer; the channel still ends, because the keeper is
                    // authoritative about its own children.
                    None => {
                        tracing::error!(
                            channel_id = frame.channel_id,
                            "the keeper's exit frame did not decode"
                        );
                        None
                    }
                };
                self.end_channel(frame.channel_id, exit_code);
            }
            other => tracing::debug!(
                channel_id = frame.channel_id,
                ?other,
                "the keeper sent a frame no request of this pool is waiting for"
            ),
        }
    }

    /// End a channel exactly once, whoever gets there first.
    ///
    /// The claim is the table's decision, not the caller's: a connection that
    /// dies while an exit is in flight must not produce an exit AND an error
    /// for one channel, and the loser of that race is the one that finds the
    /// channel already gone.
    fn end_channel(&self, channel_id: u16, exit_code: Option<i32>) {
        match self.channels.claim_exit(channel_id) {
            Some(output) => {
                output.on_exit(exit_code);
                tracing::info!(channel_id, ?exit_code, "a keeper channel ended");
            }
            None => tracing::debug!(
                channel_id,
                ?exit_code,
                "an exit arrived for a channel this worker has already ended"
            ),
        }
    }
}
