//! What one cell becomes, driven through the real emulator: column occupancy,
//! run coalescing, link runs, modes, the alt screen, the history window, and
//! the delta.
//!
//! The frame a browser paints is the observable this port has to preserve, so
//! every case asserts on text and column counts rather than on internals — the
//! internals are exactly what changed.

mod support;

use roost_protocol::cell::{CELL_BOLD, CELL_UNDERLINE, DEFAULT_COLOR, MAX_LINK_URI_BYTES};
use roost_term::{AlacrittyCore, TerminalCore, grid_delta_frame, grid_to_cell_frame, row_to_spans};

/// A frame row's text, exactly as the wire carries it.
///
/// Deliberately NOT padded out to `columns`: a span's `text` is the glyph and
/// its `columns` is the terminal-column occupancy.
fn frame_row_text(frame: &roost_protocol::cell::CellGridFrame, row: usize) -> String {
    frame.viewport_rows[row]
        .spans
        .iter()
        .map(|span| span.text.as_str())
        .collect()
}
use support::row_columns;

#[test]
fn plain_text_lands_on_the_row_it_was_written_to() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"hello");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    assert_eq!(frame_row_text(&frame, 0), "hello");
    assert!(frame.full, "a first frame is a full frame");
    assert_eq!(frame.rows, 5);
    assert_eq!(frame.cols, 20);
    assert_eq!(frame.cursor_row, 0);
    assert_eq!(frame.cursor_col, 5);
    assert!(frame.cursor_visible, "a fresh terminal shows its cursor");
}

#[test]
fn a_wide_glyph_occupies_two_columns_and_emits_one_span() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write("中文".as_bytes());
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let text = frame_row_text(&frame, 0);
    assert_eq!(
        text, "中文",
        "a wide glyph must occupy its columns, not one per span"
    );
    // Two glyphs, each its own atomic span: neither is a one-column run.
    let spans = &frame.viewport_rows[0].spans;
    assert_eq!(spans.len(), 2, "each wide glyph is one span");
    assert_eq!(spans[0].columns, 2);
    assert_eq!(spans[1].columns, 2);
    assert_eq!(spans[0].text, "中");
}

#[test]
fn a_wide_glyph_does_not_shift_the_columns_to_its_right() {
    // The failure this guards: a continuation emitted as its own cell paints
    // "中  文" and moves every column right of it by one.
    let mut core = AlacrittyCore::new(20, 5);
    core.write("a中b".as_bytes());
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    assert_eq!(frame_row_text(&frame, 0), "a中b");
    assert_eq!(row_columns(&frame.viewport_rows[0].spans), 4);
}

#[test]
fn a_narrow_run_coalesces_into_one_span_and_keeps_its_columns() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"abcdef");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let spans = &frame.viewport_rows[0].spans;
    assert_eq!(spans.len(), 1, "one style, one run");
    assert_eq!(spans[0].text, "abcdef");
    assert_eq!(spans[0].columns, 6);
}

#[test]
fn a_style_change_breaks_the_run() {
    let mut core = AlacrittyCore::new(20, 5);
    // Bold, then not: the run has to break even though the text does not.
    core.write(b"\x1b[1mab\x1b[0mcd");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let spans = &frame.viewport_rows[0].spans;
    assert_eq!(spans.len(), 2, "bold and plain are two runs");
    assert_eq!(spans[0].text, "ab");
    assert_ne!(spans[0].flags & CELL_BOLD, 0);
    assert_eq!(spans[1].text, "cd");
    assert_eq!(spans[1].flags & CELL_BOLD, 0);
}

#[test]
fn underline_reaches_the_wire_flag() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"\x1b[4mab");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let spans = &frame.viewport_rows[0].spans;
    assert_ne!(
        spans[0].flags & CELL_UNDERLINE,
        0,
        "an underlined cell must carry the wire's underline bit"
    );
}

#[test]
fn trailing_blanks_are_trimmed_and_an_empty_row_has_no_spans() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"hi\x1b[2J");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    for row in &frame.viewport_rows {
        assert!(
            row.spans
                .iter()
                .all(|span| span.text.trim() != "" || span.columns > 0),
            "a blank row must not carry padding spans"
        );
    }
    assert!(frame_row_text(&frame, 1).is_empty(), "row 1 is empty");
}

#[test]
fn an_osc_8_link_becomes_a_uri_and_a_run_key() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"\x1b]8;;https://example.test/doc\x1b\\linked\x1b]8;;\x1b\\ plain");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let spans = &frame.viewport_rows[0].spans;
    let linked = spans
        .iter()
        .find(|span| span.text == "linked")
        .expect("the linked run survives as its own span");
    assert_eq!(linked.link_uri.as_deref(), Some("https://example.test/doc"));
    assert!(
        linked.link_key.is_some(),
        "a link with no run key cannot be told from a neighbour"
    );
    let plain = spans
        .iter()
        .find(|span| span.text.contains("plain"))
        .expect("the unlinked run survives, coalesced with the space after it");
    assert_eq!(plain.link_uri, None, "the link ended");
    assert_ne!(
        linked.link_key, plain.link_key,
        "adjacent runs must not share a key"
    );
}

#[test]
fn two_runs_of_the_same_link_stay_two_spans() {
    // Same destination, different runs: merging them would ship one span
    // carrying one link and silently lose the other.
    let mut core = AlacrittyCore::new(20, 5);
    core.write(
        b"\x1b]8;;https://example.test/x\x1b\\one\x1b]8;;\x1b\\ plain \x1b]8;;https://example.test/x\x1b\\two\x1b]8;;\x1b\\",
    );
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let linked: Vec<_> = frame.viewport_rows[0]
        .spans
        .iter()
        .filter(|span| span.link_uri.is_some())
        .collect();
    assert_eq!(linked.len(), 2, "the two runs stay separate");
    assert!(linked[0].text.contains("one"));
    assert!(linked[1].text.contains("two"));
    assert_ne!(linked[0].link_key, linked[1].link_key);
}

#[test]
fn an_over_cap_link_drops_the_link_and_keeps_the_text() {
    let uri = format!("https://example.test/{}", "a".repeat(MAX_LINK_URI_BYTES));
    let mut core = AlacrittyCore::new(20, 5);
    core.write(format!("\x1b]8;;{uri}\x1b\\kept\x1b]8;;\x1b\\").as_bytes());
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    let kept = frame.viewport_rows[0]
        .spans
        .iter()
        .find(|span| span.text.contains("kept"))
        .expect("the text survives a dropped link");
    assert_eq!(
        kept.link_uri, None,
        "a truncated URI would point somewhere else entirely"
    );
    assert!(
        kept.link_key.is_none(),
        "a span with no URI may not carry a run key"
    );
}

#[test]
fn modes_reach_the_frame() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    assert!(frame.cursor_keys_app, "DECCKM");
    assert!(frame.bracketed_paste, "DECSET 2004");
    assert_eq!(
        frame.mouse_tracking,
        roost_protocol::cell::MouseTracking::PressRelease
    );
    assert!(frame.mouse_sgr, "DECSET 1006");
    assert!(frame.focus_events, "DECSET 1004");
    assert!(!frame.alt_screen);
}

#[test]
fn the_alt_screen_is_reported_and_its_grid_is_read() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"primary\x1b[?1049h\x1b[2J\x1b[Halt");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    assert!(frame.alt_screen, "the alt screen has its own grid");
    assert_eq!(
        frame_row_text(&frame, 0).trim_end(),
        "alt",
        "the alt grid is what the client paints"
    );
}

#[test]
fn any_motion_mouse_mode_is_folded_away() {
    // `1003` is folded to "no tracking" by the core, as v2 folded it: a
    // client that received it would have to report motion for a terminal the
    // product does not forward it for.
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"\x1b[?1003h");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    assert_eq!(
        frame.mouse_tracking,
        roost_protocol::cell::MouseTracking::None
    );
}

#[test]
fn history_is_addressed_by_a_monotonic_index_that_advances_past_the_ring() {
    let mut core = AlacrittyCore::with_history(20, 3, 5);
    // A cursor parked at the bottom so every line feed scrolls.
    for _ in 0..3 {
        core.write(b"x\n");
    }
    let before = core
        .discarded_line_count()
        .expect("the patched core reports it");
    for index in 0..12 {
        core.write(format!("line{index}\n").as_bytes());
    }
    let after = core
        .discarded_line_count()
        .expect("the patched core reports it");
    assert!(after > before, "a saturated ring evicts");
    let frame = grid_to_cell_frame(&core, 1, "g:0", "s", None, after);
    assert_eq!(
        frame.scrollback_total,
        after + core.scrollback_count() as u64,
        "the total is the eviction origin plus what is retained"
    );
    assert_eq!(
        frame.sb_base, after,
        "an unbounded tail starts at the floor"
    );
    // Indices are contiguous and monotonic, which is what a client splices on.
    let indices: Vec<u32> = frame.scrollback_rows.iter().map(|row| row.index).collect();
    let expected: Vec<u32> = (after..frame.scrollback_total).map(|i| i as u32).collect();
    assert_eq!(indices, expected, "history indices are contiguous");
}

#[test]
fn a_range_read_is_clamped_to_the_retained_window() {
    let mut core = AlacrittyCore::with_history(20, 3, 5);
    for _ in 0..3 {
        core.write(b"x\n");
    }
    for index in 0..12 {
        core.write(format!("line{index}\n").as_bytes());
    }
    let dropped = core
        .discarded_line_count()
        .expect("the patched core reports it");
    let retained = core.scrollback_count() as u64;
    let rows = roost_term::read_scrollback_range(&core, 0, retained + dropped, dropped);
    assert_eq!(
        rows.len() as u64,
        retained,
        "a range wider than the ring reads exactly what is retained"
    );
    assert_eq!(
        rows[0].index as u64, dropped,
        "the floor is the eviction origin"
    );
}

#[test]
fn a_delta_carries_only_the_rows_that_changed() {
    let mut core = AlacrittyCore::new(20, 5);
    core.write(b"one");
    let first = grid_to_cell_frame(&core, 1, "g:0", "s", Some(0), 0);
    core.clear_dirty();

    core.write(b"\r\ntwo");
    let delta = grid_delta_frame(&core, 0, 2, 1, "g:0", "s", 0);
    assert!(!delta.full, "a delta is not a full frame");
    assert_eq!(delta.base_seq, 1);
    assert_eq!(delta.seq, 2);
    assert!(
        delta.viewport_rows.iter().any(|row| row.index == 1),
        "the row that changed is in the delta"
    );
    // The cursor moved too, and a cursor move is a damage: a delta that omitted
    // the row the cursor left would leave a client painting a stale cursor.
    assert_eq!(delta.cursor_row, 1);
    assert_eq!(frame_row_text(&first, 0), "one");
    assert_eq!(frame_row_text(&delta, 1), "two");
}

#[test]
fn a_row_span_encoder_trims_a_trailing_blank_run() {
    let cells = vec![
        roost_term::CellData {
            character: 'a' as u32,
            ..Default::default()
        },
        roost_term::CellData::default(),
        roost_term::CellData::default(),
    ];
    let spans = row_to_spans(&cells, cells.len());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].text, "a");
    assert_eq!(spans[0].columns, 1);
    assert_eq!(spans[0].fg, DEFAULT_COLOR);
}
