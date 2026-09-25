//! The keeper's retained history, which is what a fresh worker replays to
//! rebuild a channel it did not spawn. Split from the dispatcher tests because
//! the retention rules can be exercised with no PTY and no timing at all.
//!
//! The contract is `protocol/spec/keeper.md`.

use roost_keeper::channel_history::ChannelHistory;
use roost_keeper::history::HistoryRecord;
use roost_keeper::keeper::Keeper;
use roost_keeper::payloads::TerminalState;
use support::{drain_until, input_frame, spawn_frame};

mod support;

// -- the retained history -------------------------------------------------

/// History is what a fresh worker replays, and it must carry the output in the
/// order the terminal produced it.
#[test]
fn history_replays_output_in_order() {
    let mut history = ChannelHistory::new();
    history.record_output(1, b"first");
    history.record_output(2, b"second");
    history.record_output(3, b"third");

    let records = history.records();
    assert_eq!(records.records.len(), 3);
    let mut text = Vec::new();
    for record in &records.records {
        if let HistoryRecord::Output { bytes, .. } = record {
            text.extend_from_slice(bytes);
        }
    }
    assert_eq!(text, b"firstsecondthird");
}

/// A duplicate sequence is dropped rather than stored. History is a log, and a
/// duplicate makes a replay ambiguous about which copy the client already had.
#[test]
fn a_replayed_sequence_is_not_stored_twice() {
    let mut history = ChannelHistory::new();
    history.record_output(5, b"real");
    history.record_output(5, b"duplicate");
    assert_eq!(history.len(), 1);
    assert_eq!(history.head_seq(), 5, "the head never goes backwards");
}

/// Geometry records are evicted BEFORE output. A client that lost the geometry
/// can still replay the bytes and ask for a snapshot; one that replayed bytes
/// against the wrong geometry paints a screen that was never on that terminal.
#[test]
fn geometry_records_are_evicted_before_output_bytes() {
    let mut history = ChannelHistory::with_limits(200, 1);
    for seq in 1..=6 {
        history.record_resize(
            seq,
            TerminalState {
                applied_seq: seq,
                cols: 80,
                rows: 24,
            },
        );
    }
    history.record_output(7, b"kept");

    let records = history.records();
    let geometry = records
        .records
        .iter()
        .filter(|r| matches!(r, HistoryRecord::Resize { .. }))
        .count();
    assert!(geometry <= 1, "the geometry bound held: {geometry} records");
    assert!(
        records
            .records
            .iter()
            .any(|r| matches!(r, HistoryRecord::Output { .. })),
        "output outlived the geometry"
    );
}

/// The byte bound is on BYTES, not records: what a replay costs is bytes, and a
/// record-count bound lets one chatty channel evict every other channel's data.
#[test]
fn the_history_bound_is_on_bytes_not_records() {
    let mut history = ChannelHistory::with_limits(64, 4096);
    for seq in 1..=100 {
        history.record_output(seq, &[b'x'; 32]);
    }
    assert!(
        history.retained_bytes() <= 64,
        "retained {} bytes against a 64 byte bound",
        history.retained_bytes()
    );
    assert!(
        history.head_seq() > 1,
        "the head survives eviction, or a gap is undetectable"
    );
}

/// A drain filters to what the client has not seen, which is how a resuming
/// worker avoids replaying a screenful it already has.
#[test]
fn a_drain_returns_only_what_the_client_has_not_seen() {
    let mut history = ChannelHistory::new();
    history.record_output(1, b"a");
    history.record_output(2, b"b");
    history.record_output(3, b"c");

    let fresh = history.drain_for(0);
    assert_eq!(fresh.records.len(), 3);
    let resumed = history.drain_for(2);
    assert_eq!(
        resumed.records.len(),
        1,
        "only the unseen record is replayed"
    );
    assert_eq!(resumed.records[0].sequence(), 3);
}

/// The keeper stamps output with the sequence it EMITTED, not the one the
/// program wrote, so the head matches what a client could actually have seen.
#[test]
fn emitted_output_carries_a_monotonic_sequence() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    keeper.handle(&input_frame(1, 1, b"one\r"));
    drain_until(&mut keeper, |seen| seen.windows(3).any(|w| w == b"one"));
    keeper.handle(&input_frame(1, 2, b"two\r"));
    let seen = drain_until(&mut keeper, |seen| seen.windows(3).any(|w| w == b"two"));

    let (head, records) = keeper.legacy_history(1).expect("the channel has history");
    assert!(head > 0, "a channel that has emitted has a head");
    let mut previous = 0;
    for record in &records.records {
        assert!(
            record.sequence() > previous,
            "sequences never repeat or go backwards"
        );
        previous = record.sequence();
    }
    assert!(seen.windows(3).any(|w| w == b"two"));
}
