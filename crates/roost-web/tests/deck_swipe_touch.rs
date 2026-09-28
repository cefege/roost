//! The compact deck's touch reading: the drawer's edge band and a second
//! finger never start a tab swipe, a vertical lock leaves the gesture to the
//! terminal for its whole life, and the release speed reads only the newest
//! samples. Pins the listener half of
//! `apps/web/src/components/deck/terminal-deck-swipe.ts`.

use roost_web::components::deck::deck_dom::DeckTouch;
use roost_web::components::deck::deck_swipe_touch::{SwipeTouchTracker, TouchStep};

fn start(x: f64, y: f64, touches: u32) -> DeckTouch {
    DeckTouch::Start {
        x,
        y,
        touches,
        at_ms: 0.0,
    }
}

fn moved(x: f64, y: f64, at_ms: f64) -> DeckTouch {
    DeckTouch::Move { x, y, at_ms }
}

#[test]
fn a_horizontal_drag_arms_once_then_tracks_and_is_consumed() {
    let mut tracker = SwipeTouchTracker::default();
    assert_eq!(
        tracker.step(start(200.0, 300.0, 1), true),
        TouchStep::Ignored
    );
    assert_eq!(
        tracker.step(moved(195.0, 301.0, 10.0), true),
        TouchStep::Ignored,
        "under the arm gate"
    );
    let armed = tracker.step(moved(170.0, 302.0, 20.0), true);
    assert_eq!(armed, TouchStep::Armed { delta_x: -30.0 });
    assert!(
        armed.consumes(),
        "the terminal must not also scroll a swipe"
    );
    assert_eq!(
        tracker.step(moved(150.0, 302.0, 30.0), true),
        TouchStep::Tracked { delta_x: -50.0 }
    );
}

#[test]
fn a_touch_in_the_drawer_edge_band_or_with_two_fingers_never_arms() {
    let mut tracker = SwipeTouchTracker::default();
    tracker.step(start(20.0, 300.0, 1), true);
    assert_eq!(
        tracker.step(moved(120.0, 300.0, 10.0), true),
        TouchStep::Ignored,
        "the edge band belongs to the drawer"
    );
    tracker.step(start(200.0, 300.0, 2), true);
    assert_eq!(
        tracker.step(moved(100.0, 300.0, 10.0), true),
        TouchStep::Ignored,
        "a pinch is not a swipe"
    );
    tracker.step(start(200.0, 300.0, 1), false);
    assert_eq!(
        tracker.step(moved(100.0, 300.0, 10.0), false),
        TouchStep::Ignored,
        "a desktop deck does not swipe"
    );
}

#[test]
fn a_vertical_lock_keeps_the_gesture_for_the_terminal_to_the_end() {
    let mut tracker = SwipeTouchTracker::default();
    tracker.step(start(200.0, 300.0, 1), true);
    assert_eq!(
        tracker.step(moved(202.0, 340.0, 10.0), true),
        TouchStep::Ignored
    );
    assert_eq!(
        tracker.step(moved(80.0, 345.0, 20.0), true),
        TouchStep::Ignored,
        "the lock outlives later sideways travel"
    );
    assert_eq!(
        tracker.step(DeckTouch::End { at_ms: 30.0 }, true),
        TouchStep::Ignored,
        "no swipe to release"
    );
}

#[test]
fn the_release_speed_reads_only_the_newest_samples() {
    let mut tracker = SwipeTouchTracker::default();
    tracker.step(start(300.0, 300.0, 1), true);
    tracker.step(moved(290.0, 300.0, 300.0), true);
    tracker.step(moved(285.0, 300.0, 340.0), true);
    tracker.step(moved(270.0, 300.0, 400.0), true);
    tracker.step(moved(230.0, 300.0, 440.0), true);
    let released = tracker.step(DeckTouch::End { at_ms: 450.0 }, true);
    let TouchStep::Released { delta_x, velocity } = released else {
        panic!("an armed drag releases, got {released:?}");
    };
    assert_eq!(
        delta_x, -70.0,
        "travel runs from the touch start to the last move"
    );
    assert!(
        (velocity - -1.0).abs() < 1e-9,
        "a slow start must not dilute a flick: {velocity}"
    );
    assert_eq!(
        tracker.step(DeckTouch::End { at_ms: 460.0 }, true),
        TouchStep::Ignored,
        "one release per drag"
    );
}
