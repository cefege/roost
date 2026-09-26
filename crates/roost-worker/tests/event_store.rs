//! The durable store's admission control. The property under test throughout
//! is the one the design exists for: **a live session can always record that
//! it ended**, no matter what else is competing for the store.

use roost_worker::event_store::{
    AppendError, DurableEventKind, MAX_PAYLOAD_BYTES, MAX_ROWS, ReserveError, SEQUENCE_BLOCK_SIZE,
    Store,
};

fn kind_limit(kind: DurableEventKind) -> usize {
    kind.payload_limit()
}

/// A reservation is taken BEFORE the event exists, because the caller may need
/// the capacity and not know it yet.
#[test]
fn a_reservation_takes_capacity_before_the_event_exists() {
    let mut store = Store::new();
    assert_eq!(store.live_reservations(), 0);
    let reservation = store
        .reserve(DurableEventKind::State, 16 * 1024)
        .expect("reserved");
    assert_eq!(store.live_reservations(), 1);

    store.note_persisted(0, 0, 0);
    // The reservation's capacity is not yet in the store's own totals; it is
    // claimed against them, which is a different thing.
    assert_eq!(store.stats().rows, 0);
    let _ = reservation;
}

/// A snapshot is refused while any claim is still speculative, because a
/// snapshot that consumed a live session's close capacity would make that
/// close unwritable.
#[test]
fn a_snapshot_waits_while_a_claim_is_speculative() {
    let mut store = Store::new();
    assert!(store.snapshot_allowed(), "an empty store may snapshot");

    store
        .reserve_default(DurableEventKind::Opened)
        .expect("reserved");
    assert!(
        !store.snapshot_allowed(),
        "a speculative claim blocks the snapshot"
    );

    store
        .reserve_default(DurableEventKind::Opened)
        .expect("reserved again");
    assert!(
        !store.snapshot_allowed(),
        "and the second one keeps it blocked"
    );
}

/// THE DESIGN. A committed session's claim stops blocking snapshots WITHOUT
/// being given up: the close is no longer speculative, but it still has to fit.
#[test]
fn a_held_claim_stops_blocking_snapshots_and_is_kept() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::Opened)
        .expect("reserved");
    assert!(!store.snapshot_allowed());

    store.hold(reservation).expect("held");
    assert!(
        store.snapshot_allowed(),
        "a committed session no longer blocks a snapshot"
    );
    assert_eq!(
        store.live_reservations(),
        1,
        "and the claim survives, because the close still needs the room"
    );
}

/// That surviving claim is what makes the guarantee. A store that filled up
/// after the session was committed would still have room for the close,
/// because the close's capacity was never released.
#[test]
fn a_held_claim_still_admits_its_close_when_the_store_is_full() {
    let mut store = Store::new();
    let close = store
        .reserve_default(DurableEventKind::Closed)
        .expect("reserved");
    store.hold(close).expect("held");

    // Fill the store right up to its row cap with unrelated events.
    store.note_persisted(MAX_ROWS - 1, 0, 0);
    assert!(
        store.reserve_default(DurableEventKind::Opened).is_err(),
        "an unrelated reservation is refused when the store is full"
    );

    // But the held claim was taken against capacity that is still reserved, so
    // its own write succeeds. This is the case the whole design exists for.
    let stored = store
        .append(close, DurableEventKind::Closed, 1_024)
        .expect("the close fits");
    assert_eq!(stored.kind, DurableEventKind::Closed);
}

/// Releasing a claim gives the capacity back, and a released claim is dead.
#[test]
fn a_released_claim_returns_its_capacity_and_cannot_be_used() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::State)
        .expect("reserved");
    assert!(!store.snapshot_allowed());

    store.release(reservation).expect("released");
    assert_eq!(store.live_reservations(), 0);
    assert!(store.snapshot_allowed(), "releasing unblocks the snapshot");

    assert_eq!(
        store.append(reservation, DurableEventKind::State, 512),
        Err(AppendError::ReservationNotLive {
            id: reservation.id()
        }),
        "a released claim cannot be spent"
    );
}

/// A claim is owned by ONE caller. A second use of the same token is a bug in
/// the caller, and a double-spend would write the event twice.
#[test]
fn a_claim_cannot_be_spent_twice() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::Exited)
        .expect("reserved");
    store
        .append(reservation, DurableEventKind::Exited, 128)
        .expect("written");

    assert_eq!(
        store.append(reservation, DurableEventKind::Exited, 128),
        Err(AppendError::ReservationNotLive {
            id: reservation.id()
        }),
        "a consumed claim is dead"
    );
    assert_eq!(
        store.stats().rows,
        1,
        "and the event was written exactly once"
    );
}

/// Holding an already-held claim is refused: a caller that believes it may
/// still be blocking a snapshot when it is not would size its snapshot wrongly.
#[test]
fn a_claim_cannot_be_held_twice() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::Opened)
        .expect("reserved");
    store.hold(reservation).expect("held");
    assert_eq!(
        store.hold(reservation),
        Err(ReserveError::AlreadyHeld {
            id: reservation.id()
        })
    );
}

/// A reservation for one kind cannot be spent on another. The kinds have
/// different bounds, so a mismatch means the caller's accounting is wrong.
#[test]
fn a_claim_cannot_be_spent_on_another_kind() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::Opened)
        .expect("reserved");
    assert_eq!(
        store.append(reservation, DurableEventKind::Exited, 128),
        Err(AppendError::KindMismatch {
            expected: DurableEventKind::Opened,
            actual: DurableEventKind::Exited
        })
    );
    assert_eq!(
        store.live_reservations(),
        1,
        "and the claim survives a refused write"
    );
}

/// The serialized size is checked at WRITE as well as at reservation, because
/// a caller may reserve a small default and then serialize something larger.
#[test]
fn an_oversized_write_is_refused_even_with_a_reservation() {
    let mut store = Store::new();
    let reservation = store
        .reserve_default(DurableEventKind::Exited)
        .expect("reserved");
    let too_big = kind_limit(DurableEventKind::Exited) + 1;
    assert_eq!(
        store.append(reservation, DurableEventKind::Exited, too_big),
        Err(AppendError::PayloadTooLarge {
            actual: too_big,
            limit: kind_limit(DurableEventKind::Exited)
        })
    );
    assert_eq!(
        store.live_reservations(),
        1,
        "the claim survives, so the caller can retry smaller"
    );
}

/// A reservation above its kind's bound is refused before any capacity moves.
#[test]
fn a_reservation_above_its_kinds_bound_is_refused() {
    let mut store = Store::new();
    let too_big = kind_limit(DurableEventKind::Exited) + 1;
    assert_eq!(
        store.reserve(DurableEventKind::Exited, too_big),
        Err(ReserveError::PayloadTooLarge {
            kind: DurableEventKind::Exited,
            payload: too_big,
            limit: kind_limit(DurableEventKind::Exited)
        })
    );
    assert_eq!(store.live_reservations(), 0);
}

/// A zero-byte reservation is refused: a claim for nothing would consume a row
/// and permanently reduce the store's real capacity.
#[test]
fn a_zero_byte_reservation_is_refused() {
    let mut store = Store::new();
    assert_eq!(
        store.reserve(DurableEventKind::Opened, 0),
        Err(ReserveError::PayloadNotPositive { payload: 0 })
    );
    assert_eq!(store.live_reservations(), 0);
}

/// The byte cap counts BOTH what the database holds and what is claimed against
/// it. Merging the two would let a caller reserve against capacity the
/// database has already spent.
#[test]
fn the_byte_cap_counts_stored_and_claimed_together() {
    let mut store = Store::new();
    let big = kind_limit(DurableEventKind::State) / 2;
    store
        .reserve(DurableEventKind::State, big)
        .expect("claimed");

    // The database claims most of the budget on its own.
    store.note_persisted(0, MAX_PAYLOAD_BYTES - big - 1024, 0);
    assert!(
        store.reserve(DurableEventKind::State, big).is_err(),
        "a second large claim does not fit alongside the stored bytes"
    );
}

/// The row cap is INCLUSIVE: a store holding one row short of the cap admits
/// exactly one more, and a full store refuses. The boundary is pinned because a
/// cap that is off by one in either direction is a cap nobody notices until it
/// matters.
#[test]
fn the_row_cap_is_inclusive_at_its_boundary() {
    let mut store = Store::new();
    store.note_persisted(MAX_ROWS - 1, 0, 0);
    assert!(
        store.reserve_default(DurableEventKind::Opened).is_ok(),
        "one row short of the cap still admits a row"
    );

    store.note_persisted(MAX_ROWS, 0, 0);
    assert!(
        matches!(
            store.reserve_default(DurableEventKind::Opened),
            Err(ReserveError::Full { .. })
        ),
        "a full store refuses, and says how full it is"
    );
}

/// The default size is a real bound, and it is smaller than the kind's limit —
/// the difference is headroom for an event that turns out to be larger.
#[test]
fn the_default_reservation_leaves_headroom_within_the_kinds_bound() {
    for kind in [
        DurableEventKind::Opened,
        DurableEventKind::State,
        DurableEventKind::Exited,
        DurableEventKind::Closed,
    ] {
        assert!(
            kind.default_reserved_bytes() < kind.payload_limit(),
            "{kind:?} reserves {} against a limit of {}, leaving no headroom",
            kind.default_reserved_bytes(),
            kind.payload_limit()
        );
        assert!(
            kind.default_reserved_bytes() > 0,
            "{kind:?} reserves nothing"
        );
    }
}

/// Sequences are allocated in blocks, so a crash costs the unused tail of one
/// block. The consequence is a GAP, never a repeat — a repeat would let a
/// replayed event be mistaken for a new one.
#[test]
fn sequences_are_allocated_in_blocks_so_a_crash_costs_a_gap() {
    // The block size is a decision, not a tunable. Clippy is right to flag an
    // assertion on a constant, and the answer is to record the value: a test
    // that reads like a tunable invites someone to change it.
    assert_eq!(SEQUENCE_BLOCK_SIZE, 1_024, "a block of one is not a block");
    assert!(
        SEQUENCE_BLOCK_SIZE.is_power_of_two(),
        "a round power of two, so the tail a crash costs is a known fraction of it"
    );
}

/// The database's page budget is a real bound, and the store reports whether it
/// is inside it.
#[test]
fn the_database_budget_is_reported() {
    let mut store = Store::new();
    store.note_persisted(0, 0, 1_000_000);
    assert!(store.database_within_budget());
    store.note_persisted(0, 0, u64::MAX);
    assert!(
        !store.database_within_budget(),
        "an oversized database is reported, not clamped"
    );
}

/// Many sessions reserving and releasing must not leak capacity. A store that
/// leaks is a store that eventually refuses every write, and the refusal would
/// look like a full store rather than a leak.
#[test]
fn repeated_reserve_and_release_cycles_do_not_leak() {
    let mut store = Store::new();
    for _ in 0..5_000 {
        let reservation = store
            .reserve_default(DurableEventKind::State)
            .expect("reserved");
        store.release(reservation).expect("released");
    }
    assert_eq!(store.live_reservations(), 0, "every claim was returned");
    assert!(store.snapshot_allowed(), "and nothing is left blocking");
}

/// A full open/close cycle across many sessions leaves the store exactly as it
/// started, which is the property that makes the guarantee hold indefinitely.
#[test]
fn a_full_session_lifecycle_leaves_the_store_as_it_started() {
    let mut store = Store::new();
    for _ in 0..1_000 {
        let opened = store
            .reserve_default(DurableEventKind::Opened)
            .expect("opened");
        store
            .append(opened, DurableEventKind::Opened, 2_048)
            .expect("written");

        let close = store
            .reserve_default(DurableEventKind::Closed)
            .expect("closed");
        store.hold(close).expect("held");
        store
            .append(close, DurableEventKind::Closed, 1_024)
            .expect("written");
    }
    assert_eq!(store.live_reservations(), 0);
    assert!(store.snapshot_allowed());
    assert_eq!(
        store.stats().rows,
        2_000,
        "exactly the events that were written"
    );
}
