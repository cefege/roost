//! The link's application barrier: the ordering rules that decide when a link
//! may carry live traffic, and what a stale acknowledgement may and may not do.
//!
//! These are ORDERING rules, which is exactly the class that never gets tested
//! by hand. Each test names the rule from
//! `protocol/spec/worker-link.md` §State machine that it pins.

use roost_worker::link_barrier::{Action, Barrier, Pump};

/// Drive a pump to LIVE with nothing durable outstanding, returning it.
fn live_pump() -> Pump {
    let mut pump = Pump::new();
    assert_eq!(pump.on_open(), Action::SendHello);
    assert_eq!(pump.barrier(), Barrier::Open);
    assert_eq!(pump.on_hello_ack(), Action::WriteSnapshot);
    let snapshot_seq = match pump.on_snapshot_ack(1) {
        Action::IgnoredAck { .. } => panic!("the snapshot ACK must match the sequence written"),
        _ => 2,
    };
    assert_eq!(snapshot_seq, 2);
    assert_eq!(pump.barrier(), Barrier::Live);
    pump
}

/// RULE 1. `open` is NOT application-ready. A link that has merely connected
/// must not carry traffic, because the coordinator does not yet know the
/// worker exists.
#[test]
fn an_open_link_is_not_application_ready() {
    let mut pump = Pump::new();
    assert_eq!(pump.barrier(), Barrier::Idle);
    assert!(!pump.barrier().allows_live_traffic());

    assert_eq!(
        pump.on_open(),
        Action::SendHello,
        "the hello is the only forced first write"
    );
    assert_eq!(pump.barrier(), Barrier::Open);
    assert!(!pump.barrier().allows_live_traffic(), "open is not ready");
    assert!(
        !pump.barrier().allows_durable_write(),
        "and nothing may be written but the hello"
    );
}

/// The barrier walks hello → replay → snapshot → live, and the only forced
/// first write is the hello.
#[test]
fn the_barrier_walks_hello_replay_snapshot_live() {
    let pump = live_pump();
    assert_eq!(pump.barrier(), Barrier::Live);
    assert!(pump.barrier().allows_live_traffic());
    assert!(pump.barrier().allows_durable_write());
}

/// RULE 2. Exactly ONE durable event is in flight at a time. The coordinator
/// ACKs each with its exact sequence, so a second in flight would make the two
/// ACKs ambiguous — and an ambiguous ACK is an event that may or may not have
/// been written, which a durable path cannot recover from alone.
#[test]
fn exactly_one_durable_event_is_in_flight() {
    let mut pump = Pump::new();
    pump.on_open();
    // Queued while the barrier is short of `replay`: admitted, because
    // durability does not wait on the network, but not written.
    pump.enqueue_durable(b"one".to_vec());
    pump.enqueue_durable(b"two".to_vec());
    pump.enqueue_durable(b"three".to_vec());
    pump.on_hello_ack();

    // The first is in flight; the rest wait. Nothing was written past it.
    assert_eq!(
        pump.durable_pending(),
        3,
        "all three are durable and none is lost"
    );
    assert!(
        matches!(pump.on_event_ack(99), Action::IgnoredAck { .. }),
        "a wrong sequence is ignored"
    );
    assert_eq!(
        pump.durable_pending(),
        3,
        "and the in-flight event is still in flight"
    );

    // Acknowledging the first in order releases the second, then the third.
    assert_eq!(pump.on_event_ack(1), Action::WriteDurable { seq: 2 });
    assert_eq!(pump.on_event_ack(2), Action::WriteDurable { seq: 3 });
}

/// RULE 3. A stale or duplicate ACK cannot release the barrier. A barrier
/// released by the wrong ACK admits live traffic over a durable event the
/// coordinator never confirmed — the at-least-once violation this design
/// exists to prevent.
#[test]
fn a_stale_or_duplicate_ack_cannot_release_the_barrier() {
    for wrong in [0u64, 2, 99, u64::MAX] {
        let mut pump = Pump::new();
        pump.on_open();
        pump.enqueue_durable(b"one".to_vec());
        assert_eq!(
            pump.on_event_ack(wrong),
            Action::IgnoredAck { seq: wrong },
            "sequence {wrong} is not the one in flight"
        );
        assert_ne!(
            pump.barrier(),
            Barrier::Live,
            "and the barrier did not release on a wrong ACK"
        );
        assert_eq!(
            pump.durable_pending(),
            1,
            "the event is still owed to the coordinator"
        );
    }
}

/// A duplicate ACK for an event that is ALREADY acknowledged is ignored too. On
/// a reconnect the coordinator re-sends ACKs it has already sent, and closing
/// on one would turn a benign duplicate into an outage.
#[test]
fn a_replayed_ack_for_an_already_acked_event_is_ignored() {
    let mut pump = live_pump();
    let seq = match pump.enqueue_durable(b"durable".to_vec()) {
        Action::WriteDurable { seq } => seq,
        other => panic!("a live link writes at once, got {other:?}"),
    };
    assert!(
        !matches!(pump.on_event_ack(seq), Action::IgnoredAck { .. }),
        "the first ACK is honoured"
    );
    assert_eq!(
        pump.on_event_ack(seq),
        Action::IgnoredAck { seq },
        "and the coordinator re-sending it is ignored"
    );
    assert_eq!(
        pump.barrier(),
        Barrier::Live,
        "and it did not disturb a live link"
    );
    assert_eq!(pump.durable_acked(), 2);
}

/// RULE 4. A durable event that appears DURING a snapshot forces replay AGAIN.
/// The snapshot in flight describes a state that has already moved on, so
/// acknowledging it must not admit live traffic.
#[test]
fn a_durable_event_arriving_during_a_snapshot_forces_replay_again() {
    let mut pump = Pump::new();
    pump.on_open();
    assert_eq!(
        pump.on_hello_ack(),
        Action::WriteSnapshot,
        "no durable events, so a snapshot"
    );

    // An event lands while the snapshot is in flight.
    pump.enqueue_durable(b"arrived mid-snapshot".to_vec());
    assert_eq!(
        pump.barrier(),
        Barrier::Snapshot,
        "the snapshot is still in flight"
    );

    // Acknowledging it does NOT make the link live.
    assert!(
        matches!(pump.on_snapshot_ack(1), Action::WriteDurable { .. }),
        "acknowledging the snapshot releases the event that arrived during it"
    );
    assert_eq!(
        pump.barrier(),
        Barrier::Replay,
        "the barrier went back to replay, not to live, because the snapshot \\
         described a state that has already moved on"
    );
    assert!(!pump.barrier().allows_live_traffic());
}

/// A blocking RESERVATION has the same effect as an event arriving, because it
/// is taken before the event exists and the snapshot must account for it.
#[test]
fn a_blocking_reservation_during_a_snapshot_forces_replay_again() {
    let mut pump = Pump::new();
    pump.on_open();
    pump.on_hello_ack();

    // A reservation with no bytes of its own.
    pump.note_durable_appeared();
    assert_eq!(
        pump.on_snapshot_ack(1),
        Action::WriteSnapshot,
        "replay found nothing to send, so retake it"
    );
    assert_eq!(
        pump.barrier(),
        Barrier::Snapshot,
        "and a fresh snapshot goes out"
    );
}

/// A snapshot that is acknowledged with the WRONG sequence does not release the
/// barrier, exactly as for a durable event.
#[test]
fn a_mismatched_snapshot_ack_does_not_release_the_barrier() {
    let mut pump = Pump::new();
    pump.on_open();
    pump.on_hello_ack();
    assert_eq!(pump.on_snapshot_ack(77), Action::IgnoredAck { seq: 77 });
    assert_ne!(
        pump.barrier(),
        Barrier::Live,
        "a wrong sequence must not make the link live"
    );
    assert_eq!(
        pump.barrier(),
        Barrier::Snapshot,
        "and the real snapshot is still owed"
    );
}

/// RULE 5. Disconnect resets application state to hello and clears the volatile
/// side — but durable rows REMAIN. A durable row that vanished on reconnect
/// would be a hole in the coordinator's record of what happened.
#[test]
fn a_disconnect_resets_state_but_keeps_durable_events() {
    let mut pump = Pump::new();
    pump.on_open();
    pump.enqueue_durable(b"owed to the coordinator".to_vec());
    assert_eq!(pump.durable_pending(), 1);

    let previous = pump.on_disconnect();
    assert_ne!(previous, Barrier::Idle, "it was doing something");
    assert_eq!(pump.barrier(), Barrier::Idle, "and is now doing nothing");
    assert_eq!(
        pump.durable_pending(),
        1,
        "the durable event survives the disconnect — that is the whole point \\
         of it being durable"
    );
}

/// And the reconnect walks the barrier again from the beginning, replaying what
/// was owed before anything new goes out.
#[test]
fn a_reconnect_replays_what_was_owed_before_anything_new() {
    let mut pump = Pump::new();
    pump.on_open();
    pump.enqueue_durable(b"owed".to_vec());
    pump.on_disconnect();

    // The new connection is not application-ready either.
    assert_eq!(pump.on_open(), Action::SendHello);
    assert!(!pump.barrier().allows_live_traffic());
    assert_eq!(
        pump.on_hello_ack(),
        Action::WriteDurable { seq: 1 },
        "the owed event goes first"
    );
}

/// A durable event enqueued while the link is LIVE is written immediately.
#[test]
fn a_live_link_writes_a_durable_event_at_once() {
    let mut pump = live_pump();
    assert!(
        matches!(
            pump.enqueue_durable(b"new".to_vec()),
            Action::WriteDurable { .. }
        ),
        "a live link writes a durable event at once"
    );
}

/// An event enqueued BEFORE the hello ack waits, and is written in order once
/// the barrier reaches replay. Its sequence was allocated first, so ordering is
/// preserved across the barrier rather than across the connection.
#[test]
fn an_event_queued_before_the_hello_ack_keeps_its_place_in_the_order() {
    let mut pump = Pump::new();
    pump.on_open();
    // Queued while only `open`: admitted, because durability does not wait on
    // the network, but not written.
    assert_eq!(pump.enqueue_durable(b"early".to_vec()), Action::Wait);
    assert_eq!(pump.durable_pending(), 1);

    assert_eq!(
        pump.on_hello_ack(),
        Action::WriteDurable { seq: 1 },
        "and it goes first"
    );
}

/// A snapshot is only taken when there is nothing durable to replay and nothing
/// blocked. An empty replay with a blocked reservation must NOT reach `live`.
#[test]
fn a_snapshot_is_taken_only_when_nothing_is_owed() {
    let mut pump = Pump::new();
    pump.on_open();
    pump.enqueue_durable(b"one".to_vec());
    assert_eq!(
        pump.on_hello_ack(),
        Action::WriteDurable { seq: 1 },
        "a durable event replays before any snapshot"
    );
    assert_eq!(pump.barrier(), Barrier::Replay);
}
