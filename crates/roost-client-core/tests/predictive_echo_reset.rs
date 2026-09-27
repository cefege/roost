//! The events that abandon a burst: a contradiction, alt-screen, a resize, a
//! history signal, a paste, and the reconciliation a full or delta frame
//! performs — each either wipes the burst outright or judges it against the
//! authoritative grid.
//!
//! Depends on the predictive-echo engine, its report types, and the shared
//! frame/typed fixtures in `predictive_echo_support`. The display gate lives in
//! `predictive_echo_gate.rs` and the confidence gate in
//! `predictive_echo_confidence.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod predictive_echo_support;
use predictive_echo_support::{anchored, empty_frame, frame, plain_span, typed};

use roost_client_core::client::predictive_echo::PredictiveEcho;
use roost_client_core::client::predictive_echo::report::ResetReason;
use roost_client_core::store::prefs::PredictMode;

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
