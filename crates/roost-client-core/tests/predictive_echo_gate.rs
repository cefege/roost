//! The SRTT display gate: whether the overlay is painted at all, given the
//! round trip the engine has measured and the mode the store resolved.
//!
//! Depends on the predictive-echo engine, the predictor mode enum, and the
//! shared frame/typed fixtures in `predictive_echo_support`. The confidence
//! gate lives in `predictive_echo_confidence.rs` and the events that abandon a
//! burst in `predictive_echo_reset.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod predictive_echo_support;
use predictive_echo_support::{anchored, frame, typed};

use roost_client_core::client::predictive_echo::PredictiveEcho;
use roost_client_core::store::prefs::PredictMode;
use roost_client_core::store::prefs::predict::parse;

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
fn injected_never_mode_disables_prediction() {
    let mut echo = anchored(PredictMode::Never);
    typed(&mut echo, b"a", 1, 0);
    assert_eq!(echo.debug().total, 0);
    assert_eq!(echo.paint_request(), None);
}
