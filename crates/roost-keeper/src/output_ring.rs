//! A channel's output ring: the hand-off between a blocking PTY read and the
//! keeper's frame loop. Owned by the keeper.
//!
//! A read on a pty master BLOCKS until bytes arrive. Doing that on the
//! keeper's own loop would wedge every other channel behind the slowest one,
//! so each channel reads on its own thread and hands chunks over a bounded
//! queue. The bound is what stops a chatty program from growing the keeper's
//! memory with output that is never delivered. Every hand-over raises the
//! keeper's [`OutputSignal`], which wakes the serving connection so the chunk
//! is forwarded as it arrives rather than at the loop's next drain tick.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};

/// How many unread output chunks a channel may hold before its reader blocks.
pub const OUTPUT_QUEUE_DEPTH: usize = 256;

/// The read size each blocking read asks for. Small enough that one program
/// writing a megabyte at a time cannot make the keeper hold the whole burst.
pub const READ_CHUNK_BYTES: usize = 16 * 1024;

pub(crate) struct OutputChunk {
    pub(crate) bytes: Vec<u8>,
}

/// What a reader thread tells the serving connection: "a channel has output".
///
/// One per keeper, shared by every channel's reader. A raise is coalesced —
/// only the first one after the loop's [`OutputSignal::take`] calls the
/// connection's waker — so a flooding program costs the loop one wake per
/// drain, not one per chunk. Between connections there is no waker and a raise
/// only sets the flag; the next connection drains on its first turn anyway.
#[derive(Default)]
pub struct OutputSignal {
    raised: AtomicBool,
    waker: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl OutputSignal {
    /// A reader handed over a chunk, or its child let go of the PTY.
    pub(crate) fn raise(&self) {
        if self.raised.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(wake) = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            wake();
        }
    }

    /// Clear the flag before a drain, so a chunk that lands after it raises
    /// again. Returns whether anything had been raised.
    pub fn take(&self) -> bool {
        self.raised.swap(false, Ordering::AcqRel)
    }

    /// Route raises to the connection now being served. The waker must not
    /// block: it runs on a reader thread, holding this signal's lock.
    pub fn attach(&self, waker: Box<dyn Fn() + Send>) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(waker);
    }

    /// The connection ended; raises only set the flag until the next attach.
    pub fn detach(&self) {
        *self.waker.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

impl std::fmt::Debug for OutputSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputSignal")
            .field("raised", &self.raised.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

/// The receiving end of a channel's output ring.
pub struct OutputRing {
    chunks: Receiver<OutputChunk>,
    /// A chunk larger than the caller's limit, held so the next call can
    /// continue it rather than splitting a frame across two reads.
    pending: VecDeque<u8>,
}

impl OutputRing {
    pub(crate) fn new(chunks: Receiver<OutputChunk>) -> Self {
        Self {
            chunks,
            pending: VecDeque::new(),
        }
    }

    /// Take up to `limit` bytes, or `None` when nothing is available right
    /// now. `None` is not EOF; the caller asks [`OutputRing::is_eof`] for that,
    /// because a PTY read that returned zero before the child exited would be
    /// indistinguishable from a closed channel.
    pub fn take(&mut self, limit: usize) -> Option<Vec<u8>> {
        while self.pending.len() < limit {
            match self.chunks.try_recv() {
                Ok(chunk) => self.pending.extend(chunk.bytes),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        if self.pending.is_empty() {
            return None;
        }
        let take = limit.min(self.pending.len());
        Some(self.pending.drain(..take).collect())
    }

    /// True once the child closed the slave end and everything it wrote has
    /// been handed over.
    ///
    /// The reader thread returning IS the EOF signal: it drops the sender on
    /// the way out, so a drained queue reports `Disconnected`. Peeking with
    /// `try_iter` instead would consume the chunks it was trying to measure,
    /// which is the one thing that must not happen to buffered output.
    pub fn is_eof(&mut self) -> bool {
        if !self.pending.is_empty() {
            return false;
        }
        match self.chunks.try_recv() {
            Err(TryRecvError::Disconnected) => true,
            Ok(chunk) => {
                self.pending.extend(chunk.bytes);
                false
            }
            Err(TryRecvError::Empty) => false,
        }
    }

    /// Whether nothing is left after the last [`OutputRing::take`]: neither a
    /// remainder its limit deferred nor a chunk still queued. Both were
    /// raised before the drain that left them, so no reader will raise for
    /// them again. A queued chunk is moved into `pending` to be seen, in
    /// order, as [`OutputRing::is_eof`] does. It does not distinguish
    /// "nothing yet" from "the child is gone".
    pub fn is_drained(&mut self) -> bool {
        if !self.pending.is_empty() {
            return false;
        }
        match self.chunks.try_recv() {
            Ok(chunk) => {
                self.pending.extend(chunk.bytes);
                false
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => true,
        }
    }
}

// The `Receiver` half is omitted because it has no `Debug`. What is left is
// the partial chunk, which is the only state here a reader could act on.
impl std::fmt::Debug for OutputRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputRing")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

/// Drain a PTY into a channel's ring until the child closes the slave end.
///
/// The read blocks by design — that is what a pty read does — and the queue is
/// bounded, so a program that outruns the keeper applies back pressure rather
/// than growing the keeper's memory.
fn pump_pty_output(
    mut reader: Box<dyn std::io::Read + Send>,
    sender: SyncSender<OutputChunk>,
    signal: &OutputSignal,
) {
    // One read buffer for the thread's life; each chunk is an exact-size copy,
    // so the ring holds the bytes read rather than a 16 KiB block per chunk.
    let mut buffer = vec![0u8; READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut buffer) {
            // Zero bytes on a pty means the child let go of the slave end.
            // Returning drops the sender, which is the EOF signal.
            Ok(0) => return,
            Ok(read) => {
                let bytes = buffer[..read].to_vec();
                if sender.send(OutputChunk { bytes }).is_err() {
                    return;
                }
                signal.raise();
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            // A real read error on a pty is terminal: the master is gone, and
            // returning is the only honest thing left to do.
            Err(_) => return,
        }
    }
}

/// Start a channel's reader thread and hand back its ring. Every chunk it
/// hands over, and its EOF, raises `signal`.
pub fn spawn_reader(
    channel_id: u16,
    reader: Box<dyn std::io::Read + Send>,
    signal: Arc<OutputSignal>,
) -> Result<(OutputRing, std::thread::JoinHandle<()>), std::io::Error> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(OUTPUT_QUEUE_DEPTH);
    let handle = std::thread::Builder::new()
        .name(format!("roost-keeper-pty-{channel_id}"))
        .spawn(move || {
            pump_pty_output(reader, sender, &signal);
            // After the sender is dropped, so the woken loop already sees EOF
            // and reports the exit without waiting for its tick.
            signal.raise();
        })?;
    Ok((OutputRing::new(receiver), handle))
}
