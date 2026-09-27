//! The ack/grace gate: a prediction is judged only against grid state that
//! could already hold its echo.
//!
//! Ported one-for-one from `apps/web/tests/predictiveEchoAck.test.ts`, and each
//! v2 test name is quoted verbatim on the case it became. These four are the
//! guards `docs/FAILURE-INDEX.md:2390` names, and each can fail on its own:
//!   - "an echo frame for an earlier keystroke never contradicts a later one"
//!   - "a reset re-arms the confidence gate"
//!   - "an echo that beats the write ack still unlocks the burst"
//!   - "a match that reproduces the cell's own text proves nothing"
//!
//! The rule under test is asymmetric on purpose. A proving match credits with
//! no acknowledgement at all, because waiting for one costs the first
//! characters of every burst a whole extra round trip of invisibility. A
//! contradiction requires the OPPOSITE: an acknowledged write AND a frame that
//! outlived the application's echo latency. Widening either side is the defect.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_client_core::client::predictive_echo::PredictiveEcho;
use roost_client_core::client::predictive_echo::report::ResetReason;
use roost_client_core::store::prefs::PredictMode;
use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, MouseTracking};

/// One default-styled run of `text`, one column per scalar.
fn plain_span(text: &str) -> CellSpan {
    CellSpan {
        text: text.to_string(),
        fg: 256,
        bg: 256,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: text.chars().count() as u32,
        link_uri: None,
        link_key: None,
    }
}

/// A viewport row that carries these spans, addressed by its grid index — a
/// delta carries only its DIRTY rows, so the index is not the array position.
fn row_at(index: u32, spans: Vec<CellSpan>) -> (u32, Option<Vec<CellSpan>>) {
    (index, Some(spans))
}

/// A viewport row the frame does not carry at all.
fn no_row(index: u32) -> (u32, Option<Vec<CellSpan>>) {
    (index, None)
}

/// A frame with the cursor where the predictor believes it is, and only the rows
/// the case needs.
fn frame(
    seq: u64,
    cursor_row: u32,
    cursor_col: u32,
    viewport: Vec<(u32, Option<Vec<CellSpan>>)>,
) -> CellGridFrame {
    CellGridFrame {
        stream_id: "echo-test:0".to_string(),
        grid_epoch: "echo-grid:0".to_string(),
        cols: 80,
        rows: 24,
        cursor_row,
        cursor_col,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        full: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        viewport_rows: viewport
            .into_iter()
            .map(|(index, spans)| CellRow {
                index,
                spans: Arc::from(spans.unwrap_or_default()),
            })
            .collect(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: seq.saturating_sub(1),
        seq,
    }
}

/// An anchored predictor in `mode`, with the clock and the input sequence both
/// reset — every case below drives its own `now_ms` and its own sequences.
fn anchored(mode: PredictMode) -> PredictiveEcho {
    let mut echo = PredictiveEcho::new(mode);
    echo.on_frame(&frame(1, 0, 0, vec![no_row(0)]), 0, false);
    echo
}

/// Type a keystroke AND acknowledge its PTY write, which is what every ordinary
/// keystroke has done by the time its echo can arrive.
fn typed(echo: &mut PredictiveEcho, bytes: &[u8], input_seq: u64, now_ms: u64) {
    echo.predict(bytes, input_seq, now_ms);
    echo.note_input_written(input_seq, now_ms);
}

#[test]
fn an_echo_frame_for_an_earlier_keystroke_never_contradicts_a_later_one() {
    // "abc" typed faster than the link echoes. The worker's write-ack and the
    // echo travel together, so when a's echo lands, b and c were acked only
    // moments ago — inside the application's own echo latency.
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    echo.predict(b"b", 2, 0);
    let sequence_of_c = 3;
    echo.predict(b"c", sequence_of_c, 0);
    echo.note_input_written(sequence_of_c, 190);
    echo.on_frame(
        &frame(2, 0, 1, vec![row_at(0, vec![plain_span("a")])]),
        200,
        false,
    );

    let state = echo.debug();
    assert_eq!(state.total, 2, "b and c survive as pending");
    assert_eq!(state.confirmed_epoch, 1, "a's echo still unlocks the burst");
    assert_eq!(state.reset_count, 0, "a surviving guess is not a reset");
}

#[test]
fn a_reset_re_arms_the_confidence_gate() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(
        &frame(2, 0, 1, vec![row_at(0, vec![plain_span("a")])]),
        200,
        false,
    );
    typed(&mut echo, b"b", 2, 210);
    assert_eq!(echo.debug().visible, 1, "shown on a proven epoch");

    echo.on_frame(
        &frame(3, 0, 1, vec![row_at(0, vec![plain_span("ax")])]),
        410,
        false,
    );
    assert_eq!(echo.debug().last_reset, Some(ResetReason::Contradicted));

    typed(&mut echo, b"c", 3, 420);
    let state = echo.debug();
    // The keystroke after a reset anchors on a cursor column that still lags the
    // un-echoed input, so it must stay hidden until an echo reproves the epoch.
    assert_eq!(state.visible, 0);
    assert!(state.prediction_epoch > state.confirmed_epoch);
}

#[test]
fn an_echo_that_beats_the_write_ack_still_unlocks_the_burst() {
    let mut echo = anchored(PredictMode::Always);
    echo.predict(b"a", 1, 0);
    // The echo and the write ack race; when the echo wins, waiting for the ack
    // would hide the first characters of the burst for another whole round trip.
    echo.on_frame(
        &frame(2, 0, 1, vec![row_at(0, vec![plain_span("a")])]),
        120,
        false,
    );

    let state = echo.debug();
    assert_eq!(state.total, 0);
    assert_eq!(state.confirmed_epoch, 1);
    assert!(state.srtt_ms > 0.0, "the round trip was still measured");
}

#[test]
fn a_match_that_reproduces_the_cells_own_text_proves_nothing() {
    let mut echo = PredictiveEcho::new(PredictMode::Always);
    // "a" is ALREADY at that column, so the frame is not evidence our echo
    // landed: retire the guess, but do not unlock the epoch on it.
    echo.on_frame(
        &frame(1, 0, 0, vec![row_at(0, vec![plain_span("a")])]),
        0,
        false,
    );
    echo.predict(b"a", 1, 0);
    echo.on_frame(
        &frame(2, 0, 1, vec![row_at(0, vec![plain_span("a")])]),
        120,
        false,
    );

    let state = echo.debug();
    assert_eq!(state.total, 0, "the guess is retired");
    assert_eq!(state.confirmed_epoch, 0, "and it unlocked nothing");
    assert_eq!(state.srtt_ms, 0.0, "so no round trip was sampled either");
}

#[test]
fn a_later_coincidental_match_cannot_unlock_a_tentative_epoch() {
    let mut echo = anchored(PredictMode::Always);
    echo.predict(b"a", 1, 0);
    let sequence_of_b = 2;
    echo.predict(b"b", sequence_of_b, 0);
    echo.note_input_written(sequence_of_b, 0);
    echo.on_frame(
        &frame(2, 0, 2, vec![row_at(0, vec![plain_span("zb")])]),
        20,
        false,
    );

    // "b" landed, but "a" at column 0 was judged first and unlocked nothing, so
    // the epoch is still unproven and the later match must not unlock it.
    let state = echo.debug();
    assert_eq!(state.total, 2);
    assert_eq!(state.confirmed_epoch, 0);
    assert_eq!(state.visible, 0);
}

#[test]
fn a_sparse_cursor_frame_does_not_double_count_pending_absolute_columns() {
    let mut echo = anchored(PredictMode::Always);
    echo.predict(b"abc", 1, 0);
    echo.on_frame(&frame(2, 0, 2, vec![]), 0, false);

    assert_eq!(
        echo.debug().predicted_cursor_col,
        Some(3),
        "the caret is the last survivor's column plus its own glyph"
    );
}

#[test]
fn a_prediction_is_not_judged_before_its_write_is_acknowledged() {
    let mut echo = anchored(PredictMode::Always);
    let sequence = 1;
    echo.predict(b"a", sequence, 0);
    // A frame the worker produced without provably having written "a" cannot
    // contradict it, however stale the prediction looks.
    echo.on_frame(
        &frame(2, 0, 0, vec![row_at(0, vec![plain_span("z")])]),
        200,
        false,
    );
    let unproven = echo.debug();
    assert_eq!(unproven.total, 1);
    assert_eq!(unproven.confirmed_epoch, 0);

    echo.note_input_written(sequence, 200);
    echo.on_frame(
        &frame(3, 0, 0, vec![row_at(0, vec![plain_span("z")])]),
        400,
        false,
    );
    assert_eq!(echo.debug().total, 0, "now judgeable, and wrong");
}

#[test]
fn a_contradiction_inside_the_grace_window_is_not_a_reset() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(
        &frame(2, 0, 0, vec![row_at(0, vec![plain_span("z")])]),
        20,
        false,
    );
    assert_eq!(
        echo.debug().total,
        1,
        "the application may still be echoing"
    );
    assert_eq!(echo.debug().reset_count, 0);

    echo.on_frame(
        &frame(3, 0, 0, vec![row_at(0, vec![plain_span("z")])]),
        120,
        false,
    );
    assert_eq!(echo.debug().total, 0, "outlived the grace, so it is wrong");
}

#[test]
fn expiry_abandons_a_prediction_the_application_never_echoes() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 1);
    assert!(echo.expiry_delay_ms().is_some());

    // Past the expiry floor with the RTT still unmeasured.
    echo.expire_predictions(1_500);
    let state = echo.debug();
    assert_eq!(state.total, 0);
    assert_eq!(state.last_reset, Some(ResetReason::Expired));
    assert_eq!(echo.expiry_delay_ms(), None, "nothing left to arm");
}

#[test]
fn backspace_paints_an_erase_cell_and_a_glyph_supersedes_it() {
    let mut echo = PredictiveEcho::new(PredictMode::Always);
    echo.on_frame(
        &frame(1, 0, 1, vec![row_at(0, vec![plain_span("a")])]),
        0,
        false,
    );
    typed(&mut echo, b"b", 1, 0);
    echo.on_frame(
        &frame(2, 0, 2, vec![row_at(0, vec![plain_span("ab")])]),
        200,
        false,
    );

    typed(&mut echo, b"\x7f", 2, 210);
    let erased = echo.paint_request().expect("the erase is painted");
    assert_eq!(erased.cells[0].col, 1);
    assert_eq!(erased.cells[0].ch, "", "an erase paints no glyph");
    assert_eq!(erased.caret_col, Some(1), "the caret steps back over it");

    typed(&mut echo, b"z", 3, 215);
    let retyped = echo.paint_request().expect("the retyped glyph is painted");
    assert_eq!(
        retyped.cells.len(),
        1,
        "the eraser was superseded, not added to"
    );
    assert_eq!(retyped.cells[0].ch, "z");
    assert_eq!(retyped.caret_col, Some(2));
}

#[test]
fn backspace_refuses_a_styled_cell() {
    let mut echo = anchored(PredictMode::Always);
    // A non-default background is exactly what an erase would paint over.
    let mut styled = plain_span("ab");
    styled.bg = 1;
    echo.on_frame(&frame(1, 0, 2, vec![row_at(0, vec![styled])]), 0, false);
    typed(&mut echo, b"\x7f", 1, 0);

    let state = echo.debug();
    assert_eq!(state.total, 0, "nothing is painted over a styled cell");
    assert_eq!(
        state.prediction_epoch, 2,
        "and the refusal re-arms the gate"
    );
}

#[test]
fn backspace_refuses_a_wide_glyph() {
    let mut echo = anchored(PredictMode::Always);
    let mut wide = plain_span("ab");
    wide.text = "中".to_string();
    wide.columns = 2;
    echo.on_frame(&frame(1, 0, 2, vec![row_at(0, vec![wide])]), 0, false);
    typed(&mut echo, b"\x7f", 1, 0);

    let state = echo.debug();
    assert_eq!(state.total, 0, "an atomic span is two columns wide");
    assert_eq!(state.prediction_epoch, 2);
}

#[test]
fn an_erase_match_does_not_unlock_the_epoch() {
    let mut echo = PredictiveEcho::new(PredictMode::Always);
    echo.on_frame(
        &frame(1, 0, 2, vec![row_at(0, vec![plain_span("ab")])]),
        0,
        false,
    );
    typed(&mut echo, b"\x7f", 1, 0);
    assert_eq!(echo.debug().total, 1);

    // The cell is blank now, which an UNTOUCHED cell also reads as.
    echo.on_frame(
        &frame(2, 0, 1, vec![row_at(0, vec![plain_span("a ")])]),
        200,
        false,
    );
    let state = echo.debug();
    assert_eq!(state.total, 0, "retired");
    assert_eq!(state.confirmed_epoch, 0, "a blank cell is no evidence");
    assert_eq!(state.srtt_ms, 0.0);
}
