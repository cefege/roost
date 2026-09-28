//! The per-worker `client_seq` cursor: one allocator for one sequence space.
//!
//! Owned by `CoordServices` and handed out by fingerprint. Deliberately NOT a
//! field of `FrameDispatch`, which is per socket, for three reasons that have
//! to be written down or someone will helpfully move it:
//!
//! - **A reconnect resumes the outbox, so the sequence outlives the socket.** A
//!   disconnect clears the worker's volatile lanes but "durable SQLite outbox
//!   rows remain" (`worker-link.md:32`) and the reconnect replays them. A cursor
//!   that died with its socket restarts at zero and the coordinator reads that
//!   as a *repeat* of sequence 1.
//! - **A newer hello SUPERSEDES rather than refuses, so two dispatchers for one
//!   fingerprint are a real state.** "Delayed callbacks are identity-fenced"
//!   only means anything while those callbacks are still running, which is the
//!   overlap. `admit_worker_upgrade` is a staleness check — it refuses a
//!   generation behind the current one and never asks whether the worker already
//!   has a live socket.
//! - **Two cursors is the two-allocators-of-one-space defect**, already found
//!   and fixed twice today in the `client_seq` and channel-id spaces.
//!
//! WHY `tokio::sync::Mutex` AND NOT `std::sync::Mutex`: the `std` guard is
//! `!Send` because a POSIX mutex is thread-affine, and the guard is held ACROSS
//! the `append_event` await inside a future that must be `Send`. The `std`
//! variant is not a slower choice, it is an **uncompilable** one.

use std::sync::Arc;

use tokio::sync::Mutex;

/// What one `client_seq` offer means to the worker's sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqVerdict {
    /// The exact successor: admit it, and the cursor advances.
    Admit,
    /// An exact replay of the last admitted value: dedupe at the unique index,
    /// and ACK it again.
    ///
    /// THE CURSOR DOES NOT MOVE, and that is the whole of this case. A retry
    /// that advanced the cursor would leave a gap behind it, and every later
    /// frame would then read as out of order against a sequence that skipped.
    Dedupe,
    /// Neither the successor nor the last value. Refused: a sequence that skips
    /// is a hole in the worker's durable log that the coordinator cannot
    /// distinguish from a lost frame.
    Gap {
        /// The value the cursor was waiting for.
        expected: u64,
        /// The value the worker sent.
        offered: u64,
    },
}

impl SeqVerdict {
    /// Whether this frame may be WRITTEN. A dedupe admits no write and still
    /// earns an ACK, which is why this is about the write and not the reply.
    #[must_use]
    pub const fn admits(self) -> bool {
        matches!(self, Self::Admit | Self::Dedupe)
    }
}

/// One worker's `client_seq` position, shared by every socket for that worker.
#[derive(Debug, Default)]
pub struct ClientSeqCursor {
    /// The last admitted value, or `None` before the worker's first frame.
    ///
    /// A `Mutex` and not an `AtomicU64` because the comparison and the advance
    /// are ONE decision. Two durable frames interleaving between a compare and a
    /// store is exactly the duplicate the unique index exists to catch, and
    /// catching it in SQLite is later than catching it here.
    last: Mutex<Option<u64>>,
}

impl ClientSeqCursor {
    /// A cursor that has admitted nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `offered` may be written, and advance when it is the successor.
    ///
    /// The cursor is left UNMOVED on a dedupe and not moved at all on a gap:
    /// both are refusals of the write, not progress through the sequence.
    pub async fn offer(&self, offered: u64) -> SeqVerdict {
        let mut last = self.last.lock().await;
        let expected = last.map_or(1, |held| held.saturating_add(1));
        let verdict = match *last {
            Some(held) if offered == held => SeqVerdict::Dedupe,
            Some(_) if offered == expected => SeqVerdict::Admit,
            Some(_) => SeqVerdict::Gap { expected, offered },
            None if offered == 1 => SeqVerdict::Admit,
            None => SeqVerdict::Gap { expected, offered },
        };
        if matches!(verdict, SeqVerdict::Admit) {
            *last = Some(offered);
        }
        verdict
    }

    /// The last admitted value, for a log line and for diagnostics.
    pub async fn last_admitted(&self) -> Option<u64> {
        *self.last.lock().await
    }
}

/// Every worker's cursor, keyed by fingerprint.
#[derive(Debug, Default)]
pub struct ClientSeqCursors {
    by_worker: std::sync::Mutex<std::collections::BTreeMap<String, Arc<ClientSeqCursor>>>,
}

impl ClientSeqCursors {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This worker's cursor, created on first use.
    ///
    /// The `std::sync::Mutex` is held for the MAP ONLY and is never held across
    /// an await, so it is not the guard `retained_grid.rs` argues against — the
    /// cursor itself is the `tokio::sync::Mutex`, and only the durable arm ever
    /// takes that.
    pub fn for_worker(&self, fingerprint: &str) -> Arc<ClientSeqCursor> {
        let Ok(mut registry) = self.by_worker.lock() else {
            // A poisoned map is a crash elsewhere, not a reason to invent a
            // second cursor: a fresh one would restart the sequence, which is
            // precisely the defect this type exists to prevent.
            return Arc::new(ClientSeqCursor::new());
        };
        registry
            .entry(fingerprint.to_owned())
            .or_insert_with(|| Arc::new(ClientSeqCursor::new()))
            .clone()
    }
}
