//! The residency budget, and what a screen past it does.
//!
//! `terminal-screen-budget.ts` is 79 lines of limits, and every one of them is
//! the difference between a screen that degrades and a screen that corrupts.
//! The assertion these tests exist for is the DEGRADE: a baseline the pool
//! cannot pay for is dropped, named unavailable, and signalled -- never trimmed
//! to fit, because a partially-copied grid is a grid a browser cannot tell is
//! wrong.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::terminal_screen::replica::{ScreenHub, ScreenReplicaSink};
use roost_coord::terminal_screen::screen_budget::{
    TerminalScreenCaps, sync_backpressure_bytes, terminal_screen_budget_bytes, terminal_screen_caps,
};
use roost_protocol::cell::{CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS};
use roost_protocol::viewport::TERMINAL_MAX_ROWS;
use roost_protocol::wire::SessionId;

const STREAM: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const COLS: u32 = 80;
const ROWS: u32 = 24;

fn session(tail: &str) -> SessionId {
    SessionId::try_from(format!("00000000-0000-4000-8000-{tail:0>12}").as_str()).unwrap()
}

/// One painted row on the wire, carrying a single span.
fn proto_row(index: u32) -> roost_proto::PbCellRow {
    roost_proto::PbCellRow {
        index,
        spans: vec![roost_proto::PbCellSpan {
            text: "$ ".to_owned(),
            fg: 0,
            bg: 0,
            flags: 0,
            fg_rgb: None,
            bg_rgb: None,
            columns: 2,
            link_uri: None,
            link_key: None,
            __buffa_unknown_fields: Default::default(),
        }],
        __buffa_unknown_fields: Default::default(),
    }
}

fn baseline_proto(seq: u64) -> roost_proto::PbCellGridFrame {
    baseline_proto_sized(seq, ROWS)
}

/// A baseline of `rows` rows. The row count has to match the rows the frame
/// actually carries: a snapshot validator that is doing its job refuses a
/// frame which declares more rows than it paints.
fn baseline_proto_sized(seq: u64, rows: u32) -> roost_proto::PbCellGridFrame {
    roost_proto::PbCellGridFrame {
        stream_id: STREAM.to_owned(),
        grid_epoch: "grid-1".to_owned(),
        cols: COLS,
        rows,
        full: true,
        seq,
        viewport_rows: (0..rows).map(proto_row).collect(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        cursor_visible: true,
        ..Default::default()
    }
}

/// Records what the hub told its sink, so a test can name the action taken.
#[derive(Default)]
struct RecordingSink {
    unavailable: Mutex<Vec<(String, String)>>,
    full_accepted: Mutex<Vec<String>>,
}

impl ScreenReplicaSink for RecordingSink {
    fn unavailable(&self, session_id: &SessionId, reason: &str) {
        self.unavailable
            .lock()
            .expect("the recording sink is not poisoned")
            .push((session_id.as_str().to_owned(), reason.to_owned()));
    }

    fn full_accepted(&self, session_id: &SessionId, _stream_id: &str) {
        self.full_accepted
            .lock()
            .expect("the recording sink is not poisoned")
            .push(session_id.as_str().to_owned());
    }
}

fn roomy_caps() -> TerminalScreenCaps {
    TerminalScreenCaps {
        max_resident_rows: 65_536,
        max_resident_spans: 2_097_152,
    }
}

#[test]
fn a_declared_budget_never_falls_below_one_worst_styled_max_geometry_session() {
    // The floor is not a nicety: a budget that could not hold a single 256-row
    // terminal would make an unpaintable screen a configuration choice.
    for budget in [0u64, 1_024, 65_536, 1_048_576, 268_435_456] {
        let caps = terminal_screen_caps(budget);
        assert!(
            caps.max_resident_rows >= u64::from(TERMINAL_MAX_ROWS),
            "the row ceiling floors at {TERMINAL_MAX_ROWS} for a {budget}-byte budget: {caps:?}"
        );
        assert!(
            caps.max_resident_spans >= u64::from(CELL_GRID_SNAPSHOT_MAX_SPANS),
            "the span ceiling floors at {CELL_GRID_SNAPSHOT_MAX_SPANS} for a {budget}-byte budget: {caps:?}"
        );
    }
}

#[test]
fn a_large_budget_clamps_at_the_hard_maxima_rather_than_growing_without_bound() {
    let caps = terminal_screen_caps(1 << 40);
    assert_eq!(caps.max_resident_rows, 65_536);
    assert_eq!(caps.max_resident_spans, 2_097_152);
}

#[test]
fn the_budget_bytes_are_a_quarter_of_the_detected_ceiling_unless_declared() {
    assert_eq!(terminal_screen_budget_bytes(None, 1_000_000), 250_000);
    assert_eq!(
        terminal_screen_budget_bytes(Some(777), 1_000_000),
        777,
        "an operator's declared budget wins over the detected ceiling"
    );
}

#[test]
fn a_socket_send_slice_is_two_whole_snapshot_parts_and_at_most_eight_mib() {
    assert_eq!(
        sync_backpressure_bytes(1),
        2 * u64::from(CELL_GRID_PART_MAX_BYTES)
    );
    assert_eq!(sync_backpressure_bytes(1 << 40), 8 * 1024 * 1024);
}

#[test]
fn a_screen_past_its_budget_is_dropped_and_named_unavailable_not_trimmed() {
    // Twelve rows of ceiling against a twenty-four-row baseline: the first
    // full cannot be paid for at all, which is the case a silent truncation
    // would turn into a half-painted grid.
    let sink = Arc::new(RecordingSink::default());
    let hub = ScreenHub::with_sink(
        TerminalScreenCaps {
            max_resident_rows: 12,
            max_resident_spans: 2_097_152,
        },
        sink.clone(),
    );
    let id = session("1");
    hub.expect_stream(&id, STREAM, COLS, ROWS);

    hub.publish_frame(&id, &mut baseline_proto(1), 1_000);

    assert_eq!(
        sink.unavailable
            .lock()
            .expect("the sink is not poisoned")
            .as_slice(),
        [(
            id.as_str().to_owned(),
            "coordinator terminal cache capacity exceeded".to_owned()
        )],
        "an over-budget baseline DEGRADES: the sink is told the session is unavailable, \
         and the reason names the budget rather than the bytes"
    );
    assert!(
        sink.full_accepted
            .lock()
            .expect("the sink is not poisoned")
            .is_empty()
    );
    assert!(
        !hub.has_valid_cache(&id),
        "a refused baseline leaves no half-served replica behind"
    );
}

#[test]
fn the_budget_is_shared_so_the_session_that_misses_out_is_named() {
    let sink = Arc::new(RecordingSink::default());
    let hub = ScreenHub::with_sink(
        TerminalScreenCaps {
            max_resident_rows: u64::from(ROWS) * 2,
            max_resident_spans: 2_097_152,
        },
        sink.clone(),
    );
    let (first, second, third) = (session("1"), session("2"), session("3"));
    for id in [&first, &second, &third] {
        hub.expect_stream(id, STREAM, COLS, ROWS);
        hub.publish_frame(id, &mut baseline_proto(1), 1_000);
    }

    assert!(
        hub.has_valid_cache(&first),
        "the first two fit and are resident"
    );
    assert!(hub.has_valid_cache(&second));
    assert!(
        !hub.has_valid_cache(&third),
        "the third does not fit and is not served"
    );
    assert_eq!(
        sink.unavailable
            .lock()
            .expect("the sink is not poisoned")
            .as_slice(),
        [(
            third.as_str().to_owned(),
            "coordinator terminal cache capacity exceeded".to_owned()
        )],
        "only the session that actually missed out is named unavailable"
    );
}

#[test]
fn an_admissible_baseline_is_installed_and_announced_once() {
    let sink = Arc::new(RecordingSink::default());
    let hub = ScreenHub::with_sink(roomy_caps(), sink.clone());
    let id = session("1");
    hub.expect_stream(&id, STREAM, COLS, ROWS);

    hub.publish_frame(&id, &mut baseline_proto(1), 1_000);

    assert!(hub.has_valid_cache(&id));
    assert_eq!(hub.expected_stream_id(&id).as_deref(), Some(STREAM));
    assert_eq!(
        sink.full_accepted
            .lock()
            .expect("the sink is not poisoned")
            .as_slice(),
        [id.as_str().to_owned()],
        "an admitted baseline is announced once, and only once"
    );
    assert!(
        sink.unavailable
            .lock()
            .expect("the sink is not poisoned")
            .is_empty()
    );
}

#[test]
fn a_delta_that_does_not_follow_the_baseline_invalidates_rather_than_skips() {
    let sink = Arc::new(RecordingSink::default());
    let hub = ScreenHub::with_sink(roomy_caps(), sink);
    let id = session("1");
    hub.expect_stream(&id, STREAM, COLS, ROWS);
    let mut baseline = baseline_proto(1);
    hub.publish_frame(&id, &mut baseline, 1_000);

    let mut orphan_delta = baseline_proto(7);
    orphan_delta.full = false;
    orphan_delta.base_seq = 99;
    orphan_delta.seq = 100;
    hub.publish_frame(&id, &mut orphan_delta, 1_100);

    assert!(
        !hub.has_valid_cache(&id),
        "a delta that cannot be applied is refused, not skipped: a skipped delta is a \
         permanently wrong grid that still looks complete"
    );
}

#[test]
fn a_geometry_change_drops_the_old_baseline_rather_than_reusing_it() {
    let sink = Arc::new(RecordingSink::default());
    let hub = ScreenHub::with_sink(roomy_caps(), sink.clone());
    let id = session("1");
    hub.expect_stream(&id, STREAM, COLS, ROWS);
    let mut baseline = baseline_proto(1);
    hub.publish_frame(&id, &mut baseline, 1_000);
    assert!(hub.has_valid_cache(&id));

    hub.expect_stream(&id, STREAM, COLS, 40);

    assert!(
        !hub.has_valid_cache(&id),
        "the old grid is not a prefix of a taller one, so it is dropped rather than stretched"
    );
    let mut resized = baseline_proto_sized(2, 40);
    hub.publish_frame(&id, &mut resized, 1_100);
    assert!(hub.has_valid_cache(&id));
}
