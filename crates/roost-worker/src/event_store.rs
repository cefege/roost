//! The durable session-event store's admission control: who may claim capacity,
//! for how long, and when a snapshot is allowed to proceed.
//!
//! Owned by the worker. The SQLite schema, its I/O, and sequence allocation
//! live elsewhere; this file is the part that decides whether an event may be
//! written at all, and that decision is where the durable guarantee actually
//! lives.
//!
//! THE PROBLEM THIS SOLVES. A session that is open has not yet written its
//! `closed` event, and it must be able to. If the store fills up in the
//! meantime, that close is unwritable — and a session that cannot record that
//! it ended is a session a client believes is still running. So opening a
//! session RESERVES the capacity its close will need, before anyone knows
//! whether the close will happen.
//!
//! That reservation is a claim with a lifetime, not a queue entry, and the
//! subtlety is that it must stop blocking snapshots without giving up the
//! claim. `hold` does exactly that: the session is committed, so its close
//! capacity is no longer speculative, and the same token remains the sole
//! owner of that close.

use std::collections::HashMap;
use std::time::Duration;

/// How many rows the store will hold.
pub const MAX_ROWS: usize = 8_192;

/// How many payload bytes the store will hold across every row.
pub const MAX_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

/// How large the whole database may grow, page overhead included.
pub const MAX_DATABASE_BYTES: u64 = 16 * 1024 * 1024;

/// Sequences are allocated in blocks this size.
///
/// A block is claimed and written as a unit, so a crash costs the unused tail
/// of one block rather than renumbering every event written after it. The
/// consequence is a GAP in the sequence, never a repeat — a repeat would let a
/// replayed event be mistaken for a new one.
pub const SEQUENCE_BLOCK_SIZE: u64 = 1_024;

/// The kinds of event a session may durably record.
///
/// A closed set, because each kind has a different serialized size bound and a
/// caller that reserves the wrong kind's capacity would discover the mismatch
/// at write time, when the store is full and cannot grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DurableEventKind {
    Opened,
    State,
    Exited,
    Closed,
}

impl DurableEventKind {
    /// The serialized bound for this kind.
    ///
    /// Per kind rather than one global number, because the kinds are not
    /// interchangeable: reserving a generous bound for a small event wastes
    /// capacity that a large one needs.
    pub fn payload_limit(self) -> usize {
        match self {
            DurableEventKind::Opened => 256 * 1024,
            DurableEventKind::State => 1024 * 1024,
            DurableEventKind::Exited => 64 * 1024,
            DurableEventKind::Closed => 64 * 1024,
        }
    }

    /// The bound charged when a caller does not state one.
    pub fn default_reserved_bytes(self) -> usize {
        match self {
            DurableEventKind::Opened => 4 * 1024,
            DurableEventKind::State => 16 * 1024,
            DurableEventKind::Exited => 2 * 1024,
            DurableEventKind::Closed => 2 * 1024,
        }
    }
}

/// What the database itself currently holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub rows: usize,
    pub payload_bytes: usize,
    pub database_bytes: u64,
}

/// A claim on capacity, owned by exactly one caller until it is consumed or
/// released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    id: u64,
    kind: DurableEventKind,
    payload_bytes: usize,
    /// Whether this claim still blocks a snapshot. Cleared by [`Store::hold`].
    snapshot_blocking: bool,
}

impl Reservation {
    /// This claim's identity, for matching a refusal back to its request.
    pub fn id(self) -> u64 {
        self.id
    }

    /// What this claim was taken for.
    pub fn kind(self) -> DurableEventKind {
        self.kind
    }
}

/// Why a reservation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReserveError {
    #[error(
        "the store is full: {rows} rows and {bytes} payload bytes against caps of {max_rows} and {max_bytes}"
    )]
    Full {
        rows: usize,
        bytes: usize,
        max_rows: usize,
        max_bytes: usize,
    },
    #[error("a {kind:?} event of {payload} bytes exceeds its {limit} byte bound")]
    PayloadTooLarge {
        kind: DurableEventKind,
        payload: usize,
        limit: usize,
    },
    #[error("a reservation of {payload} bytes is not a positive number of bytes")]
    PayloadNotPositive { payload: usize },
    #[error("reservation {id} is not live: it was already consumed or released")]
    ReservationNotLive { id: u64 },
    #[error("reservation {id} is already held and is no longer blocking snapshots")]
    AlreadyHeld { id: u64 },
}

/// Why a stored event did not match its reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AppendError {
    #[error("the reservation for {expected:?} was used for a {actual:?} event")]
    KindMismatch {
        expected: DurableEventKind,
        actual: DurableEventKind,
    },
    #[error("reservation {id} is not live: it was already consumed or released")]
    ReservationNotLive { id: u64 },
    #[error("the serialized event of {actual} bytes exceeds its {limit} byte bound")]
    PayloadTooLarge { actual: usize, limit: usize },
}

/// The store's admission control.
#[derive(Debug)]
pub struct Store {
    next_reservation_id: u64,
    reservations: HashMap<u64, Reservation>,
    /// Rows and bytes claimed by live reservations, counted SEPARATELY from
    /// what the database holds. Merging them would let a caller reserve
    /// against capacity the database has already spent.
    reserved_rows: usize,
    reserved_bytes: usize,
    /// How many live reservations still block a snapshot.
    blocking_reserved_rows: usize,
    stats: StoreStats,
}

impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}

impl Store {
    pub fn new() -> Self {
        Self {
            next_reservation_id: 1,
            reservations: HashMap::new(),
            reserved_rows: 0,
            reserved_bytes: 0,
            blocking_reserved_rows: 0,
            stats: StoreStats::default(),
        }
    }

    /// What the database holds, as opposed to what is claimed against it.
    pub fn stats(&self) -> StoreStats {
        self.stats
    }

    /// How many reservations are live.
    pub fn live_reservations(&self) -> usize {
        self.reservations.len()
    }

    /// Whether a snapshot may proceed.
    ///
    /// A snapshot is one unchunked sequence of every session's events. If it
    /// consumed the capacity a live session has claimed for its own close, that
    /// close becomes unwritable — so snapshots wait while any claim is still
    /// speculative.
    pub fn snapshot_allowed(&self) -> bool {
        self.blocking_reserved_rows == 0
    }

    /// Claim capacity for one durable event.
    ///
    /// The claim is taken BEFORE the event exists, which is the whole point: the
    /// caller may need the capacity and not know it yet.
    pub fn reserve(
        &mut self,
        kind: DurableEventKind,
        payload_bytes: usize,
    ) -> Result<Reservation, ReserveError> {
        if payload_bytes == 0 {
            return Err(ReserveError::PayloadNotPositive {
                payload: payload_bytes,
            });
        }
        let limit = kind.payload_limit();
        if payload_bytes > limit {
            return Err(ReserveError::PayloadTooLarge {
                kind,
                payload: payload_bytes,
                limit,
            });
        }
        // A reserve that would exactly fill the store leaves nothing for the
        // next event, and the next event is usually a close.
        if self.stats.rows + self.reserved_rows >= MAX_ROWS
            || self.stats.payload_bytes + self.reserved_bytes + payload_bytes > MAX_PAYLOAD_BYTES
        {
            return Err(ReserveError::Full {
                rows: self.stats.rows + self.reserved_rows,
                bytes: self.stats.payload_bytes + self.reserved_bytes,
                max_rows: MAX_ROWS,
                max_bytes: MAX_PAYLOAD_BYTES,
            });
        }

        let reservation = Reservation {
            id: self.next_reservation_id,
            kind,
            payload_bytes,
            snapshot_blocking: true,
        };
        self.next_reservation_id += 1;
        self.reserved_rows += 1;
        self.reserved_bytes += payload_bytes;
        self.blocking_reserved_rows += 1;
        self.reservations.insert(reservation.id, reservation);
        Ok(reservation)
    }

    /// Claim capacity at the kind's default size.
    pub fn reserve_default(&mut self, kind: DurableEventKind) -> Result<Reservation, ReserveError> {
        self.reserve(kind, kind.default_reserved_bytes())
    }

    /// Stop a claim from blocking snapshots, without giving it up.
    ///
    /// The session is committed: it is open and the coordinator knows, so its
    /// close is not speculative. The claim survives because the close still has
    /// to fit — releasing it here would make a busy store able to strand a live
    /// session with nowhere to record its end.
    pub fn hold(&mut self, reservation: Reservation) -> Result<Reservation, ReserveError> {
        let mut active = self
            .active(reservation.id)
            .ok_or(ReserveError::ReservationNotLive { id: reservation.id })?;
        if !active.snapshot_blocking {
            return Err(ReserveError::AlreadyHeld { id: active.id });
        }
        active.snapshot_blocking = false;
        self.blocking_reserved_rows -= 1;
        self.reservations.insert(active.id, active);
        Ok(active)
    }

    /// Give a claim back without writing anything.
    pub fn release(&mut self, reservation: Reservation) -> Result<(), ReserveError> {
        let Some(active) = self.active(reservation.id) else {
            return Err(ReserveError::ReservationNotLive { id: reservation.id });
        };
        self.consume(active);
        Ok(())
    }

    /// Write the event the claim was taken for, and spend the claim.
    ///
    /// The bytes are checked against the kind's bound HERE as well as at
    /// reservation, because the caller may have reserved a small default and
    /// then serialized something larger.
    pub fn append(
        &mut self,
        reservation: Reservation,
        actual_kind: DurableEventKind,
        actual_payload_bytes: usize,
    ) -> Result<StoredEvent, AppendError> {
        let Some(active) = self.active(reservation.id) else {
            return Err(AppendError::ReservationNotLive { id: reservation.id });
        };
        if active.kind != actual_kind {
            return Err(AppendError::KindMismatch {
                expected: active.kind,
                actual: actual_kind,
            });
        }
        let limit = active.kind.payload_limit();
        if actual_payload_bytes > limit {
            return Err(AppendError::PayloadTooLarge {
                actual: actual_payload_bytes,
                limit,
            });
        }

        self.consume(active);
        // The reservation charged a default or an estimate; the difference is
        // settled here, so the running totals are what the store actually
        // holds rather than what callers hoped they would.
        self.stats.rows += 1;
        self.stats.payload_bytes += actual_payload_bytes;
        self.reserved_bytes = self.reserved_bytes.saturating_sub(active.payload_bytes);
        Ok(StoredEvent {
            reservation_id: active.id,
            kind: actual_kind,
            payload_bytes: actual_payload_bytes,
        })
    }

    /// Report what the database holds, for the store's own persistence.
    pub fn note_persisted(&mut self, rows: usize, payload_bytes: usize, database_bytes: u64) {
        self.stats = StoreStats {
            rows,
            payload_bytes,
            database_bytes,
        };
    }

    /// Whether the database may grow to the next block without exceeding its
    /// page budget.
    pub fn database_within_budget(&self) -> bool {
        self.stats.database_bytes <= MAX_DATABASE_BYTES
    }

    fn active(&self, id: u64) -> Option<Reservation> {
        self.reservations.get(&id).copied()
    }

    /// Spend a claim, returning its capacity whether or not it was blocking.
    fn consume(&mut self, active: Reservation) {
        if self.reservations.remove(&active.id).is_some() {
            self.reserved_rows -= 1;
            self.reserved_bytes = self.reserved_bytes.saturating_sub(active.payload_bytes);
            if active.snapshot_blocking {
                self.blocking_reserved_rows -= 1;
            }
        }
    }
}

/// An event that has been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredEvent {
    pub reservation_id: u64,
    pub kind: DurableEventKind,
    pub payload_bytes: usize,
}

/// How long a caller may hold a reservation before it is assumed abandoned.
///
/// A crash while holding one would otherwise leak capacity for the life of the
/// process, and a store that leaks capacity is a store that eventually refuses
/// every write.
pub const RESERVATION_LEASE: Duration = Duration::from_secs(60);
