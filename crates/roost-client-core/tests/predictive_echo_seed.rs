//! Seeding the echo RTT from a route's probe: the display gate arms before the
//! first echo is timed, a second seed changes nothing, and a real sample still
//! blends against the seed with the ordinary weight.
//!
//! Depends on the predictive-echo engine and `predictive_echo_support`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod predictive_echo_support;
use predictive_echo_support::{anchored, frame, typed};

use roost_client_core::client::predictive_echo::expiry::RTT_SAMPLE_WEIGHT;
use roost_client_core::store::prefs::PredictMode;

#[test]
fn a_seeded_rtt_arms_the_gate_before_any_echo_is_timed() {
    let mut echo = anchored(PredictMode::Adaptive);
    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 0, false);
    typed(&mut echo, b"b", 2, 0);
    assert_eq!(echo.debug().visible, 0, "unmeasured: nothing is shown");
    assert!(!echo.rtt_measured());

    echo.seed_rtt(40.0);
    assert!(echo.rtt_measured());
    assert_eq!(echo.debug().srtt_ms, 40.0);
    typed(&mut echo, b"c", 3, 1);
    assert!(echo.debug().visible > 0, "a 40 ms route shows the guess");
}

#[test]
fn a_seed_never_replaces_an_estimate_and_a_sample_blends_against_it() {
    let mut echo = anchored(PredictMode::Adaptive);
    echo.seed_rtt(40.0);
    echo.seed_rtt(400.0);
    assert_eq!(echo.debug().srtt_ms, 40.0, "the second seed is ignored");

    typed(&mut echo, b"a", 1, 0);
    echo.on_frame(&frame(2, 0, 1, 0, "a"), 200, false);
    let expected = 40.0 * (1.0 - RTT_SAMPLE_WEIGHT) + 200.0 * RTT_SAMPLE_WEIGHT;
    assert!((echo.debug().srtt_ms - expected).abs() < 1e-9);
}

#[test]
fn a_nonpositive_seed_is_ignored() {
    let mut echo = anchored(PredictMode::Adaptive);
    echo.seed_rtt(0.0);
    echo.seed_rtt(-5.0);
    assert!(!echo.rtt_measured());
}
