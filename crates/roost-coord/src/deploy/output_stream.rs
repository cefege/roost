//! A bounded per-subscriber queue over one `BoundedBus`, and the deploy output
//! read built on it: a job's buffered lines, then its live lines, until `done`.
//! Called by `deploy::rpc_deploy` (the `WorkersDeployOutput` stream) and by
//! `deploy::catchup` (the outcome watcher). Depends on `events::bus` and
//! `deploy::jobs`. Ports apps/coord/src/sync/sse.ts and the `deployOutput`
//! generator of apps/coord/src/deploy/deploy-jobs.ts.
//!
//! THE QUEUE IS BOUNDED BY FRAMES AND BYTES, as the Sync application window is:
//! a stalled HTTP reader must meet the same close-for-backpressure a stalled
//! socket does, so an overflow ends the subscription and the reader is told,
//! rather than the coordinator buffering a deploy's whole output per reader.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::Stream;
use roost_observability::{LogFields, SignalKind};
use tokio::sync::Notify;

use crate::deploy::jobs::{DeployJournal, DeployStreamMsg, is_deploy_job_id};
use crate::events::bus::{BoundedBus, Subscription};
use crate::sync_ws::ack_window::{MAX_UNACKED_BYTES, MAX_UNACKED_FRAMES};

/// The subscriber fell further behind than its bound allows; the subscription
/// is over and the reader must reopen to resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("sse subscriber queue overflowed ({frames} frames, {bytes} bytes)")]
pub struct SubscriberQueueOverflow {
    /// Messages queued when the bound tripped.
    pub frames: usize,
    /// Their weight when the bound tripped.
    pub bytes: u64,
}

/// How far one subscriber may fall behind.
#[derive(Debug, Clone, Copy)]
pub struct QueueBounds<T> {
    /// Queued messages allowed before the subscription ends.
    pub max_frames: usize,
    /// Queued weight allowed before the subscription ends.
    pub max_bytes: u64,
    /// One message's weight; a function returning zero leaves only the frame cap.
    pub size_of: fn(&T) -> u64,
}

impl<T> QueueBounds<T> {
    /// The Sync application window's magnitudes, weighted by `size_of`.
    #[must_use]
    pub fn application_window(size_of: fn(&T) -> u64) -> Self {
        Self {
            max_frames: MAX_UNACKED_FRAMES,
            max_bytes: MAX_UNACKED_BYTES,
            size_of,
        }
    }
}

/// One subscriber's bounded queue, fed by the bus it subscribed to.
///
/// Dropping it ends the subscription, which is how an abandoned reader
/// unsubscribes: there is no abort signal to forget to wire.
pub struct BusSubscriberQueue<T> {
    shared: Arc<QueueShared<T>>,
    subscription: Option<Subscription<T>>,
}

impl<T> std::fmt::Debug for BusSubscriberQueue<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BusSubscriberQueue")
            .field("subscribed", &self.subscription.is_some())
            .finish()
    }
}

struct QueueShared<T> {
    state: Mutex<QueueState<T>>,
    arrived: Notify,
    bounds: QueueBounds<T>,
}

struct QueueState<T> {
    queue: VecDeque<T>,
    queued_bytes: u64,
    overflow: Option<SubscriberQueueOverflow>,
}

impl<T: Clone + Send + Sync + 'static> BusSubscriberQueue<T> {
    /// Subscribe to `bus`. Only messages published from now on are queued; the
    /// bus's ring is never replayed.
    pub fn subscribe(bus: &BoundedBus<T>, bounds: QueueBounds<T>) -> Self {
        let shared = Arc::new(QueueShared {
            state: Mutex::new(QueueState {
                queue: VecDeque::new(),
                queued_bytes: 0,
                overflow: None,
            }),
            arrived: Notify::new(),
            bounds,
        });
        let listener = Arc::clone(&shared);
        let subscription = bus.subscribe(move |message| listener.enqueue(message));
        Self {
            shared,
            subscription: Some(subscription),
        }
    }

    /// The next message, waiting for one, or the overflow that ended this
    /// subscription. An overflow wins over anything still queued: the reader is
    /// already behind, and the tail it would read next is not contiguous.
    pub async fn next_message(&mut self) -> Result<T, SubscriberQueueOverflow> {
        loop {
            let arrived = self.shared.arrived.notified();
            {
                let mut state = self.shared.lock();
                if let Some(overflow) = state.overflow {
                    drop(state);
                    self.subscription = None;
                    return Err(overflow);
                }
                if let Some(message) = state.queue.pop_front() {
                    let weight = (self.shared.bounds.size_of)(&message);
                    state.queued_bytes = state.queued_bytes.saturating_sub(weight);
                    return Ok(message);
                }
            }
            arrived.await;
        }
    }
}

impl<T: Clone> QueueShared<T> {
    fn lock(&self) -> MutexGuard<'_, QueueState<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The bus listener. Runs inside `publish`, so it only queues.
    fn enqueue(&self, message: &T) {
        let tripped = {
            let mut state = self.lock();
            if state.overflow.is_some() {
                return;
            }
            state.queued_bytes += (self.bounds.size_of)(message);
            state.queue.push_back(message.clone());
            if state.queue.len() > self.bounds.max_frames
                || state.queued_bytes > self.bounds.max_bytes
            {
                let overflow = SubscriberQueueOverflow {
                    frames: state.queue.len(),
                    bytes: state.queued_bytes,
                };
                state.overflow = Some(overflow);
                state.queue.clear();
                Some(overflow)
            } else {
                None
            }
        };
        if let Some(overflow) = tripped {
            tracing::warn!(
                frames = overflow.frames,
                bytes = overflow.bytes,
                "stream subscriber: queue_overflow; ending the subscription"
            );
            // The Sync high-water kind is exactly this failure shape, so the
            // vocabulary is shared rather than grown.
            roost_observability::signal::emit(
                SignalKind::SyncQueueOverflow,
                LogFields::new()
                    .set("frames", overflow.frames)
                    .set("bytes", overflow.bytes)
                    .set("max_frames", self.bounds.max_frames)
                    .set("max_bytes", self.bounds.max_bytes),
            );
        }
        self.arrived.notify_one();
    }
}

/// One reader's view of one deploy job: what was already written, then what
/// the job writes next, ending at the job's `done`.
#[derive(Debug)]
pub struct DeployOutput {
    buffered: VecDeque<DeployStreamMsg>,
    live: Option<BusSubscriberQueue<DeployStreamMsg>>,
    finished: bool,
}

impl DeployOutput {
    /// A read that is already complete: the buffered messages and nothing live.
    #[must_use]
    pub(crate) fn settled(buffered: VecDeque<DeployStreamMsg>) -> Self {
        Self {
            buffered,
            live: None,
            finished: false,
        }
    }

    /// A read of a running job: its lines so far, then its bus.
    #[must_use]
    pub(crate) fn following(
        buffered: VecDeque<DeployStreamMsg>,
        live: BusSubscriberQueue<DeployStreamMsg>,
    ) -> Self {
        Self {
            buffered,
            live: Some(live),
            finished: false,
        }
    }

    /// The next message; `None` once `done` has been read.
    pub async fn next_message(
        &mut self,
    ) -> Option<Result<DeployStreamMsg, SubscriberQueueOverflow>> {
        if self.finished {
            return None;
        }
        let next = match self.buffered.pop_front() {
            Some(message) => Ok(message),
            None => match self.live.as_mut() {
                Some(live) => live.next_message().await,
                None => {
                    self.finished = true;
                    return None;
                }
            },
        };
        if !matches!(next, Ok(DeployStreamMsg::Line(_))) {
            self.finished = true;
            self.live = None;
        }
        Some(next)
    }

    /// The same read as a stream, for a server-streaming response.
    pub fn into_stream(
        self,
    ) -> impl Stream<Item = Result<DeployStreamMsg, SubscriberQueueOverflow>> + Send {
        futures_util::stream::unfold(self, |mut output| async move {
            let next = output.next_message().await?;
            Some((next, output))
        })
    }
}

/// Weight a deploy message so a flood of long lines trips the byte cap before
/// the frame cap; the character count approximates the UTF-8 cost closely
/// enough for a bound.
fn deploy_message_weight(message: &DeployStreamMsg) -> u64 {
    match message {
        DeployStreamMsg::Line(text) => u64::try_from(text.chars().count()).unwrap_or(u64::MAX),
        DeployStreamMsg::Done { .. } => 32,
    }
}

/// The bound every deploy output reader gets.
#[must_use]
pub(crate) fn deploy_output_bounds() -> QueueBounds<DeployStreamMsg> {
    QueueBounds::application_window(deploy_message_weight)
}

/// Open one job's output. An id that is not a job id, or names no job this
/// coordinator holds, reads as one `done` carrying `unknown jobId`.
#[must_use]
pub fn open_deploy_output(journal: &DeployJournal, job_id: &str) -> DeployOutput {
    let job = if is_deploy_job_id(job_id) {
        journal.job(job_id)
    } else {
        None
    };
    match job {
        Some(job) => job.open_output(),
        None => {
            tracing::info!(job_id, "deploy output: unknown job");
            DeployOutput::settled(VecDeque::from([DeployStreamMsg::Done {
                exit: None,
                error: Some("unknown jobId".to_owned()),
            }]))
        }
    }
}
