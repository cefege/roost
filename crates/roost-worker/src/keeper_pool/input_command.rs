//! The worker's half of acknowledged keeper input: a worker-owned input
//! sequence per channel, the table of written batches awaiting their result
//! frame, the per-channel caps, and the bounded wait. `pool::KeeperPool` writes
//! through it, `dispatch` settles it from the keeper's result frames, and a lost
//! keeper settles every pending batch as ambiguous. Ports `channelInputCommand`
//! and `settlePendingInput` of v2 `apps/worker/src/keeper/keeper-pool-io.ts`
//! and `beginInput` of `apps/worker/src/keeper/multiplexed-client.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use roost_keeper::codec::MuxFrame;
use roost_keeper::payloads::{PtyInRejectReason, PtyInResult};
use tokio::sync::oneshot;

use crate::session::keeper_channels::{InputNotWritten, KeeperInputCommand, KeeperInputResult};

/// How long a written batch waits for its result before it is ambiguous.
/// v2 keeper-pool-io.ts `COMMAND_RESULT_TIMEOUT_MS`.
pub const COMMAND_RESULT_TIMEOUT: Duration = Duration::from_millis(2_500);
/// Written-and-unanswered batches one channel may hold (v2 `MAX_PENDING_INPUT_COMMANDS`).
pub const MAX_PENDING_INPUT_COMMANDS: u32 = 200;
/// Their bytes (v2 `MAX_PENDING_INPUT_BYTES`).
pub const MAX_PENDING_INPUT_BYTES: u64 = 256 * 1024;

/// What one channel has written and not yet heard back about, for pipeline
/// evidence (v2 `pendingInputs` / `_pendingInputUsage`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingInputUsage {
    pub started: Vec<Instant>,
    pub commands: u32,
    pub bytes: u64,
}

struct Waiter {
    expected: u32,
    started: Instant,
    answer: oneshot::Sender<KeeperInputResult>,
}

#[derive(Default)]
struct PendingState {
    waiters: HashMap<(u16, u64), Waiter>,
    /// The last sequence handed out per channel. Worker-owned: a browser's
    /// input_seq never reaches the keeper, so two senders may both use 1.
    last_seq: HashMap<u16, u64>,
}

/// Written batches awaiting their keeper result, keyed by channel and sequence.
#[derive(Default)]
pub(crate) struct PendingInputs {
    state: Mutex<PendingState>,
}

impl PendingInputs {
    /// Claim the next free sequence for a batch of `len` bytes, or refuse it
    /// against the channel's caps. Called with the keeper connection held, so no
    /// result frame can be dispatched before [`PendingInputs::register`].
    pub(crate) fn reserve(&self, channel_id: u16, len: usize) -> Result<u64, InputNotWritten> {
        let mut state = self.lock();
        let usage = usage_of(&state, channel_id);
        if usage.commands >= MAX_PENDING_INPUT_COMMANDS
            || usage.bytes + len as u64 > MAX_PENDING_INPUT_BYTES
        {
            tracing::warn!(
                channel_id,
                commands = usage.commands,
                bytes = usage.bytes,
                "keeper input: pending caps are full"
            );
            return Err(InputNotWritten::QueueFull);
        }
        let mut seq = next_seq(state.last_seq.get(&channel_id).copied().unwrap_or(0));
        while state.waiters.contains_key(&(channel_id, seq)) {
            seq = next_seq(seq);
        }
        state.last_seq.insert(channel_id, seq);
        Ok(seq)
    }

    /// Record a batch that is now on the socket, and return its result half.
    pub(crate) fn register(
        self: &Arc<Self>,
        channel_id: u16,
        input_seq: u64,
        expected: u32,
    ) -> KeeperInputCommand {
        let (answer, receiver) = oneshot::channel();
        let waiter = Waiter {
            expected,
            started: Instant::now(),
            answer,
        };
        self.lock().waiters.insert((channel_id, input_seq), waiter);
        // Moved into the future so dropping it unpolled still clears the entry.
        let guard = PendingGuard {
            table: Arc::clone(self),
            key: (channel_id, input_seq),
        };
        let result = Box::pin(async move {
            let settled = match tokio::time::timeout(COMMAND_RESULT_TIMEOUT, receiver).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => ambiguous("disconnected"),
                Err(_) => {
                    tracing::warn!(
                        channel_id,
                        input_seq,
                        "keeper input: no result within the command timeout"
                    );
                    ambiguous("timeout")
                }
            };
            drop(guard);
            settled
        });
        KeeperInputCommand {
            admission: Ok(()),
            result,
        }
    }

    /// Settle a batch from the keeper's result frame; `false` when nobody waits.
    pub(crate) fn settle_frame(&self, frame: &MuxFrame) -> bool {
        let Some(received) = PtyInResult::decode(frame.frame_type, &frame.payload) else {
            tracing::warn!(
                channel_id = frame.channel_id,
                "keeper input: a result frame did not decode"
            );
            return false;
        };
        let (input_seq, result) = match received {
            PtyInResult::Ack { input_seq, written } => {
                (input_seq, KeeperInputResult::Ack { written })
            }
            PtyInResult::Reject { input_seq, reason } => (
                input_seq,
                KeeperInputResult::Reject {
                    reason: wire_reason(reason).to_owned(),
                },
            ),
            PtyInResult::Ambiguous {
                input_seq,
                written,
                reason,
            } => (
                input_seq,
                KeeperInputResult::Ambiguous {
                    written: Some(written),
                    reason: wire_reason(reason).to_owned(),
                },
            ),
        };
        let settled = self.settle(frame.channel_id, input_seq, result);
        if !settled {
            tracing::debug!(
                channel_id = frame.channel_id,
                input_seq,
                "keeper input: a result had no waiter"
            );
        }
        settled
    }

    /// Hand one batch its result. A count the request cannot have produced is
    /// ambiguous rather than believed (v2 `settlePendingInput`).
    fn settle(&self, channel_id: u16, input_seq: u64, received: KeeperInputResult) -> bool {
        let Some(waiter) = self.lock().waiters.remove(&(channel_id, input_seq)) else {
            return false;
        };
        let contradicts = match &received {
            KeeperInputResult::Ack { written } => *written != waiter.expected,
            KeeperInputResult::Ambiguous {
                written: Some(written),
                ..
            } => *written > waiter.expected,
            KeeperInputResult::Reject { .. }
            | KeeperInputResult::Ambiguous { written: None, .. } => false,
        };
        let result = if contradicts {
            tracing::warn!(
                channel_id,
                input_seq,
                ?received,
                "keeper input: a result contradicts its request"
            );
            ambiguous("protocol_error")
        } else {
            received
        };
        tracing::debug!(
            channel_id,
            input_seq,
            ?result,
            "keeper input: a batch settled"
        );
        // The receiver is gone only when its caller stopped waiting.
        let _ = waiter.answer.send(result);
        true
    }

    /// The connection is gone: every written batch is unknowable now.
    pub(crate) fn settle_all_disconnected(&self) {
        let waiters: Vec<Waiter> = self
            .lock()
            .waiters
            .drain()
            .map(|(_, waiter)| waiter)
            .collect();
        if !waiters.is_empty() {
            tracing::warn!(
                pending = waiters.len(),
                "keeper input: pending batches lost with the connection"
            );
        }
        for waiter in waiters {
            let _ = waiter.answer.send(ambiguous("disconnected"));
        }
    }

    /// A closed channel's sequence starts over (v2 `forgetInputSequence`).
    pub(crate) fn forget_channel(&self, channel_id: u16) {
        self.lock().last_seq.remove(&channel_id);
    }

    pub(crate) fn usage(&self, channel_id: u16) -> PendingInputUsage {
        usage_of(&self.lock(), channel_id)
    }

    fn lock(&self) -> MutexGuard<'_, PendingState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct PendingGuard {
    table: Arc<PendingInputs>,
    key: (u16, u64),
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.table.lock().waiters.remove(&self.key);
    }
}

fn usage_of(state: &PendingState, channel_id: u16) -> PendingInputUsage {
    let mut usage = PendingInputUsage::default();
    for ((channel, _), waiter) in &state.waiters {
        if *channel == channel_id {
            usage.started.push(waiter.started);
            usage.commands += 1;
            usage.bytes += u64::from(waiter.expected);
        }
    }
    usage
}

fn next_seq(previous: u64) -> u64 {
    previous.checked_add(1).unwrap_or(1)
}

fn ambiguous(reason: &str) -> KeeperInputResult {
    KeeperInputResult::Ambiguous {
        written: None,
        reason: reason.to_owned(),
    }
}

/// The keeper's refusal reason as v2 spells it on the result.
fn wire_reason(reason: PtyInRejectReason) -> &'static str {
    match reason {
        PtyInRejectReason::NoSuchChannel => "channel_missing",
        PtyInRejectReason::NoReader => "write_error",
        PtyInRejectReason::QueueFull => "queue_full",
        PtyInRejectReason::ChildExited => "channel_exited",
        PtyInRejectReason::PartialWrite => "partial_write",
    }
}
