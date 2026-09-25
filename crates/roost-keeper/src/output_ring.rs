//! A channel's output ring: the hand-off between a blocking PTY read and the
//! keeper's frame loop. Owned by the keeper.
//!
//! A read on a pty master BLOCKS until bytes arrive. Doing that on the
//! keeper's own loop would wedge every other channel behind the slowest one,
//! so each channel reads on its own thread and hands chunks over a bounded
//! queue. The bound is what stops a chatty program from growing the keeper's
//! memory with output that is never delivered.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};

/// How many unread output chunks a channel may hold before its reader blocks.
pub const OUTPUT_QUEUE_DEPTH: usize = 256;

/// The read size each blocking read asks for. Small enough that one program
/// writing a megabyte at a time cannot make the keeper hold the whole burst.
pub const READ_CHUNK_BYTES: usize = 16 * 1024;

pub(crate) struct OutputChunk {
    pub(crate) bytes: Vec<u8>,
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

    /// Whether anything is buffered. For diagnostics only: it does not
    /// distinguish "nothing yet" from "the child is gone".
    pub fn is_drained(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Drain a PTY into a channel's ring until the child closes the slave end.
///
/// The read blocks by design — that is what a pty read does — and the queue is
/// bounded, so a program that outruns the keeper applies back pressure rather
/// than growing the keeper's memory.
fn pump_pty_output(mut reader: Box<dyn std::io::Read + Send>, sender: SyncSender<OutputChunk>) {
    loop {
        let mut buffer = vec![0u8; READ_CHUNK_BYTES];
        match reader.read(&mut buffer) {
            // Zero bytes on a pty means the child let go of the slave end.
            // Returning drops the sender, which is the EOF signal.
            Ok(0) => return,
            Ok(read) => {
                buffer.truncate(read);
                if sender.send(OutputChunk { bytes: buffer }).is_err() {
                    return;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            // A real read error on a pty is terminal: the master is gone, and
            // returning is the only honest thing left to do.
            Err(_) => return,
        }
    }
}

/// Start a channel's reader thread and hand back its ring.
pub fn spawn_reader(
    channel_id: u16,
    reader: Box<dyn std::io::Read + Send>,
) -> Result<(OutputRing, std::thread::JoinHandle<()>), std::io::Error> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(OUTPUT_QUEUE_DEPTH);
    let handle = std::thread::Builder::new()
        .name(format!("roost-keeper-pty-{channel_id}"))
        .spawn(move || pump_pty_output(reader, sender))?;
    Ok((OutputRing::new(receiver), handle))
}
