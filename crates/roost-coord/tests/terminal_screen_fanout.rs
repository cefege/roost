//! What the replica leaves room for on the way out: every planned snapshot part
//! still fits once a recipient's egress stamps its fan-out time, a delta that
//! leaves no room for that stamp is repaired rather than fanned out, ingress
//! timing survives every cache rebuild, and history never counts against the
//! residency budget.
//!
//! Ports `apps/coord/tests/terminal/screen/terminal-screen-fanout-headroom.test.ts`
//! and `terminal-screen-cache-accounting.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_hub_support;

use roost_coord::sync_ws::retained_frame::SharedCellFrame;
use roost_coord::terminal_screen::screen_budget::TerminalScreenCaps;
use roost_proto::PbCellGridFrame;
use roost_protocol::cell::CELL_GRID_PART_MAX_BYTES;
use roost_protocol::cell::frame_chunk_validation::{
    CELL_GRID_COORD_FANOUT_STAMP_MAX, CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES,
};
use roost_protocol::cell::frame_chunks::{
    encoded_cell_grid_chunk_size, encoded_cell_grid_frame_size,
};
use terminal_screen_hub_support::{
    SESSION, SNAPSHOT_A, STREAM, TestSink, baseline, delta, delta_frame, full_frame, harness,
    harness_with_caps, request, row, row_chunks, session, watch,
};

/// The largest frame `frame_for_text` builds that still encodes within
/// `maximum_bytes`.
fn largest_fitting_frame(
    frame_for_text: impl Fn(usize) -> PbCellGridFrame,
    maximum_bytes: u32,
) -> PbCellGridFrame {
    let (mut lower, mut upper) = (0_usize, maximum_bytes as usize);
    let mut best = frame_for_text(0);
    while lower <= upper {
        let length = (lower + upper) / 2;
        let candidate = frame_for_text(length);
        if encoded_cell_grid_frame_size(&candidate) <= maximum_bytes {
            best = candidate;
            lower = length + 1;
        } else {
            upper = length - 1;
        }
    }
    best
}

/// `frame` fits one part, but not once the widest fan-out stamp is written.
fn assert_fanout_boundary(frame: &PbCellGridFrame) {
    let encoded = encoded_cell_grid_frame_size(frame);
    assert!(encoded <= CELL_GRID_PART_MAX_BYTES);
    assert!(encoded > CELL_GRID_PART_MAX_BYTES - CELL_GRID_COORD_FANOUT_STAMP_MAX_ENCODED_BYTES);
    let stamped = PbCellGridFrame {
        coord_fanout_ms: CELL_GRID_COORD_FANOUT_STAMP_MAX,
        ..frame.clone()
    };
    assert!(encoded_cell_grid_frame_size(&stamped) > CELL_GRID_PART_MAX_BYTES);
}

// v2 "plans snapshot parts with recipient fanout stamp headroom".
#[test]
fn a_boundary_full_is_planned_in_parts_that_fit_once_stamped() {
    let source = largest_fitting_frame(
        |length| PbCellGridFrame {
            // The hub stamps its own session id before it plans, so the
            // boundary is measured with the id the plan will carry.
            session_id: SESSION.to_owned(),
            ..full_frame(
                STREAM,
                1,
                1,
                2,
                &["x".repeat(length).as_str(), "x".repeat(length).as_str()],
            )
        },
        CELL_GRID_PART_MAX_BYTES,
    );
    assert_fanout_boundary(&source);

    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 1, 2);
    h.frame(source);
    assert!(
        h.requests().is_empty(),
        "the boundary full itself is admissible unchunked"
    );

    let served = sink.snapshots().pop().expect("the watcher is seeded");
    assert!(
        served.parts.len() > 1,
        "a full that cannot take its stamp is chunked"
    );
    for part in served.parts {
        let SharedCellFrame::Chunk(chunk) = part else {
            panic!("every part of a boundary snapshot is a chunk");
        };
        assert_eq!(
            chunk.part.as_option().map(|part| part.coord_fanout_ms),
            Some(CELL_GRID_COORD_FANOUT_STAMP_MAX)
        );
        assert!(encoded_cell_grid_chunk_size(&chunk) <= CELL_GRID_PART_MAX_BYTES);
    }
}

// v2 "repairs a delta that leaves no recipient fanout stamp headroom".
#[test]
fn a_delta_that_leaves_no_stamp_headroom_is_repaired_not_fanned_out() {
    let boundary = largest_fitting_frame(
        |length| PbCellGridFrame {
            session_id: SESSION.to_owned(),
            cols: 1,
            rows: 1,
            ..delta_frame(STREAM, 1, 0, "x".repeat(length).as_str())
        },
        CELL_GRID_PART_MAX_BYTES,
    );
    assert_fanout_boundary(&boundary);

    let h = harness();
    let sink = TestSink::queuing();
    watch(&h.hub, &sink, "socket-a");
    h.hub.expect_stream(&session(), STREAM, 1, 1);
    h.frame(full_frame(STREAM, 1, 1, 1, &[""]));
    assert!(h.requests().is_empty());

    h.frame(boundary);
    assert_eq!(h.requests(), [request(STREAM)]);
    assert!(
        sink.deltas.lock().unwrap().is_empty(),
        "no socket is handed a delta it cannot stamp"
    );
}

// v2 "preserves ingress timing through canonical snapshot cache updates".
#[test]
fn ingress_timing_survives_every_cache_rebuild() {
    let h = harness();
    let initial = TestSink::queuing();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    watch(&h.hub, &initial, "initial");
    h.frame(PbCellGridFrame {
        coord_recv_ms: 711,
        ..baseline(1, &[])
    });
    assert_eq!(initial.last_seeded().coord_recv_ms, 711);

    h.frame(PbCellGridFrame {
        coord_recv_ms: 722,
        ..delta(1, "changed")
    });
    let recovered = TestSink::queuing();
    watch(&h.hub, &recovered, "recovered");
    assert!(h.hub.seed_socket("recovered", &session()));
    assert_eq!(
        recovered.last_seeded().coord_recv_ms,
        722,
        "the fold carries the delta's receipt"
    );
}

// v2 "uses first receipt timing across an assembled chunked snapshot".
#[test]
fn an_assembled_snapshot_carries_its_first_chunk_receipt() {
    let h = harness();
    let sink = TestSink::queuing();
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    watch(&h.hub, &sink, "chunked");
    let mut parts = row_chunks(&baseline(1, &[]), SNAPSHOT_A);
    h.hub.publish_chunk(&session(), &mut parts[0], 811);
    h.hub.publish_chunk(&session(), &mut parts[1], 822);

    let stamped: Vec<u64> = parts
        .iter()
        .map(|chunk| chunk.part.as_option().map_or(0, |part| part.coord_recv_ms))
        .collect();
    assert_eq!(stamped, [811, 811], "every part carries the first receipt");
    assert!(h.requests().is_empty());
    assert_eq!(sink.last_seeded().coord_recv_ms, 811);
}

// v2 terminal-screen-cache-accounting.test.ts "full and live history never
// enter cache residency accounting". The budget below holds exactly one
// two-row, two-span viewport: any history counted against it is refused.
#[test]
fn history_on_a_full_or_a_live_delta_never_counts_against_the_budget() {
    let h = harness_with_caps(TerminalScreenCaps {
        max_resident_rows: 2,
        max_resident_spans: 2,
    });
    h.hub.expect_stream(&session(), STREAM, 8, 2);

    let mut legacy = baseline(1, &[]);
    legacy.scrollback_rows = (0..300)
        .map(|index| row(index, &format!("history-{index}")))
        .collect();
    legacy.scrollback_total = 300;
    legacy.sb_base = 0;
    h.frame(legacy);
    assert!(
        h.unavailable().is_empty(),
        "300 history rows were not charged"
    );
    assert_eq!(h.replica(), Some((1, true)));

    let mut scrolled = delta(1, "changed");
    scrolled.scrollback_append = (0..300)
        .map(|index| row(300 + index, &format!("append-{index}")))
        .collect();
    scrolled.scrollback_total = 600;
    h.frame(scrolled);
    assert_eq!(h.replica(), Some((2, true)));
    assert!(h.unavailable().is_empty() && h.requests().is_empty());
}
