//! The production `ChannelDelivery`'s three lanes: an open resize capture holds
//! and hands back, a trapped core retains without parsing, and everything else
//! parses. Moved from `runtime/channel_delivery.rs`'s unit tests; the trapped
//! lane ports v2 `emitUpstreamChunk`'s `!stream.coreValid` branch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_stream_support;

use roost_term::RioCore;
use roost_worker::runtime::channel_delivery::CAPTURE_CAP_BYTES;
use terminal_stream_support::{COLS, Harness, ROWS, STREAM_A, channel, held};

#[test]
fn an_open_capture_holds_output_and_hands_it_back_on_close() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    assert!(held(&harness.delivery).freeze_capture(channel(), 1));
    assert!(
        !held(&harness.delivery).freeze_capture(channel(), 1),
        "a second boundary cannot open"
    );
    harness.deliver(b"first ");
    harness.deliver(b"second");
    assert_eq!(
        harness.with_record(|record| record.head_seq),
        12,
        "the ring kept every byte"
    );
    let parsed = harness
        .with_record(|record| char::from_u32(record.terminal_core.viewport_cell(0, 0).character));
    assert_eq!(
        parsed,
        Some(' '),
        "nothing reached the core while the boundary was open"
    );
    let held_back = held(&harness.delivery).close_capture(channel());
    assert_eq!(held_back.bytes, b"first second");
    assert!(!held_back.overflowed);
    assert!(
        held(&harness.delivery)
            .close_capture(channel())
            .bytes
            .is_empty(),
        "a closed channel hands back nothing"
    );
}

#[test]
fn a_capture_past_the_retained_window_reports_rather_than_trims() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    held(&harness.delivery).freeze_capture(channel(), 1);
    harness.deliver(&vec![b'x'; CAPTURE_CAP_BYTES + 1]);
    let held_back = held(&harness.delivery).close_capture(channel());
    assert!(held_back.overflowed);
    assert!(held_back.bytes.len() <= CAPTURE_CAP_BYTES);
}

#[tokio::test]
async fn a_trapped_core_takes_the_retain_only_lane_and_still_scans_alt_mode() {
    let harness = Harness::scripted(RioCore::new(COLS, ROWS));
    harness.enable(STREAM_A, COLS, ROWS).await;
    held(&harness.delivery)
        .stream_emission()
        .unwrap()
        .trap_core(channel());
    harness.deliver(b"hidden\x1b[?10");
    harness.deliver(b"49h");
    let (head, alt, parsed) = harness.with_record(|record| {
        (
            record.head_seq,
            record.alt_mode,
            record.terminal_core.using_alt_screen(),
        )
    });
    assert_eq!(head, 14, "retained");
    assert!(
        alt,
        "a split alt-screen entry is still stream truth behind a trapped core"
    );
    assert!(!parsed, "the trapped core itself parsed nothing");
}
