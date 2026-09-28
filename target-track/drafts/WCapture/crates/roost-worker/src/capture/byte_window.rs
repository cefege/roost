//! The per-session window of the last [`BYTE_CAPTURE_WINDOW_BYTES`] of PTY
//! output, always on. Ports `apps/worker/src/diag/byte-capture.ts`: the
//! capture tap pushes every retained chunk with the session's `head_seq`, the
//! worker-section freeze snapshots an OWNED tail, and session teardown drops it.
//!
//! The offsets are ABSOLUTE stream positions, stamped from the `head_seq` the
//! chunk ended at, so an evicted prefix reports the window it still covers
//! rather than claiming to start at zero.

use std::collections::{HashMap, VecDeque};

use base64::Engine as _;
use roost_protocol::terminal_capture::bundle::TerminalWorkerByteCaptureTail;

use super::BYTE_CAPTURE_WINDOW_BYTES;

/// One session's retained tail and the absolute offset its last byte ends at.
#[derive(Debug)]
pub struct ByteWindow {
    bytes: VecDeque<u8>,
    end_seq: u64,
}

impl Default for ByteWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl ByteWindow {
    /// One fixed allocation per session, as v2's `SbRing` makes.
    pub fn new() -> Self {
        Self {
            bytes: VecDeque::with_capacity(BYTE_CAPTURE_WINDOW_BYTES),
            end_seq: 0,
        }
    }

    /// Append `chunk`, displacing the oldest bytes past the cap. O(chunk).
    pub fn push(&mut self, chunk: &[u8], end_seq: u64) {
        if chunk.len() >= BYTE_CAPTURE_WINDOW_BYTES {
            self.bytes.clear();
            self.bytes
                .extend(&chunk[chunk.len() - BYTE_CAPTURE_WINDOW_BYTES..]);
        } else {
            let overflow =
                (self.bytes.len() + chunk.len()).saturating_sub(BYTE_CAPTURE_WINDOW_BYTES);
            self.bytes.drain(..overflow);
            self.bytes.extend(chunk);
        }
        self.end_seq = end_seq;
    }

    /// The retained tail with its absolute bounds, or `None` when nothing was
    /// retained. The bytes are COPIED: a bundle assembled after a later push
    /// would otherwise carry whatever displaced them.
    pub fn snapshot(&self) -> Option<TerminalWorkerByteCaptureTail> {
        if self.bytes.is_empty() {
            return None;
        }
        let (front, back) = self.bytes.as_slices();
        let mut owned = Vec::with_capacity(self.bytes.len());
        owned.extend_from_slice(front);
        owned.extend_from_slice(back);
        let byte_length = owned.len() as u64;
        let end_offset = self.end_seq.max(byte_length);
        Some(TerminalWorkerByteCaptureTail {
            end_offset: end_offset.to_string(),
            start_offset: (end_offset - byte_length).to_string(),
            byte_length,
            base64: base64::engine::general_purpose::STANDARD.encode(&owned),
        })
    }
}

/// Every session's window, keyed by session id.
#[derive(Debug, Default)]
pub struct ByteWindows {
    rings: HashMap<String, ByteWindow>,
}

impl ByteWindows {
    /// v2 `push(sid, chunk, endSeq)`.
    pub fn push(&mut self, session_id: &str, chunk: &[u8], end_seq: u64) {
        if let Some(window) = self.rings.get_mut(session_id) {
            window.push(chunk, end_seq);
            return;
        }
        let mut window = ByteWindow::new();
        window.push(chunk, end_seq);
        self.rings.insert(session_id.to_owned(), window);
    }

    /// v2 `drop(sid)`: a closed session's tail goes with it.
    pub fn drop_session(&mut self, session_id: &str) {
        self.rings.remove(session_id);
    }

    /// v2 `snapshotByteCapture(sid)`.
    pub fn snapshot(&self, session_id: &str) -> Option<TerminalWorkerByteCaptureTail> {
        self.rings.get(session_id).and_then(ByteWindow::snapshot)
    }

    /// Sessions currently holding a window.
    pub fn len(&self) -> usize {
        self.rings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }
}
