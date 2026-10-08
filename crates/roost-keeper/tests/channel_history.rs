#![cfg(unix)]
//! The keeper's retained history, which is what a fresh worker replays to
//! rebuild a channel it did not spawn: v2 `keeper/keeper-history.ts` semantics
//! (head counts every byte, resize markers split the window, an evicted marker
//! becomes the base geometry, a full marker budget discards the window) and
//! `keeper-history-resume.test.ts` against a live channel.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_keeper::channel_history::ChannelHistory;
use roost_keeper::history::{HistoryRecord, HistoryRecords};
use roost_keeper::keeper::Keeper;
use support::{drain_until, input_frame, spawn_frame};

mod support;

fn output(bytes: &[u8]) -> HistoryRecord {
    HistoryRecord::Output {
        bytes: bytes.to_vec(),
    }
}

/// Under the cap the ring holds the whole lifetime, so the head (every byte
/// ever emitted) equals the retained window and the base is the spawn geometry.
#[test]
fn the_head_counts_every_emitted_byte() {
    let mut history = ChannelHistory::new(100, 30);
    history.record_output(b"first ");
    history.record_output(b"second");

    assert_eq!(
        history.ordered(),
        HistoryRecords {
            head_seq: 12,
            base_cols: 100,
            base_rows: 30,
            records: vec![output(b"first second")]
        }
    );
}

/// A resize marker cuts the output at the byte it became effective, so a
/// replay reflows exactly the lines the PTY reflowed.
#[test]
fn a_resize_marker_splits_the_window_at_its_byte() {
    let mut history = ChannelHistory::new(80, 24);
    history.record_output(b"before");
    history.record_resize(4, 132, 43);
    history.record_output(b"after");

    assert_eq!(
        history.ordered().records,
        vec![
            output(b"before"),
            HistoryRecord::Resize {
                seq: 4,
                cols: 132,
                rows: 43
            },
            output(b"after")
        ]
    );
}

/// The ring evicts over a marker, and that marker's geometry is then what the
/// oldest retained byte was produced at: the BASE, not the spawn geometry.
#[test]
fn an_evicted_marker_becomes_the_base_geometry() {
    let mut history = ChannelHistory::with_limits(80, 24, 8, 4096);
    history.record_output(b"0123456789");
    history.record_resize(1, 100, 40);
    history.record_output(b"abcdefghijklmnopqrst");

    let ordered = history.ordered();
    assert_eq!(ordered.head_seq, 30, "the head survives eviction");
    assert_eq!((ordered.base_cols, ordered.base_rows), (100, 40));
    assert_eq!(ordered.records, vec![output(b"mnopqrst")]);
}

/// A resize-only flood cannot grow the marker list without bound: a full
/// budget discards the raw window, because bytes under an unknowable geometry
/// are worse than fewer records.
#[test]
fn a_full_marker_budget_discards_the_retained_window() {
    let mut history = ChannelHistory::with_limits(80, 24, 1024, 2);
    history.record_output(b"old");
    history.record_resize(1, 90, 30);
    history.record_resize(2, 91, 31);
    history.record_resize(3, 92, 32);

    let ordered = history.ordered();
    assert_eq!(ordered.head_seq, 3);
    assert_eq!(
        (ordered.base_cols, ordered.base_rows),
        (91, 31),
        "the base is the geometry before the kept marker"
    );
    assert_eq!(
        ordered.records,
        vec![HistoryRecord::Resize {
            seq: 3,
            cols: 92,
            rows: 32
        }]
    );
}

/// A channel the keeper does not hold answers "nothing emitted" at the default
/// geometry rather than failing (v2 `keeper-frame-handler.ts:510-515`).
#[test]
fn an_unknown_channel_answers_an_empty_history() {
    let keeper = Keeper::new();
    assert_eq!(
        keeper.ordered_history(60_000),
        HistoryRecords::unknown_channel()
    );
    assert_eq!(keeper.legacy_history(60_000), None);
}

/// v2 `keeper-history-resume.test.ts`: the echoed output is retained, the head
/// equals the ring under the cap, and it advances as output accrues.
#[test]
fn a_live_channel_retains_its_echo_and_its_head_advances() {
    let mut keeper = Keeper::new();
    keeper.handle(&spawn_frame(1, 80, 24));
    keeper.handle(&input_frame(1, 1, b"first-chunk-RC2\r"));
    drain_until(&mut keeper, |seen| {
        seen.windows(15).any(|w| w == b"first-chunk-RC2")
    });
    let (first_head, first_ring) = keeper.legacy_history(1).expect("the channel has history");
    assert_eq!(
        first_head,
        first_ring.len() as u64,
        "under the cap the ring is the whole lifetime"
    );

    keeper.handle(&input_frame(1, 2, b"second-chunk-RC2\r"));
    drain_until(&mut keeper, |seen| {
        seen.windows(16).any(|w| w == b"second-chunk-RC2")
    });
    let (second_head, second_ring) = keeper.legacy_history(1).expect("the channel has history");
    assert!(second_head > first_head, "the head advances with output");
    let text = String::from_utf8_lossy(&second_ring);
    assert!(
        text.contains("first-chunk-RC2") && text.contains("second-chunk-RC2"),
        "{text}"
    );
    assert_eq!(keeper.ordered_history(1).head_seq, second_head);
}
