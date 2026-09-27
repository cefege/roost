//! The two-epoch confidence gate: a prediction stays hidden until an echo has
//! proven the epoch it was made in, and what happens when that proof arrives —
//! including the one cell that turns out wrong.
//!
//! Depends on the predictive-echo engine and the shared frame/typed fixtures in
//! `predictive_echo_support`. The display gate lives in `predictive_echo_gate.rs`
//! and the events that abandon a burst in `predictive_echo_reset.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod predictive_echo_support;
use predictive_echo_support::{anchored, frame, typed};

use roost_client_core::store::prefs::PredictMode;

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
