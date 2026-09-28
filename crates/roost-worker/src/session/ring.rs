//! The per-session retained PTY byte window, as an actual fixed-capacity ring.
//! `SessionRecord::scrollback` owns one; `session::scrollback` owns the append
//! and replay POLICY, and this file owns only the container it holds them in.
//!
//! It is a ring rather than a grow-and-slice buffer because the predecessor
//! allocated `retained + chunk` and copied twice per chunk, so appending one
//! byte of keystroke echo to a saturated session copied the whole window. The
//! failure that motivated it is in `docs/FAILURE-INDEX.md`: a memory-tight host
//! OOMs first on per-session copies, not on the sessions themselves.
//!
//! The ring holds NO byte counter. The monotonic count is
//! [`super::types::SessionRecord::head_seq`], because the emitter, the
//! `dead-birth` check and the wire's `end_seq` all read that one number, and a
//! second counter inside the ring would be a second answer to it.

use std::collections::VecDeque;

/// The retained window's capacity, per session.
///
/// 1 MiB, matched to the keeper's own per-channel ring so `getScrollback`
/// serves a fresh SPA the same depth of history the keeper started this worker
/// with. It was 8 MiB until 2026-06-22, when it was matched down to the keeper
/// on a permanently RAM-full box: ~10k lines is ample and the smaller
/// per-channel footprint is what a host with a cgroup limit actually needs.
pub const SCROLLBACK_CAP_BYTES: usize = 1024 * 1024;

/// A fixed-capacity window of the oldest PTY bytes a session has produced.
///
/// The retained bytes are held oldest first. A window that has not wrapped
/// holds a contiguous run; one that has wraps, and reading it back is an
/// allocation rather than a view because the borrowed version's validity window
/// ended at the next append — the same synchronous consumption the v2 rebuild
/// path documented as its correctness argument, and the reason this is a ring
/// rather than a slice into a `Vec`.
#[derive(Debug)]
pub struct ScrollbackRing {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl ScrollbackRing {
    /// An empty ring of `capacity` bytes.
    ///
    /// A zero capacity is raised to one rather than accepted: a ring that
    /// retains nothing cannot express `head_seq - len` as a floor, and the
    /// failure would surface as a silently wrong history address much later.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            // The window is allocated up to a page's worth of the cap and grows
            // into the rest, because a session that produces nothing — which is
            // most of them, most of the time — must cost nothing.
            bytes: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
        }
    }

    /// The window's capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// How many bytes are retained right now.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether nothing is retained. A session that has produced no output.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Whether the window has reached capacity and is evicting.
    ///
    /// The only honest answer to "could a rebuild have lost a byte?", so the
    /// diagnostic snapshot reports it rather than the length, which looks fine
    /// either side of the wrap.
    pub fn evicting(&self) -> bool {
        self.bytes.len() >= self.capacity
    }

    /// Append one chunk, evicting the oldest bytes at capacity.
    ///
    /// `O(chunk)`. Returns how many bytes are retained afterwards.
    pub fn append(&mut self, chunk: &[u8]) -> usize {
        if chunk.len() >= self.capacity {
            // A chunk at least as large as the window can only leave its own
            // tail behind. Going through the general path would evict byte by
            // byte for no different result.
            self.bytes.clear();
            self.bytes
                .extend(chunk[chunk.len() - self.capacity..].iter().copied());
            return self.bytes.len();
        }
        // How many of the bytes already retained this chunk displaces. The sum
        // is clamped rather than the difference: a window short of its cap
        // displaces nothing, and a naive `filled + len - cap` wraps in usize
        // and evicts the whole window instead.
        let overflow = (self.bytes.len() + chunk.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend(chunk.iter().copied());
        self.bytes.len()
    }

    /// The retained bytes, oldest first.
    pub fn to_vec(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    /// Feed the retained window to a consumer in order, without allocating.
    pub fn read(&self, mut consume: impl FnMut(u8)) {
        for byte in &self.bytes {
            consume(*byte);
        }
    }
}

impl Default for ScrollbackRing {
    /// The window a session spawns with, at [`SCROLLBACK_CAP_BYTES`].
    fn default() -> Self {
        Self::new(SCROLLBACK_CAP_BYTES)
    }
}
