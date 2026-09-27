//! The predictive-echo state machine: the SRTT display gate, the two-epoch
//! confidence gate, and the four events that abandon a burst.
//!
//! Ported one-for-one from `apps/web/tests/renderer/predictiveEcho.test.ts`,
//! with each v2 test name quoted verbatim on the case it became. The clock and
//! the input sequence are parameters here, not a shared fixture, so each case
//! is decidable on its own terms; the ack and grace half of the engine is in
//! `predictive_echo_ack.rs`, which owns the four named guards.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use roost_client_core::client::predictive_echo::{PredictiveEcho, ResetReason};
use roost_client_core::store::prefs::PredictMode;
use roost_client_core::store::prefs::predict::parse;
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

/// A frame carrying `text` on one row, with the cursor where it is told.
fn frame(seq: u64, cursor_row: u32, cursor_col: u32, row: u32, text: &str) -> CellGridFrame {
    empty_frame(
        seq,
        cursor_row,
        cursor_col,
        vec![(row, Some(vec![plain_span(text)]))],
    )
}

/// A frame with no rows at all, which is what a coalesced empty batch carries.
fn empty_frame(
    seq: u64,
    cursor_row: u32,
    cursor_col: u32,
    rows: Vec<(u32, Option<Vec<CellSpan>>)>,
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
        viewport_rows: rows
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

/// An anchored predictor in `mode`, at t=0 with the cursor at column 0.
fn anchored(mode: PredictMode) -> PredictiveEcho {
    let mut echo = PredictiveEcho::new(mode);
    echo.on_frame(&empty_frame(1, 0, 0, Vec::new()), 0, false);
    echo
}

/// Type a keystroke AND acknowledge its PTY write.
fn typed(echo: &mut PredictiveEcho, bytes: &[u8], input_seq: u64, now_ms: u64) {
    echo.predict(bytes, input_seq, now_ms);
    echo.note_input_written(input_seq, now_ms);
}

#[test]
fn slow_link_first_keystroke_hidden_confirmed_next_keystroke_shown() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    let typed_state = echo.debug();
    assert_eq!(typed_state.total, 1);
    assert_eq!(
        typed_state.visible, 0,
        "tentative, and the RTT is unmeasured"
    );

    echo.on_frame(&frame(2, 0, 1, 0, "a"), 200, false);
    let confirmed = echo.debug();
    assert_eq!(confirmed.confirmed_epoch, 1);
    assert!(
        confirmed.srtt_ms > 100.0,
        "a 200 ms round trip was measured"
    );

    typed(&mut echo, b"b", 2, 210);
    assert_eq!(echo.debug().visible, 1, "same epoch, now proven, now shown");
}

#[test]
fn fast_link_predictions_made_but_never_shown() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 5, false);
    typed(&mut echo, b"b", 2, 6);

    let state = echo.debug();
    assert_eq!(state.total, 1, "the guess is still tracked");
    assert_eq!(
        state.visible, 0,
        "srtt/2 is inside the dead-band, so nothing paints"
    );
}

#[test]
fn shown_wrong_guess_hard_reset_falls_back_to_the_authoritative_grid() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 200, false);
    typed(&mut echo, b"b", 2, 210);
    assert_eq!(echo.debug().visible, 1, "b is shown");

    echo.on_frame(&frame(3, 0, 1, 0, "ax"), 410, false);
    let state = echo.debug();
    assert_eq!(
        state.total, 0,
        "a SHOWN guess that was wrong nukes the burst"
    );
    assert_eq!(state.last_reset, Some(ResetReason::Contradicted));
}

#[test]
fn alt_screen_suppresses_and_clears_predictions() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 1);

    let mut alt = empty_frame(2, 0, 0, Vec::new());
    alt.alt_screen = true;
    echo.on_frame(&alt, 0, false);
    assert_eq!(echo.debug().total, 0, "alt-screen coordinates are void");
    assert_eq!(echo.debug().last_reset, Some(ResetReason::AltScreen));

    typed(&mut echo, b"xyz", 2, 0);
    assert_eq!(echo.debug().total, 0, "and nothing is predicted into a TUI");
}

#[test]
fn esc_control_byte_refuses_and_bumps_the_epoch() {
    let mut echo = anchored(PredictMode::Adaptive);
    echo.predict(&[0x1b], 1, 0);
    assert_eq!(echo.debug().total, 0, "an escape sequence is not a glyph");
    assert_eq!(echo.debug().prediction_epoch, 2);

    typed(&mut echo, b"a", 2, 0);
    assert_eq!(echo.debug().visible, 0, "the re-armed epoch stays hidden");
}

#[test]
fn injected_never_mode_disables_prediction() {
    let mut echo = anchored(PredictMode::Never);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 0);
    assert_eq!(echo.paint_request(), None);
}

#[test]
fn mode_resolution_and_back_compat_aliases() {
    // The pane hands the predictor the mode the store resolved, so every stored
    // spelling has to reach the engine as the behaviour it names.
    for (stored, expected) in [
        (None, PredictMode::Adaptive),
        (Some("adaptive"), PredictMode::Adaptive),
        (Some("0"), PredictMode::Never),
        (Some("never"), PredictMode::Never),
        (Some("force"), PredictMode::Always),
        (Some("always"), PredictMode::Always),
        (Some("experimental"), PredictMode::Experimental),
    ] {
        assert_eq!(parse(stored), expected, "stored as {stored:?}");
        assert_eq!(PredictiveEcho::new(parse(stored)).mode(), expected);
        assert_eq!(expected.as_str(), parse(Some(expected.as_str())).as_str());
    }
}

#[test]
fn experimental_first_keystroke_shown_immediately() {
    let mut echo = anchored(PredictMode::Experimental);
    typed(&mut echo, b"a", 1, 0);
    let state = echo.debug();
    assert_eq!(state.mode, PredictMode::Experimental);
    assert_eq!(state.visible, 1, "no confidence gate and no RTT sample");
    assert_eq!(state.srtt_ms, 0.0);
}

#[test]
fn always_shown_after_the_epoch_confirms_even_on_a_fast_link() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(
        echo.debug().visible,
        0,
        "the first character is still tentative"
    );

    echo.on_frame(&frame(2, 0, 1, 0, "a"), 5, false);
    typed(&mut echo, b"b", 2, 6);
    assert_eq!(echo.debug().visible, 1, "always paints despite a 5 ms link");
}

#[test]
fn right_left_arrow_predicts_a_cursor_move() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, &[0x1b, 0x5b, 0x43], 1, 0);
    assert_eq!(echo.debug().predicted_cursor_col, Some(1));
    typed(&mut echo, &[0x1b, 0x5b, 0x43], 2, 0);
    assert_eq!(echo.debug().predicted_cursor_col, Some(2));
    typed(&mut echo, &[0x1b, 0x5b, 0x44], 3, 0);
    assert_eq!(echo.debug().predicted_cursor_col, Some(1));
    assert_eq!(
        echo.debug().total,
        0,
        "an arrow moves the caret and paints no glyph"
    );
}

#[test]
fn predicted_cursor_leads_the_echoed_chars_when_shown() {
    let mut echo = anchored(PredictMode::Experimental);
    typed(&mut echo, b"ab", 1, 0);
    let paint = echo.paint_request().expect("nothing hides these");
    assert_eq!(paint.cells.len(), 2);
    assert_eq!(
        paint.caret_col,
        Some(2),
        "a@0 and b@1, so the caret leads to 2"
    );
}

#[test]
fn experimental_wrong_guess_resets_only_that_cell() {
    let mut echo = anchored(PredictMode::Experimental);
    typed(&mut echo, b"ab", 1, 0);
    assert_eq!(echo.debug().visible, 2);

    // "a" echoed correctly, but column 1 is "x": past the ack and the grace, so
    // the contradiction is judgeable rather than pending.
    echo.on_frame(&frame(2, 0, 1, 0, "ax"), 100, false);
    let state = echo.debug();
    assert_eq!(state.mode, PredictMode::Experimental);
    assert_eq!(state.total, 0, "b dropped on its own");
    assert_eq!(state.reset_count, 0, "and no hard reset, no epoch kill");
}

#[test]
fn non_resize_full_frame_reconciles_instead_of_wiping() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 1);

    let mut full = frame(2, 0, 1, 0, "a");
    full.full = true;
    echo.on_frame(&full, 200, false);
    let state = echo.debug();
    assert_eq!(state.total, 0, "confirmed and retired, not wiped unjudged");
    assert!(
        state.srtt_ms > 0.0,
        "a wipe would have left the RTT unsampled"
    );
}

#[test]
fn resize_full_frame_still_wipes() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);

    let mut resized = frame(2, 0, 1, 0, "a");
    resized.full = true;
    resized.cols = 100;
    echo.on_frame(&resized, 200, false);
    let state = echo.debug();
    assert_eq!(
        state.total, 0,
        "the coordinates the burst addressed are gone"
    );
    assert_eq!(state.srtt_ms, 0.0, "so it was never judged");
    assert_eq!(state.last_reset, Some(ResetReason::Resized));
}

#[test]
fn a_batch_history_signal_clears_predictions_from_an_earlier_delta() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 1);

    // A coalesced batch that carried history the frame it hands over no longer
    // lists: the signal is separate from the frame's own append list.
    let mut carried = empty_frame(2, 0, 0, vec![(0, None)]);
    carried.full = true;
    echo.on_frame(&carried, 0, true);
    let state = echo.debug();
    assert_eq!(state.total, 0);
    assert_eq!(state.last_reset, Some(ResetReason::Scrolled));
}

#[test]
fn hysteresis_shows_through_the_dead_band_once_armed() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 200, false);

    // Drive the EWMA down into the 40-60 ms band, where a stateless gate would
    // disarm, with fifty-millisecond confirmations.
    let mut row = String::from("a");
    let mut now_ms = 200u64;
    let mut frame_seq = 3u64;
    for step in 0..29u8 {
        let character = char::from(b'b' + step);
        typed(
            &mut echo,
            character.to_string().as_bytes(),
            frame_seq,
            now_ms,
        );
        now_ms += 50;
        row.push(character);
        frame_seq += 1;
        echo.on_frame(
            &frame(frame_seq, 0, row.chars().count() as u32, 0, &row),
            now_ms,
            false,
        );
    }
    let mid = echo.debug();
    assert!(
        mid.srtt_ms > 40.0 && mid.srtt_ms <= 60.0,
        "genuinely in the band"
    );
    typed(&mut echo, b"0", frame_seq, now_ms);
    assert_eq!(
        echo.debug().visible,
        1,
        "the armed trigger survives the band"
    );
}

#[test]
fn paste_guard_resets_and_never_predicts() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 1);

    echo.predict(&[b'x'; 101], 2, 0);
    let state = echo.debug();
    assert_eq!(
        state.total, 0,
        "a paste floods the overlay and is unguessable"
    );
    assert_eq!(state.last_reset, Some(ResetReason::Paste));
}

#[test]
fn delta_frame_reconciles_and_is_not_wiped_by_dirty_count_drift() {
    // A delta carries only its CHANGED rows, so the row count in the frame is
    // the dirty-row count and drifts. Resize is detected from the viewport
    // HEIGHT instead, or consecutive deltas wipe every prediction.
    let mut echo = PredictiveEcho::new(PredictMode::Adaptive);
    echo.on_frame(&empty_frame(1, 0, 0, vec![(0, None), (1, None)]), 0, false);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 200, false);

    let state = echo.debug();
    assert_eq!(state.total, 0, "reconciled and retired, not wiped");
    assert!(
        state.srtt_ms > 0.0,
        "the round trip was sampled, so no wipe ran"
    );
}

#[test]
fn delta_confirm_reads_the_right_row_by_index_with_the_cursor_off_row_zero() {
    // Production deltas carry only dirty rows, so the row must be found by its
    // own index. A decoy row at index 0 makes an array-position lookup read the
    // wrong cell, which is the bottom-prompt case every shell has.
    let mut echo = PredictiveEcho::new(PredictMode::Adaptive);
    echo.on_frame(&empty_frame(1, 5, 0, vec![(5, None)]), 0, false);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(
        &empty_frame(
            2,
            5,
            1,
            vec![
                (0, Some(vec![plain_span("z")])),
                (5, Some(vec![plain_span("a")])),
            ],
        ),
        200,
        false,
    );

    let state = echo.debug();
    assert!(state.confirmed_epoch > 0, "a wipe leaves the epoch at zero");
    assert!(state.srtt_ms > 0.0);
}

#[test]
fn unrelated_sparse_delta_does_not_judge_a_pending_prediction() {
    let mut echo = anchored(PredictMode::Always);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 20, false);
    typed(&mut echo, b"b", 2, 20);
    assert_eq!(echo.debug().visible, 1);

    // A delta for a different row omits the prediction's row entirely, so it
    // cannot hold this cell's echo either way.
    echo.on_frame(&frame(3, 4, 1, 4, "x"), 60, false);
    let state = echo.debug();
    assert_eq!(state.total, 1);
    assert_eq!(state.visible, 1);
}
