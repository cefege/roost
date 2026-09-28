//! The keeper's PTY-input lane: one FIFO per channel, shared by the legacy
//! `PtyIn` and the acknowledged `PtyInRequest` so the two can never interleave
//! mid-batch, bounded by a command and a byte budget, and written by a thread of
//! its own so a child that stops reading stalls only its own channel. Owned by
//! `pty_channel::PtyChannel`; `keeper_ops` enqueues and `server` attaches the
//! connection results go to. Ports `apps/worker/src/keeper/keeper-input-queue.ts`.

use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::codec::{MuxFrame, MuxFrameType};
use crate::payloads::{PtyInRejectReason, PtyInResult};
use crate::pty_channel::WriteOutcome;

/// Queued plus in-flight batches one channel may hold (v2 keeper-input-queue.ts:16).
pub const KEEPER_INPUT_QUEUE_MAX_COMMANDS: usize = 200;
/// Queued plus in-flight bytes one channel may hold (v2 keeper-input-queue.ts:17).
pub const KEEPER_INPUT_QUEUE_MAX_BYTES: usize = 256 * 1024;

/// Where a lane writes the result frame of an acknowledged batch.
pub trait InputResultSink: Send + Sync {
    /// Write one result frame to the connection that asked; `false` once it is gone.
    fn deliver(&self, frame: &MuxFrame) -> bool;
}

/// The one writer a connection has. The server loop and every input lane write
/// through it, so a result frame can never land inside an output frame.
#[derive(Debug)]
pub struct ConnectionWriter {
    stream: Mutex<UnixStream>,
}

impl ConnectionWriter {
    /// A writer over its own handle to `stream`, so the reading loop keeps the original.
    pub fn for_stream(stream: &UnixStream) -> std::io::Result<Self> {
        let write_half = stream.try_clone().inspect_err(|error| {
            tracing::error!(%error, "keeper: the connection's write half could not be opened");
        })?;
        Ok(Self {
            stream: Mutex::new(write_half),
        })
    }

    /// Write frames whole, in order, under the connection's one lock.
    pub fn write_frames(&self, frames: &[MuxFrame]) -> std::io::Result<()> {
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for frame in frames {
            stream.write_all(&frame.encode())?;
        }
        stream.flush()
    }
}

impl InputResultSink for ConnectionWriter {
    fn deliver(&self, frame: &MuxFrame) -> bool {
        self.write_frames(std::slice::from_ref(frame)).is_ok()
    }
}

/// The connection acknowledged input answers on. A generation per attach is
/// what lets a lane recognise a batch whose connection has already gone.
#[derive(Default)]
pub struct InputRoute {
    attached: Mutex<Option<(u64, Arc<dyn InputResultSink>)>>,
    generations: AtomicU64,
}

impl std::fmt::Debug for InputRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputRoute")
            .field("current", &self.current())
            .finish()
    }
}

impl InputRoute {
    /// Route results to a new connection, returning its generation.
    pub fn attach(&self, sink: Arc<dyn InputResultSink>) -> u64 {
        let generation = self.generations.fetch_add(1, Ordering::AcqRel) + 1;
        *self.lock() = Some((generation, sink));
        tracing::info!(
            generation,
            "keeper: input results route to a new connection"
        );
        generation
    }

    /// The connection is gone: its unstarted batches are dropped, not written.
    pub fn detach(&self) {
        if let Some((generation, _)) = self.lock().take() {
            tracing::info!(generation, "keeper: the input-result connection detached");
        }
    }

    /// The generation results currently go to, if any connection is attached.
    pub fn current(&self) -> Option<u64> {
        self.lock().as_ref().map(|(generation, _)| *generation)
    }

    fn deliver(&self, generation: u64, frame: &MuxFrame) -> bool {
        let sink = match self.lock().as_ref() {
            Some((current, sink)) if *current == generation => Arc::clone(sink),
            _ => return false,
        };
        sink.deliver(frame)
    }

    fn lock(&self) -> MutexGuard<'_, Option<(u64, Arc<dyn InputResultSink>)>> {
        self.attached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Who a batch's result is owed to.
#[derive(Debug)]
pub enum InputReply {
    /// Legacy `PtyIn`: nothing is owed and nothing is dropped with a connection.
    Unacknowledged,
    /// `PtyInRequest`: one result frame, to the connection generation that asked.
    /// `None` when no connection was attached (a dispatcher driven directly).
    Acknowledged {
        input_seq: u64,
        route: Arc<InputRoute>,
        generation: Option<u64>,
    },
}

struct Batch {
    bytes: Vec<u8>,
    reply: InputReply,
}

#[derive(Default)]
struct LaneState {
    queue: VecDeque<Batch>,
    /// Queued plus in-flight bytes, released only once a batch has settled.
    held_bytes: usize,
    writing: bool,
    closed: bool,
}

struct LaneShared {
    channel_id: u16,
    state: Mutex<LaneState>,
    ready: Condvar,
    exited: AtomicBool,
}

impl LaneShared {
    fn lock(&self) -> MutexGuard<'_, LaneState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// One channel's input FIFO and the thread that writes it to the PTY.
pub struct InputLane {
    shared: Arc<LaneShared>,
}

impl std::fmt::Debug for InputLane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InputLane")
            .field("channel_id", &self.shared.channel_id)
            .field("exited", &self.shared.exited.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl InputLane {
    /// Start the writer thread over the PTY's write half.
    pub fn start(channel_id: u16, writer: Box<dyn Write + Send>) -> std::io::Result<Self> {
        let shared = Arc::new(LaneShared {
            channel_id,
            state: Mutex::new(LaneState::default()),
            ready: Condvar::new(),
            exited: AtomicBool::new(false),
        });
        let lane = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(format!("roost-keeper-input-{channel_id}"))
            .spawn(move || drain_lane(&lane, writer))?;
        Ok(Self { shared })
    }

    /// Queue one batch behind every earlier one, or refuse it when the channel's
    /// budget is spent. Refusal is the only outcome that proves nothing was written.
    pub fn enqueue(&self, bytes: Vec<u8>, reply: InputReply) -> bool {
        let mut state = self.shared.lock();
        let commands = state.queue.len() + usize::from(state.writing);
        if commands >= KEEPER_INPUT_QUEUE_MAX_COMMANDS
            || state.held_bytes + bytes.len() > KEEPER_INPUT_QUEUE_MAX_BYTES
        {
            tracing::warn!(
                channel_id = self.shared.channel_id,
                commands,
                held_bytes = state.held_bytes,
                bytes = bytes.len(),
                "keeper: the channel's input queue is full"
            );
            return false;
        }
        state.held_bytes += bytes.len();
        state.queue.push_back(Batch { bytes, reply });
        self.shared.ready.notify_one();
        true
    }

    /// The child has exited: every batch not yet written is refused.
    pub fn mark_exited(&self) {
        if !self.shared.exited.swap(true, Ordering::AcqRel) {
            tracing::info!(
                channel_id = self.shared.channel_id,
                "keeper: input lane saw the child exit"
            );
        }
    }
}

impl Drop for InputLane {
    fn drop(&mut self) {
        // The thread is not joined: a write blocked on a PTY nobody reads ends
        // when the child's side closes, and the keeper loop must not wait for it.
        self.shared.lock().closed = true;
        self.shared.ready.notify_one();
    }
}

fn drain_lane(lane: &LaneShared, mut writer: Box<dyn Write + Send>) {
    while let Some(batch) = next_batch(lane) {
        settle_batch(lane, &mut writer, &batch);
        let mut state = lane.lock();
        state.held_bytes -= batch.bytes.len();
        state.writing = false;
    }
    tracing::debug!(channel_id = lane.channel_id, "keeper: input lane stopped");
}

fn next_batch(lane: &LaneShared) -> Option<Batch> {
    let mut state = lane.lock();
    while state.queue.is_empty() && !state.closed {
        state = lane
            .ready
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    let batch = state.queue.pop_front()?;
    state.writing = true;
    Some(batch)
}

fn settle_batch(lane: &LaneShared, writer: &mut Box<dyn Write + Send>, batch: &Batch) {
    if let InputReply::Acknowledged {
        input_seq,
        route,
        generation: Some(generation),
    } = &batch.reply
        && route.current() != Some(*generation)
    {
        // v2 drops an unstarted batch whose socket was destroyed: nobody is
        // left to be told, and writing it would type into a session unobserved.
        tracing::info!(
            channel_id = lane.channel_id,
            input_seq,
            "keeper: dropped input from a departed connection"
        );
        return;
    }
    let closed = lane.lock().closed;
    let outcome = if closed || lane.exited.load(Ordering::Acquire) {
        WriteOutcome::Rejected {
            reason: PtyInRejectReason::ChildExited,
        }
    } else {
        write_batch(writer, &batch.bytes)
    };
    if let WriteOutcome::Rejected { reason } | WriteOutcome::Partial { reason, .. } = outcome {
        tracing::warn!(
            channel_id = lane.channel_id,
            ?reason,
            ?outcome,
            "keeper: an input batch did not complete"
        );
    }
    let InputReply::Acknowledged {
        input_seq,
        route,
        generation,
    } = &batch.reply
    else {
        return;
    };
    let (tag, result) = acknowledged_result(*input_seq, outcome);
    let delivered = match (
        generation,
        MuxFrame::new(tag, lane.channel_id, result.encode()),
    ) {
        (Some(generation), Ok(frame)) => route.deliver(*generation, &frame),
        (None, Ok(_)) => false,
        (_, Err(error)) => {
            tracing::error!(channel_id = lane.channel_id, %error, "keeper: an input result could not be framed");
            false
        }
    };
    if !delivered {
        tracing::debug!(
            channel_id = lane.channel_id,
            input_seq,
            "keeper: an input result had no connection to go to"
        );
    }
}

/// Write one batch, reporting exactly how much reached the PTY. A short write is
/// `Partial` rather than completed here, because the bytes that did land are
/// indistinguishable from the ones still queued.
fn write_batch(writer: &mut Box<dyn Write + Send>, bytes: &[u8]) -> WriteOutcome {
    if bytes.is_empty() {
        return WriteOutcome::Complete { written: 0 };
    }
    match writer.write(bytes) {
        // A buffered writer that accepted bytes is not proof they reached the
        // child, so the flush is checked and its failure reported.
        Ok(written) if written == bytes.len() => match writer.flush() {
            Ok(()) => WriteOutcome::Complete {
                written: written as u32,
            },
            Err(_) => WriteOutcome::Partial {
                written: written as u32,
                reason: PtyInRejectReason::PartialWrite,
            },
        },
        Ok(written) => WriteOutcome::Partial {
            written: written as u32,
            reason: PtyInRejectReason::PartialWrite,
        },
        Err(_) => WriteOutcome::Rejected {
            reason: PtyInRejectReason::NoReader,
        },
    }
}

fn acknowledged_result(input_seq: u64, outcome: WriteOutcome) -> (MuxFrameType, PtyInResult) {
    match outcome {
        WriteOutcome::Complete { written } => (
            MuxFrameType::PtyInAck,
            PtyInResult::Ack { input_seq, written },
        ),
        WriteOutcome::Rejected { reason } => (
            MuxFrameType::PtyInReject,
            PtyInResult::Reject { input_seq, reason },
        ),
        // A partial write is the case a client must NOT retry.
        WriteOutcome::Partial { written, reason } => (
            MuxFrameType::PtyInAmbiguous,
            PtyInResult::Ambiguous {
                input_seq,
                written,
                reason,
            },
        ),
    }
}
