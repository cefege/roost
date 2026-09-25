//! The cell wire boundary: what survives the round trip, and what is refused on
//! the way in.
//!
//! Column occupancy, hyperlink identity, geometry and sequence are checked at
//! this boundary in BOTH directions, because a frame that crosses it is about to
//! be painted: a row without its occupancy draws every wide glyph one column
//! short, and a delta that does not continue its predecessor installs a
//! generation no frame ever emitted.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_proto::{PbCellGridFrame, PbCellRow, PbCellSpan};
use roost_protocol::ProtocolError;
use roost_protocol::cell::proto::{
    cell_frame_to_proto, cell_row_from_proto, cell_row_to_proto, proto_to_cell_frame,
};
use roost_protocol::cell::types::{CellGridFrame, CellRow, CellSpan, DEFAULT_COLOR, MouseTracking};

const STREAM_ID: &str = "00000000-0000-4000-8000-000000000001";

fn span(text: &str, columns: u32) -> CellSpan {
    CellSpan {
        text: text.to_owned(),
        fg: DEFAULT_COLOR,
        bg: DEFAULT_COLOR,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns,
        link_uri: None,
        link_key: None,
    }
}

fn row(index: u32, spans: Vec<CellSpan>) -> CellRow {
    CellRow {
        index,
        spans: Arc::from(spans),
    }
}

fn frame() -> CellGridFrame {
    CellGridFrame {
        stream_id: STREAM_ID.to_owned(),
        grid_epoch: "test-grid:0".to_owned(),
        cols: 6,
        rows: 2,
        cursor_row: 1,
        cursor_col: 3,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: true,
        bracketed_paste: true,
        mouse_tracking: MouseTracking::ButtonMotion,
        mouse_sgr: true,
        focus_events: true,
        full: true,
        viewport_rows: vec![
            row(0, vec![span("hi", 2), span("中", 2)]),
            row(1, vec![span("tail", 4)]),
        ],
        scrollback_rows: vec![row(0, vec![span("old", 3)])],
        scrollback_append: Vec::new(),
        scrollback_total: 1,
        sb_base: 0,
        base_seq: 0,
        seq: 42,
    }
}

fn delta() -> CellGridFrame {
    CellGridFrame {
        full: false,
        viewport_rows: vec![row(1, vec![span("new", 3)])],
        scrollback_rows: Vec::new(),
        scrollback_append: vec![row(1, vec![span("new", 3)])],
        scrollback_total: 2,
        base_seq: 41,
        ..frame()
    }
}

/// The refusal as the log line would read it.
fn refusal(value: Result<impl Sized, ProtocolError>) -> String {
    value
        .err()
        .map(|error| error.to_string())
        .expect("expected a refusal")
}

fn encoded() -> PbCellGridFrame {
    cell_frame_to_proto(&frame(), "s").expect("a valid frame encodes")
}

#[test]
fn a_frame_survives_the_round_trip_with_its_session_id_stamped() {
    let original = frame();
    let wire = cell_frame_to_proto(&original, "sess-123").expect("a valid frame encodes");
    assert_eq!(wire.session_id, "sess-123");
    assert_eq!(proto_to_cell_frame(&wire).expect("it decodes"), original);
    assert!(wire.cursor_keys_app, "DECCKM rides the wire as well");
    assert!(wire.focus_events, "DECSET 1004 rides the wire as well");
}

#[test]
fn column_occupancy_rides_the_wire_and_is_required_on_decode() {
    let wire = encoded();
    let occupancy: Vec<(&str, u32)> = wire.viewport_rows[0]
        .spans
        .iter()
        .map(|span| (span.text.as_str(), span.columns))
        .collect();
    assert_eq!(occupancy, [("hi", 2), ("中", 2)]);

    // A producer that ships no occupancy paints every wide glyph one column
    // short, so it is refused at the boundary.
    let mut without_occupancy = wire.clone();
    without_occupancy.viewport_rows[0].spans[1].columns = 0;
    let reason = refusal(proto_to_cell_frame(&without_occupancy));
    assert!(reason.contains("claims 0 columns"), "{reason}");

    // Occupancy wider than the grid cannot be painted as sent.
    let mut overflowing = wire.clone();
    overflowing.viewport_rows[0].spans[0].columns = 40;
    let reason = refusal(proto_to_cell_frame(&overflowing));
    assert!(reason.contains("of a 6-column grid"), "{reason}");

    // A retained scrollback line keeps its write-time width, so the current grid
    // does not bound it.
    let mut wide_history = wire;
    wide_history.scrollback_rows[0].spans[0].columns = 200;
    assert!(proto_to_cell_frame(&wide_history).is_ok());
}

#[test]
fn a_link_without_its_run_identity_is_refused_on_decode() {
    // The check is on the decode half only, and that asymmetry is deliberate.
    // A frame on its way out was built by the emitter, which validates column
    // occupancy and link identity as it constructs each span; re-checking on
    // encode would cost a pass over every cell of every frame to catch a
    // producer that is this binary. A frame on its way IN came from a peer, so
    // it is checked there — which is the only place a hostile value can arrive.
    let mut unkeyed_uri = encoded();
    unkeyed_uri.viewport_rows[0].spans[0].link_uri = Some("https://example.test".to_owned());
    let reason = refusal(proto_to_cell_frame(&unkeyed_uri));
    assert!(reason.contains("link_key"), "{reason}");

    let mut keyed_without_uri = encoded();
    keyed_without_uri.viewport_rows[0].spans[0].link_key = Some("k".to_owned());
    let reason = refusal(proto_to_cell_frame(&keyed_without_uri));
    assert!(reason.contains("link_uri"), "{reason}");
}

#[test]
fn zero_geometry_is_refused() {
    let wire = PbCellGridFrame {
        stream_id: STREAM_ID.to_owned(),
        grid_epoch: "grid:0".to_owned(),
        full: true,
        seq: 1,
        ..Default::default()
    };
    let reason = refusal(proto_to_cell_frame(&wire));
    assert!(reason.contains("cell_frame"), "{reason}");
}

#[test]
fn a_full_baseline_must_carry_every_row_and_exactly_its_history() {
    let wire = encoded();

    let mut no_rows = wire.clone();
    no_rows.viewport_rows.pop();
    let reason = refusal(proto_to_cell_frame(&no_rows));
    assert!(reason.contains("required viewport rows"), "{reason}");

    let mut appended = wire.clone();
    appended.scrollback_append.push(PbCellRow {
        index: 1,
        ..Default::default()
    });
    let reason = refusal(proto_to_cell_frame(&appended));
    assert!(reason.contains("scrollback_append"), "{reason}");

    let mut short_history = wire.clone();
    short_history.scrollback_total = 2;
    let reason = refusal(proto_to_cell_frame(&short_history));
    assert!(reason.contains("does not cover"), "{reason}");

    let mut misplaced = wire;
    misplaced.scrollback_rows[0].index = 5;
    let reason = refusal(proto_to_cell_frame(&misplaced));
    assert!(reason.contains("out of order"), "{reason}");
}

#[test]
fn a_delta_must_continue_its_predecessor_by_one() {
    let original = delta();
    let wire = cell_frame_to_proto(&original, "s").expect("a valid delta encodes");
    assert_eq!(proto_to_cell_frame(&wire).expect("it decodes"), original);

    let mut skipped = wire.clone();
    skipped.seq = 44;
    skipped.base_seq = 41;
    let reason = refusal(proto_to_cell_frame(&skipped));
    assert!(reason.contains("does not follow base_seq"), "{reason}");

    let mut history_on_delta = wire.clone();
    history_on_delta.scrollback_rows = vec![PbCellRow {
        index: 0,
        ..Default::default()
    }];
    let reason = refusal(proto_to_cell_frame(&history_on_delta));
    assert!(reason.contains("delta cannot carry"), "{reason}");

    let mut unnamed = wire;
    unnamed.stream_id = "not-a-uuid".to_owned();
    let reason = refusal(proto_to_cell_frame(&unnamed));
    assert!(reason.contains("not a UUID"), "{reason}");
}

#[test]
fn an_empty_grid_epoch_is_refused_in_both_directions() {
    let mut unnamed = frame();
    unnamed.grid_epoch = String::new();
    let reason = refusal(cell_frame_to_proto(&unnamed, "s").map(|_| ()));
    assert!(reason.contains("grid_epoch"), "{reason}");

    let mut wire = encoded();
    wire.grid_epoch = String::new();
    let reason = refusal(proto_to_cell_frame(&wire));
    assert!(reason.contains("grid_epoch"), "{reason}");
}

#[test]
fn a_zero_sequence_is_refused_and_a_full_baseline_names_base_seq_zero() {
    let mut unsound = frame();
    unsound.seq = 0;
    let reason = refusal(cell_frame_to_proto(&unsound, "s").map(|_| ()));
    assert!(reason.contains("cell.seq"), "{reason}");

    let mut rebased = frame();
    rebased.base_seq = 7;
    let reason = refusal(cell_frame_to_proto(&rebased, "s").map(|_| ()));
    assert!(reason.contains("nonzero base_seq"), "{reason}");
}

#[test]
fn an_unknown_mouse_mode_decodes_as_no_tracking() {
    let wire = encoded();
    let mut reported = wire.clone();
    reported.mouse_tracking = 1003;
    let decoded = proto_to_cell_frame(&reported).expect("an unknown mode is not a refusal");
    assert_eq!(decoded.mouse_tracking, MouseTracking::None);
    let known = proto_to_cell_frame(&wire).expect("it decodes");
    assert_eq!(known.mouse_tracking, MouseTracking::ButtonMotion);
}

#[test]
fn a_row_converts_both_ways_and_keeps_its_true_colour_and_link() {
    let mut linked = span("ab", 2);
    linked.fg = 7;
    linked.bg = 8;
    linked.flags = 5;
    linked.fg_rgb = Some(0x00ff00);
    linked.bg_rgb = Some(0x101010);
    linked.link_uri = Some("https://example.test/x".to_owned());
    linked.link_key = Some("k-4".to_owned());
    let original = row(4, vec![linked]);

    let wire = cell_row_to_proto(&original);
    assert_eq!(wire.spans[0].fg, 7);
    assert_eq!(wire.spans[0].fg_rgb, Some(0x00ff00));

    let decoded = cell_row_from_proto(&wire).expect("a bounded row decodes");
    assert_eq!(decoded, original);
    assert_eq!(decoded.spans[0].link_key.as_deref(), Some("k-4"));
}

#[test]
fn a_palette_value_the_value_model_cannot_hold_is_refused_rather_than_truncated() {
    let wire = PbCellRow {
        index: 0,
        spans: vec![PbCellSpan {
            text: "x".to_owned(),
            columns: 1,
            fg: 70_000,
            ..Default::default()
        }],
        ..Default::default()
    };
    let reason = refusal(cell_row_from_proto(&wire));
    assert!(reason.contains("16-bit palette and flag range"), "{reason}");
}
