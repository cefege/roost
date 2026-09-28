//! Pins the press/repeat contract a controller depends on: a button fires once
//! on press, a direction auto-repeats only after the initial delay, a discrete
//! command never repeats while held, and a stick inside the deadzone fires
//! nothing — the whole difference between "one nudge moves one row" and "one
//! nudge scrolls the pane away". Also pins the held-state publication the live
//! controller map highlights from, and when the poll loop runs at all.
//! Ports `apps/web/tests/browser/gamepadSource.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::input_nav::ModeChoice;
use roost_web::input_nav::gamepad_source::{PadPollGate, PollTransition};
use roost_web::input_nav::pad_bindings::PadAction;
use roost_web::input_nav::pad_mapper::{
    HeldPublisher, PAD_REPEAT_DELAY_MS, PAD_REPEAT_INTERVAL_MS, PadHoldState, PadSnapshot,
};

const BUTTON_DOWN: usize = 13;
const BUTTON_A: usize = 0;
const LEFT_STICK_Y: usize = 1;

fn pad_snapshot(pressed: &[usize], axes: [f64; 4], button_count: usize) -> PadSnapshot {
    PadSnapshot {
        buttons: (0..button_count)
            .map(|index| pressed.contains(&index))
            .collect(),
        axes: axes.to_vec(),
    }
}

fn pressed(buttons: &[usize]) -> PadSnapshot {
    pad_snapshot(buttons, [0.0; 4], 16)
}

#[test]
fn a_held_direction_fires_once_then_repeats_after_the_delay() {
    let mut holds = PadHoldState::new();
    let down = pressed(&[BUTTON_DOWN]);

    assert_eq!(holds.actions_to_fire(&down, 0.0), vec![PadAction::MoveDown]);
    assert_eq!(holds.actions_to_fire(&down, 100.0), vec![]);
    assert_eq!(
        holds.actions_to_fire(&down, PAD_REPEAT_DELAY_MS),
        vec![PadAction::MoveDown]
    );
    assert_eq!(
        holds.actions_to_fire(&down, PAD_REPEAT_DELAY_MS + PAD_REPEAT_INTERVAL_MS),
        vec![PadAction::MoveDown]
    );
}

#[test]
fn a_held_discrete_command_fires_exactly_once() {
    let mut holds = PadHoldState::new();
    let activate = pressed(&[BUTTON_A]);

    assert_eq!(
        holds.actions_to_fire(&activate, 0.0),
        vec![PadAction::Activate]
    );
    assert_eq!(
        holds.actions_to_fire(&activate, PAD_REPEAT_DELAY_MS),
        vec![]
    );
    assert_eq!(
        holds.actions_to_fire(&activate, PAD_REPEAT_DELAY_MS * 10.0),
        vec![]
    );
}

#[test]
fn a_stick_inside_the_deadzone_produces_nothing() {
    let mut holds = PadHoldState::new();
    let mut axes = [0.0; 4];

    axes[LEFT_STICK_Y] = -0.3;
    assert_eq!(
        holds.actions_to_fire(&pad_snapshot(&[], axes, 16), 0.0),
        vec![]
    );

    axes[LEFT_STICK_Y] = -0.4;
    assert_eq!(
        holds.actions_to_fire(&pad_snapshot(&[], axes, 16), 0.0),
        vec![PadAction::MoveUp]
    );
}

#[test]
fn releasing_clears_the_hold_so_the_next_press_fires_immediately() {
    let mut holds = PadHoldState::new();
    let down = pressed(&[BUTTON_DOWN]);

    assert_eq!(holds.actions_to_fire(&down, 0.0), vec![PadAction::MoveDown]);
    assert_eq!(holds.actions_to_fire(&pressed(&[]), 10.0), vec![]);
    assert!(holds.is_empty());
    assert_eq!(
        holds.actions_to_fire(&down, 20.0),
        vec![PadAction::MoveDown]
    );
}

#[test]
fn the_same_intent_from_d_pad_and_stick_fires_once() {
    let mut holds = PadHoldState::new();
    let mut axes = [0.0; 4];
    axes[LEFT_STICK_Y] = 1.0;

    assert_eq!(
        holds.actions_to_fire(&pad_snapshot(&[BUTTON_DOWN], axes, 16), 0.0),
        vec![PadAction::MoveDown]
    );
}

#[test]
fn held_publication_includes_unbound_indices() {
    // Index 16 is standard mapping's optional guide button and this build binds
    // nothing to it: the map still lights it, because seeing a button register
    // is how a user learns their pad reports it at all.
    let mut publisher = HeldPublisher::new();
    let held = publisher
        .publish(Some(&pad_snapshot(&[10, 11, 16], [0.0; 4], 17)))
        .unwrap();

    assert_eq!(held.buttons, vec![10, 11, 16]);
    assert_eq!(
        held.actions,
        vec![PadAction::MicToggle, PadAction::FolderNext]
    );
}

#[test]
fn held_state_is_republished_only_when_membership_changes() {
    let mut publisher = HeldPublisher::new();
    assert!(publisher.publish(Some(&pressed(&[13]))).is_some());

    // Same membership from a fresh snapshot: a republish here would re-render
    // the whole controller map every frame the button is down.
    assert_eq!(publisher.publish(Some(&pressed(&[13]))), None);

    let held = publisher.publish(Some(&pressed(&[13, 0]))).unwrap();
    assert_eq!(held.buttons, vec![0, 13]);
}

#[test]
fn an_axis_past_the_deadzone_publishes_its_intent_and_release_clears_it() {
    let mut publisher = HeldPublisher::new();
    let held = publisher
        .publish(Some(&pad_snapshot(&[], [0.0, 0.0, 0.0, -1.0], 16)))
        .unwrap();
    assert_eq!(held.actions, vec![PadAction::ScrollUp]);

    let released = publisher
        .publish(Some(&pad_snapshot(&[], [0.0, 0.0, 0.0, -0.1], 16)))
        .unwrap();
    assert!(released.actions.is_empty());
}

#[test]
fn a_stopped_poll_loop_clears_the_held_state() {
    let mut publisher = HeldPublisher::new();
    let held = publisher
        .publish(Some(&pad_snapshot(&[0], [-1.0, 0.0, 0.0, 0.0], 16)))
        .unwrap();
    assert_eq!(held.actions.len(), 2);

    let cleared = publisher.publish(None).unwrap();
    assert!(cleared.buttons.is_empty());
    assert!(cleared.actions.is_empty());
}

#[test]
fn the_poll_loop_runs_only_with_a_pad_and_a_mode_that_is_not_off() {
    let mut gate = PadPollGate::new();

    // A desktop without a pad pays nothing, whatever the choice.
    let idle = gate.refresh(0, ModeChoice::On);
    assert_eq!(idle.transition, PollTransition::Keep);
    assert!(!idle.connection_changed);

    // `Auto` polls: the first real press is what latches the mode.
    let connected = gate.refresh(1, ModeChoice::Auto);
    assert_eq!(connected.transition, PollTransition::Start);
    assert!(connected.connection_changed);
    assert_eq!(
        gate.refresh(2, ModeChoice::Auto).transition,
        PollTransition::Keep
    );

    assert_eq!(
        gate.refresh(2, ModeChoice::Off).transition,
        PollTransition::Stop
    );
    assert_eq!(
        gate.refresh(2, ModeChoice::On).transition,
        PollTransition::Start
    );

    let unplugged = gate.refresh(0, ModeChoice::On);
    assert_eq!(unplugged.transition, PollTransition::Stop);
    assert!(unplugged.connection_changed);
    assert!(!gate.is_polling());
}
