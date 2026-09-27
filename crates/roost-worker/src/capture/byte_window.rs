//! A per-session window of the last `BYTE_CAPTURE_WINDOW_BYTES` of PTY output,
//! always on. `capture::recorder` writes into it; `capture::bundle` reads the
//! tail out of it.
//!
//! It is v2's `apps/worker/src/diag/byte-capture.ts`, and the one sentence that
//! justifies it is the same there: AN ANOMALY FIRES PRECISELY WHEN THE
//! DIAGNOSTIC GATE WAS OFF, and a recorder that only retained bytes while armed
//! would find an empty window at exactly the moment it is needed.
//!
//! The offsets are LOGICAL BYTE POSITIONS, not ring indices. A window that
//! evicted its prefix has to report the absolute range it still covers, or an
//! operator reading a bundle cannot line it up against the session's own
//! `head_seq` — and "start_offset: 0" on a window that has dropped two hundred
//! kilobytes is a lie an operator would act on.

use std::collections::VecDeque;

use super::BYTE_CAPTURE_WINDOW_BYTES;

/// How many bytes of raw PTY output are retained per session.
///
/// Matches v2's `RING_CAP_BYTES`. The two cannot drift: a bundle written by a
/// v3 worker is read by an operator comparing it against a v2 incident, and a
/// window half the size is a bundle missing the start of the event.
pub const WINDOW_BYTES: usize = BYTE_CAPTURE_WINDOW_BYTES;

/// The retained tail, and the logical position its last byte sits at.
#[derive(Debug, Default)]
pub struct ByteWindow {
    bytes: VecDeque<u8>,
    /// The logical offset of the byte BEFORE `bytes[0]`.
    dropped: u64,
}

impl ByteWindow {
    /// A window with the declared capacity.
    pub fn new() -> Self {
        Self {
            bytes: VecDeque::with_capacity(WINDOW_BYTES),
            dropped: 0,
        }
    }

    /// Append a retained chunk, displacing from the front past the cap.
    pub fn push(&mut self, chunk: &[u8]) {
        for byte in chunk {
            if self.bytes.len() == WINDOW_BYTES {
                self.bytes.pop_front();
                self.dropped = self.dropped.saturating_add(1);
            }
            self.bytes.push_back(*byte);
        }
    }

    /// Forget the window. A closed session's evidence is not a live session's
    /// evidence: leaving it behind is a capture that freezes a session nobody
    /// can reach.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.dropped = 0;
    }

    /// The retained tail, OWNED, with its absolute bounds.
    ///
    /// The copy is not optional. A bundle assembled after a later append would
    /// otherwise carry whatever overwrote the bytes it was going to freeze, and
    /// the bundle is the artefact an operator trusts.
    pub fn tail(&self) -> Option<ByteTail> {
        if self.bytes.is_empty() {
            return None;
        }
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        let byte_length = bytes.len() as u64;
        Some(ByteTail {
            bytes,
            start_offset: self.dropped,
            end_offset: self.dropped.saturating_add(byte_length),
        })
    }
}

/// One owned tail, with the absolute range it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteTail {
    pub bytes: Vec<u8>,
    pub start_offset: u64,
    pub end_offset: u64,
}

#[cfg(test)]
mod tests {
    use super::{ByteWindow, WINDOW_BYTES};

    /// The whole reason the window is always on: an anomaly fires when nothing
    /// was armed, so an armed-only recorder would freeze an empty window.
    #[test]
    fn a_window_that_was_never_armed_still_holds_the_tail() {
        let mut window = ByteWindow::new();
        window.push(b"before anything was armed");
        let tail = window.tail().expect("an unarmed window still retains");
        assert_eq!(tail.bytes, b"before anything was armed");
        assert_eq!(tail.start_offset, 0);
        // The END offset is the count of retained bytes, and this literal is 25
        // of them. It was written as 24, which is a test that fails for a
        // reason unrelated to the thing it is testing — the worst kind, because
        // it trains a reader to ignore a red in this file.
        assert_eq!(tail.end_offset, 25);
    }

    #[test]
    fn a_displaced_prefix_reports_the_absolute_range_it_still_covers() {
        let mut window = ByteWindow::new();
        // One byte past the cap, so the first byte is displaced and the rest is
        // still addressable. A test that shrank the cap instead would be
        // exercising a capacity this type is never built with.
        let mut chunk = vec![b'x'; WINDOW_BYTES + 1];
        chunk[0] = b'f';
        window.push(&chunk);
        let tail = window.tail().expect("the tail survives a displacement");
        assert_eq!(tail.bytes.len(), WINDOW_BYTES);
        assert_eq!(tail.start_offset, 1);
        assert_eq!(tail.end_offset, WINDOW_BYTES as u64 + 1);
        assert_eq!(tail.bytes[0], b'x');
    }

    #[test]
    fn a_cleared_window_reports_nothing_rather_than_a_stale_tail() {
        let mut window = ByteWindow::new();
        window.push(b"evidence");
        window.clear();
        assert_eq!(window.tail(), None);
    }
}
