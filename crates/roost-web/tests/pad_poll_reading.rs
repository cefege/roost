//! Pins what the Gamepad poll is allowed to read and therefore count. A pad
//! entry is plain data — `connected`, a mapping string, a button list, an axis
//! list — and the poll must admit an entry that merely HAS that shape, because
//! the browser is not the only thing that hands `navigator.getGamepads()` an
//! object: a stand-in, a polyfill and a cross-realm wrapper all look the same
//! to a `Reflect` read and all used to be dropped by a brand check. Symptom
//! this pins: "the controller does nothing" with no error anywhere.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::input_nav::ModeChoice;
use roost_web::input_nav::gamepad_source::{PadPollGate, PadReading, PollTransition};
use roost_web::input_nav::pad_bindings::PadAction;
use roost_web::input_nav::pad_mapper::PadHoldState;

const BUTTON_DOWN: usize = 13;

/// The shape `smoke/terminal/fixtures.ts` installs: 16 buttons, 4 axes, the
/// standard mapping string.
fn stand_in_pad() -> PadReading {
    PadReading::from_parts(true, "standard", vec![false; 16], vec![0.0; 4])
}

#[test]
fn a_connected_standard_pad_is_counted_whatever_its_provenance() {
    assert!(stand_in_pad().is_pollable());

    // A disconnected slot and a legacy `""` mapping are still not pollable, so
    // the loosened read does not admit every entry getGamepads() reports.
    assert!(!PadReading::from_parts(false, "standard", vec![], vec![]).is_pollable());
    assert!(!PadReading::from_parts(true, "", vec![], vec![]).is_pollable());
}

#[test]
fn admitting_a_pad_starts_the_poll_loop() {
    let readings = [stand_in_pad()];
    let pollable = readings.iter().filter(|pad| pad.is_pollable()).count();
    assert_eq!(pollable, 1);

    let mut gate = PadPollGate::new();
    assert_eq!(
        gate.refresh(pollable, ModeChoice::On).transition,
        PollTransition::Start
    );
    assert!(gate.is_polling());
}

#[test]
fn a_held_dpad_down_on_a_counted_pad_produces_move_down() {
    let idle = stand_in_pad();
    // A poll that observed every button released first, then the press: the
    // press is a FRESH press, which is the one a rAF poll can miss.
    let mut holds = PadHoldState::new();
    assert!(holds.actions_to_fire(idle.snapshot(), 0.0).is_empty());

    let pressed = PadReading::from_parts(
        true,
        "standard",
        (0..16).map(|index| index == BUTTON_DOWN).collect(),
        vec![0.0; 4],
    );
    assert_eq!(
        holds.actions_to_fire(pressed.snapshot(), 16.0),
        vec![PadAction::MoveDown]
    );
}
