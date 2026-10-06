//! The compact deck's touch reading: the drawer's edge band and a second
//! finger never start a tab swipe, a vertical lock leaves the gesture to the
//! terminal for its whole life, the release speed reads only the newest
//! samples, and every touch that ends an armed drag without a release
//! cancels it. Pins the listener half of
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

/// An armed drag: down at x 300, then 80px toward the next tab.
fn armed_drag() -> SwipeTouchTracker {
    let mut tracker = SwipeTouchTracker::default();
    tracker.step(start(300.0, 300.0, 1), true);
    tracker.step(moved(260.0, 300.0, 10.0), true);
    tracker.step(moved(220.0, 300.0, 20.0), true);
    tracker
}

/// A second finger landing mid-drag reset the tracker, so the lift that
/// followed read as "nothing armed" and the swipe froze at its last offset.
#[test]
fn a_second_finger_during_an_armed_drag_cancels_it() {
    let mut tracker = armed_drag();
    assert_eq!(
        tracker.step(start(100.0, 400.0, 2), true),
        TouchStep::Cancelled
    );
    assert_eq!(
        tracker.step(moved(150.0, 300.0, 30.0), true),
        TouchStep::Ignored,
        "the pinch does not re-arm"
    );
    assert_eq!(
        tracker.step(DeckTouch::End { at_ms: 40.0 }, true),
        TouchStep::Ignored,
        "one end per drag"
    );
}

#[test]
fn a_cancelled_touch_cancels_the_drag_instead_of_releasing_it() {
    let mut tracker = armed_drag();
    assert_eq!(tracker.step(DeckTouch::Cancel, true), TouchStep::Cancelled);
    assert_eq!(
        tracker.step(DeckTouch::End { at_ms: 40.0 }, true),
        TouchStep::Ignored
    );
    let mut idle = SwipeTouchTracker::default();
    assert_eq!(
        idle.step(DeckTouch::Cancel, true),
        TouchStep::Ignored,
        "nothing armed, nothing to cancel"
    );
}

/// A drag whose release went to a detached target leaves the tracker armed;
/// the next touch cancels that drag and still starts its own.
#[test]
fn a_new_touch_over_a_drag_that_never_released_cancels_it_and_tracks_anew() {
    let mut tracker = armed_drag();
    assert_eq!(
        tracker.step(start(200.0, 300.0, 1), true),
        TouchStep::Cancelled
    );
    assert_eq!(
        tracker.step(moved(170.0, 301.0, 50.0), true),
        TouchStep::Armed { delta_x: -30.0 },
        "the new touch is a swipe of its own"
    );
}
