//! Row and span accounting for the canonical terminal caches the coordinator
//! holds, plus the old versions an active snapshot cursor keeps alive.
//!
//! Ported from `apps/coord/src/terminal/screen/terminal-screen-residency.ts`.
//! The replica asks this owner before replacing a cache, so a slow cursor
//! cannot become an uncharged resident full -- the accounting is only worth
//! having if nothing can add to the pool without passing through it.
//!
//! A REFUSAL IS NOT A TRUNCATION. `can_replace` answering false means the
//! replica drops what it had, tells its sink the session is unavailable, and
//! raises one capacity signal; it never serves a partially-copied screen.
//!
//! WHY THE PIN IS A CHARGE AND NOT A SECOND CACHE. v2's `pinnedCache` exists
//! so the pool keeps counting a version a cursor is still walking; the frame
//! itself is kept alive by the cursor's own source closure, not by that slot.
//! Pinning the size and the lease count is the whole of what residency needs,
//! and copying a 256-row grid to hold a number would be the expensive way to
//! learn it.

use roost_protocol::cell::CellGridFrame;

/// The hard maxima, used when an operator declared no byte budget.
pub const TERMINAL_SCREEN_MAX_RESIDENT_ROWS: u64 = 65_536;
pub const TERMINAL_SCREEN_MAX_RESIDENT_SPANS: u64 = 2_097_152;

/// One immutable canonical full the coordinator is holding.
///
/// `generation` is the identity a cursor's lease is held against: a cache is
/// replaced wholesale, so two versions of one session are never equal, and
/// comparing generations is what replaces v2's reference comparison.
#[derive(Debug)]
pub struct ResidentCache {
    pub generation: u64,
    pub frame: CellGridFrame,
    /// The coordinator receipt time of the frame that produced this cache.
    pub coord_recv_ms: u64,
    /// How many snapshot sources still need this exact version. While it is
    /// non-zero the version is pinned and cannot be un-charged.
    pub source_lease_count: u32,
    pub rows: u64,
    pub spans: u64,
    /// False once a delta or a baseline failed against it. A replica never
    /// serves a cache it knows is wrong.
    pub valid: bool,
}

impl ResidentCache {
    /// A cache over an already-validated frame.
    #[must_use]
    pub fn new(
        generation: u64,
        frame: CellGridFrame,
        coord_recv_ms: u64,
        rows: u64,
        spans: u64,
    ) -> Self {
        Self {
            generation,
            frame,
            coord_recv_ms,
            source_lease_count: 0,
            rows,
            spans,
            valid: true,
        }
    }
}

/// A version a live cursor is still walking, held in the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedCharge {
    pub generation: u64,
    pub source_lease_count: u32,
    pub rows: u64,
    pub spans: u64,
}

/// One session's current replica and its at-most-one pinned predecessor.
#[derive(Debug, Default)]
pub struct SessionCharge {
    pub current: Option<ResidentCache>,
    pub pinned: Option<PinnedCharge>,
}

impl SessionCharge {
    /// The session's current cache, if it holds one.
    #[must_use]
    pub fn current(&self) -> Option<&ResidentCache> {
        self.current.as_ref()
    }
}

/// The row and span pool every screen replica is charged against.
#[derive(Debug)]
pub struct TerminalScreenResidency {
    max_rows: u64,
    max_spans: u64,
    resident_rows: u64,
    resident_spans: u64,
    next_generation: u64,
}

impl TerminalScreenResidency {
    /// A pool with the two ceilings a budget resolved to.
    #[must_use]
    pub fn new(max_rows: u64, max_spans: u64) -> Self {
        Self {
            max_rows,
            max_spans,
            resident_rows: 0,
            resident_spans: 0,
            next_generation: 1,
        }
    }

    /// The two ceilings this pool enforces.
    #[must_use]
    pub fn caps(&self) -> (u64, u64) {
        (self.max_rows, self.max_spans)
    }

    /// What the pool currently holds, for a diagnostics answer.
    #[must_use]
    pub fn usage(&self) -> (u64, u64) {
        (self.resident_rows, self.resident_spans)
    }

    /// Whether the pool would admit `rows`/`spans` for this session.
    ///
    /// A session that already has a pinned predecessor cannot also be
    /// mid-replacement: admitting the next version would need three slots, and
    /// the cursor that holds the predecessor is by definition slower than the
    /// stream that produced the current one.
    #[must_use]
    pub fn can_replace(&self, state: &SessionCharge, rows: u64, spans: u64) -> bool {
        let current_rows = state.current.as_ref().map_or(0, |cache| cache.rows);
        let current_spans = state.current.as_ref().map_or(0, |cache| cache.spans);
        if state
            .current
            .as_ref()
            .is_some_and(|cache| cache.source_lease_count > 0)
            && state.pinned.is_some()
        {
            return false;
        }
        self.resident_rows - current_rows + rows <= self.max_rows
            && self.resident_spans - current_spans + spans <= self.max_spans
    }

    /// Swap in the next cache, pinning the current one if a source still needs
    /// it. False means the pool refused and nothing changed.
    pub fn replace(
        &mut self,
        state: &mut SessionCharge,
        frame: CellGridFrame,
        coord_recv_ms: u64,
        rows: u64,
        spans: u64,
    ) -> bool {
        if !self.can_replace(state, rows, spans) {
            return false;
        }
        if let Some(current) = state.current.take() {
            if !self.retain_current_version(state, &current) {
                state.current = Some(current);
                return false;
            }
            self.resident_rows -= current.rows;
            self.resident_spans -= current.spans;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.resident_rows += rows;
        self.resident_spans += spans;
        state.current = Some(ResidentCache::new(
            generation,
            frame,
            coord_recv_ms,
            rows,
            spans,
        ));
        true
    }

    /// Un-charge a session's current cache, pinning it first if a cursor needs
    /// it. False means the pool could not take the pin, so nothing changed.
    pub fn drop_cache(&mut self, state: &mut SessionCharge) -> bool {
        let Some(current) = state.current.take() else {
            return true;
        };
        if !self.retain_current_version(state, &current) {
            state.current = Some(current);
            return false;
        }
        self.resident_rows -= current.rows;
        self.resident_spans -= current.spans;
        true
    }

    /// Take a lease on the current version. A version that is no longer the
    /// session's current one is refused: a new lease on it would keep bytes
    /// nobody is reading.
    pub fn acquire_source_lease(&mut self, state: &mut SessionCharge, generation: u64) -> bool {
        let Some(current) = state.current.as_mut() else {
            return false;
        };
        if current.generation != generation {
            return false;
        }
        current.source_lease_count += 1;
        true
    }

    /// Give a lease back. The last one out un-pins the version and returns it
    /// to the pool.
    pub fn release_source_lease(&mut self, state: &mut SessionCharge, generation: u64) {
        if let Some(current) = state.current.as_mut()
            && current.generation == generation
            && current.source_lease_count > 0
        {
            current.source_lease_count -= 1;
            return;
        }
        let Some(pinned) = state.pinned.as_ref() else {
            return;
        };
        if pinned.generation != generation || pinned.source_lease_count == 0 {
            return;
        }
        let rows = pinned.rows;
        let spans = pinned.spans;
        let remaining = pinned.source_lease_count - 1;
        if remaining != 0 {
            if let Some(pinned) = state.pinned.as_mut() {
                pinned.source_lease_count = remaining;
            }
            return;
        }
        self.resident_rows -= rows;
        self.resident_spans -= spans;
        state.pinned = None;
    }

    /// Pin a version a cursor still needs, charging it a second time. False
    /// means another version is already pinned or the pool is full.
    fn retain_current_version(&mut self, state: &mut SessionCharge, cache: &ResidentCache) -> bool {
        if cache.source_lease_count == 0 {
            return true;
        }
        if state.pinned.is_some() {
            return false;
        }
        if self.resident_rows + cache.rows > self.max_rows
            || self.resident_spans + cache.spans > self.max_spans
        {
            return false;
        }
        self.resident_rows += cache.rows;
        self.resident_spans += cache.spans;
        state.pinned = Some(PinnedCharge {
            generation: cache.generation,
            source_lease_count: cache.source_lease_count,
            rows: cache.rows,
            spans: cache.spans,
        });
        true
    }
}
