//! What the keeper sends that is not an answer to a blocking request: PTY bytes
//! and exits, delivered to the session each belongs to, and acknowledged-input
//! results, settled into `input_command`'s waiters. `pool::KeeperPool` starts
//! the loop in its constructor; nothing else drives it. Depends on the pool and
//! the keeper's frame types — nothing here. A connection the keeper closed is
//! seen here first and reported as a lost keeper (v2's socket `close` handler).
//!
//! THE LOOP OWNS THE EVENT STREAM. The client hands a frame to whoever is
//! waiting for it and drops the rest here, so there is exactly one reader: a
//! second one would steal a reply from the request that is blocked on it, and a
//! request that loses its reply looks precisely like a wedged keeper.
//!
//! A BLOCKING REQUEST'S ANSWER IS NEVER DISPATCHED. A frame that answers one is
//! consumed inside the call, while the connection handle is held, and the
//! dispatcher cannot run at the same time. So every such answer seen here
//! belongs to a request that already returned — a late reply to a call that
//! timed out — and it is reported rather than dropped silently. Acknowledged
//! input is the one request that does not block: its waiter is registered
//! under the handle, before the dispatcher can see its answer.
//!
//! A PASS HOLDS `routing` FROM TAKE TO LAST DELIVERY, so a history read at an
//! ordered boundary (`pool_history`) never races a batch taken before it.
//! Ports v2 `apps/worker/src/keeper/keeper-pool-lifecycle.ts`.

use std::sync::{PoisonError, Weak};
use std::time::Duration;

use roost_keeper::client::KeeperClient;
use roost_keeper::codec::{MuxFrame, MuxFrameType};
use roost_keeper::frames::ExitFrame;

use super::pool::KeeperPool;

/// The longest the dispatch loop waits for the keeper's reader to ring when
/// the keeper said nothing.
///
/// The wait ends the moment a frame arrives, so this bounds only how long an
/// idle loop goes without re-checking the pool and the connection — not how
/// long output or an input acknowledgement sits undelivered. An echo that
/// waited out a poll would land after the predictive overlay's next guess.
pub const DISPATCH_IDLE: Duration = Duration::from_millis(16);

/// Deliver until the pool is gone.
pub(super) fn dispatch_loop(pool: &Weak<KeeperPool>) {
    while let Some(pool) = pool.upgrade() {
        let delivered = pool.dispatch_ready();
        // Taken under the handle, waited on outside it: a wait that held the
        // handle would put every keystroke behind the keeper's silence.
        let arrival = (delivered == 0).then(|| pool.keeper.with(KeeperClient::arrival_bell));
        // The strong reference is dropped before the wait: a pool nobody holds
        // must be able to end, and waiting with it alive is what would keep a
        // retired worker from ever stopping this thread.
        drop(pool);
        if let Some(arrival) = arrival {
            arrival.wait(DISPATCH_IDLE);
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
        let routing = self.routing.lock().unwrap_or_else(PoisonError::into_inner);
        let arrived = self.take_arrived_frames();
        let delivered = arrived.frames.len();
        for frame in arrived.frames {
            self.route(frame);
        }
        drop(routing);
        if arrived.closed && self.is_connected() {
            self.keeper_lost("the keeper closed its connection".to_owned());
        }
        delivered
    }

    /// Hand one frame to the session it belongs to.
    fn route(&self, frame: MuxFrame) {
        match frame.frame_type {
            MuxFrameType::PtyOut => match self.output_binding_for(frame.channel_id) {
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
            // An acknowledged batch's answer. It arrives here rather than inside
            // a request because the writer released the connection once the
            // batch was on the socket; its waiter was registered before that.
            MuxFrameType::PtyInAck | MuxFrameType::PtyInReject | MuxFrameType::PtyInAmbiguous => {
                self.pending_inputs.settle_frame(&frame);
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
    /// dies while an exit is in flight must not produce two endings for one
    /// channel, and the loser of that race is the one that finds it gone.
    fn end_channel(&self, channel_id: u16, exit_code: Option<i32>) {
        match self.claim_channel_exit(channel_id) {
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
