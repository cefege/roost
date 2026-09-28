//! Two contracts of the predictor that the echo host in `roost-web-terminal`
//! leans on: a hidden epoch refused by one contradiction takes every guess of
//! that epoch with it, including one already judged earlier in the same pass;
//! and only the transitions v2 repaints on ask the overlay for a repaint.
//!
//! Depends on the predictive-echo engine and the shared fixtures in
//! `predictive_echo_support`. v2 semantics: `apps/web/src/renderer/predictiveEcho.ts`
//! (`reconcileAgainst`'s `this.preds.forEach(... other.epoch = -1)`, and the
//! `repaint()` call sites).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod predictive_echo_support;
use predictive_echo_support::{anchored, empty_frame, frame, typed};

use roost_client_core::store::prefs::PredictMode;

#[test]
fn a_refused_hidden_epoch_drops_a_guess_already_judged_pending_in_the_same_pass() {
    let mut echo = anchored(PredictMode::Always);
    // "a" at (0,0), then the cursor moves to row 1 without a scroll: the sparse
    // delta that moves it does not carry row 0, so "a" stays pending.
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&empty_frame(2, 1, 0, vec![(1, None)]), 10, false);
    assert_eq!(echo.debug().total, 1);
    // "b" lands at (1,1) in the same, still hidden, epoch.
    typed(&mut echo, b"b", 2, 10);
    // Row 1 alone: "a" is judged pending again (its row is absent) BEFORE "b" is
    // judged contradicted. The contradiction refuses the whole hidden epoch, so
    // the pending "a" goes with it rather than surviving to be painted later.
    echo.on_frame(&frame(3, 1, 2, 1, "zx"), 100, false);
    let debug = echo.debug();
    assert_eq!(debug.total, 0, "no guess of a refused epoch survives");
    assert_eq!(debug.confirmed_epoch, 0);
    assert_eq!(debug.prediction_epoch, 2, "the refusal re-arms the gate");
    assert_eq!(debug.reset_count, 0, "a hidden refusal is not a hard reset");
}

#[test]
fn an_acknowledgement_alone_never_asks_the_overlay_to_repaint() {
    let mut echo = anchored(PredictMode::Always);
    assert!(echo.take_repaint(), "the anchoring frame reconciled");
    assert!(!echo.take_repaint(), "a repaint request is taken once");
    echo.predict(b"a", 1, 0);
    assert!(echo.take_repaint(), "a keystroke repaints");
    echo.note_input_written(1, 5);
    assert!(!echo.take_repaint(), "an ack changes nothing painted");
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 20, false);
    assert!(echo.take_repaint(), "a reconcile repaints");
}

#[test]
fn a_keystroke_refused_with_nothing_predicted_costs_no_repaint() {
    let mut echo = anchored(PredictMode::Never);
    assert!(echo.take_repaint());
    echo.predict(b"a", 1, 0);
    assert!(!echo.take_repaint(), "never mode with nothing to clear is silent");
    echo.set_mode(PredictMode::Adaptive);
    assert!(
        echo.take_repaint(),
        "a preference change repaints even while idle"
    );
    echo.set_mode(PredictMode::Never);
    assert!(echo.take_repaint(), "switching prediction off clears the overlay");
    assert_eq!(
        echo.debug().last_reset.map(|reason| reason.as_str()),
        Some("preference")
    );
}
