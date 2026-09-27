//! The forwarding decision above the encoder: whether a gesture reaches the
//! shell at all, and whether a reader's wheel or touch becomes native scroll
//! intent. Every case is a gate the browser used to get wrong, and the one that
//! matters most is the last pair — a clamped gesture belongs to the shell, and
//! an unclamped one belongs to the reader.
//!
//! Test names are v2's, from
//! `apps/web/tests/terminalMouseForwarding.dom.test.ts`.

mod mouse_forwarding_support;

use mouse_forwarding_support::*;
use roost_protocol::cell::MouseTracking;
use roost_web_terminal::mouse_forward::{
    MOUSE_FORWARD_DEFAULT, MouseButton, MouseForwarding, MouseGestureKind, MouseModifiers,
    MouseReport, MouseReportEncoding, MouseReportModes, NativeGesture, NativeScrollIntent,
    ScrollExtent, WheelDirection, forwarded_mouse_report, mouse_button_from_dom,
    mouse_gestures_forwarded, native_scroll_intent, should_forward, touch_travel_notches,
};
use roost_web_terminal::reader_intent::ReaderIntentReason;

#[test]
fn wheel_gestures_with_no_overflow_leave_live_painting_enabled() {
    for scroll_delta in [-20.0, 20.0] {
        let clamped = intent(NativeGesture::Wheel, NO_OVERFLOW, scroll_delta);
        assert_eq!(clamped, NativeScrollIntent::Clamped);
    }
}

#[test]
fn wheel_intent_follows_the_available_direction_at_both_clamped_edges() {
    // Wheel up is clamped at the top; wheel down can move toward the live tail.
    assert_eq!(
        intent(NativeGesture::Wheel, pane(0.0), -20.0),
        NativeScrollIntent::Clamped
    );
    assert_eq!(
        intent(NativeGesture::Wheel, pane(0.0), 20.0),
        NativeScrollIntent::Park(ReaderIntentReason::Wheel)
    );
    // Wheel down is clamped at the bottom; wheel up can move into history.
    assert_eq!(
        intent(NativeGesture::Wheel, pane(200.0), 20.0),
        NativeScrollIntent::Clamped
    );
    assert_eq!(
        intent(NativeGesture::Wheel, pane(200.0), -20.0),
        NativeScrollIntent::Park(ReaderIntentReason::Wheel)
    );
}

#[test]
fn touch_gestures_with_no_overflow_leave_live_painting_enabled() {
    // A finger moving down scrolls toward history and one moving up toward the
    // live tail, so the sign is inverted before it means anything here.
    for travel in [20.0, -20.0] {
        assert_eq!(
            intent(NativeGesture::Touch, NO_OVERFLOW, -travel),
            NativeScrollIntent::Clamped
        );
    }
}

#[test]
fn touch_intent_follows_the_available_direction_at_both_clamped_edges() {
    // A finger down is clamped at the top; a finger up reaches the live tail.
    assert_eq!(
        intent(NativeGesture::Touch, pane(0.0), -20.0),
        NativeScrollIntent::Clamped
    );
    assert_eq!(
        intent(NativeGesture::Touch, pane(0.0), 20.0),
        NativeScrollIntent::Park(ReaderIntentReason::Touch)
    );
    // A finger up is clamped at the bottom; a finger down moves into history.
    assert_eq!(
        intent(NativeGesture::Touch, pane(200.0), 20.0),
        NativeScrollIntent::Clamped
    );
    assert_eq!(
        intent(NativeGesture::Touch, pane(200.0), -20.0),
        NativeScrollIntent::Park(ReaderIntentReason::Touch)
    );
}

#[test]
fn application_forwarding_still_owns_clamped_wheel_and_touch_gestures() {
    // A tracked application is sent the notch and the browser's scroll is
    // stopped, so the reader is never parked at all — which is why no reader
    // intent is consulted for a gesture forwarding owns.
    let notch = wheel(WheelDirection::Down, 1, 1);
    assert_eq!(
        should_forward(SGR, &notch),
        Some(MouseReportEncoding::Sgr1006)
    );
    assert_eq!(sgr_text(SGR, &notch).as_deref(), Some("\x1b[<65;1;1M"));
    // The touch side of the same gesture, and its one-cell-height threshold.
    assert_eq!(
        touch_travel_notches(10.0, -9.0),
        None,
        "a sub-cell tap changes no state"
    );
    let (notches, direction) = touch_travel_notches(10.0, -20.0).expect("two cells of travel");
    assert_eq!(notches, 2);
    assert_eq!(
        sgr_text(SGR, &wheel(direction, 1, 1)).as_deref(),
        Some("\x1b[<65;1;1M")
    );
    // And the pane those notches are on has no overflow at all, so neither
    // direction could have scrolled it: forwarding owns the gesture outright.
    for gesture in [NativeGesture::Wheel, NativeGesture::Touch] {
        assert_eq!(
            intent(gesture, NO_OVERFLOW, 20.0),
            NativeScrollIntent::Clamped
        );
    }
}

#[test]
fn upgrades_a_passive_native_scroll_fallback_to_wheel_intent() {
    // Chromium can scroll a passive wheel to the top before the listener runs.
    // The recorded native scroll proves that the same gesture really moved.
    assert_eq!(
        native_scroll_intent(NativeGesture::Wheel, pane(0.0), -1200.0, true),
        NativeScrollIntent::Park(ReaderIntentReason::Wheel)
    );
}

#[test]
fn a_native_wheel_bypass_is_guarded_by_the_same_scroll_feasibility() {
    // Shift keeps a wheel native, so it falls through to the scroll path — which
    // is still guarded by whether the gesture could move the display.
    let bypass = with(wheel(WheelDirection::Up, 1, 1), true, false, false, false);
    assert_eq!(forwarded_mouse_report(SGR, &bypass), None);
    assert_eq!(
        intent(NativeGesture::Wheel, pane(0.0), -20.0),
        NativeScrollIntent::Clamped
    );
    assert_eq!(
        intent(NativeGesture::Wheel, pane(50.0), -20.0),
        NativeScrollIntent::Park(ReaderIntentReason::Wheel)
    );
}

// ── the drag the application is holding ───────────────────────────────────

#[test]
fn a_forwarded_drag_is_completed_at_the_cell_it_last_saw() {
    let mut forwarding = MouseForwarding::default();
    assert!(!forwarding.is_dragging());
    let sent = forwarding.press(SGR, MouseButton::Left, (4, 2), NONE_MODIFIERS);
    assert_eq!(
        sent.expect("mode 1002 reports a press").as_bytes(),
        b"\x1b[<0;4;2M"
    );
    assert_eq!(forwarding.pressed_button(), Some(MouseButton::Left));
    let moved = forwarding.motion(SGR, (5, 3), NONE_MODIFIERS);
    assert!(moved.consumed);
    assert!(moved.report.is_some());
    // The same cell reports nothing, so a held-still pointer does not flood the
    // PTY — but the browser is still stopped.
    let still = forwarding.motion(SGR, (5, 3), NONE_MODIFIERS);
    assert!(still.consumed);
    assert_eq!(still.report, None);
    // The pane is about to drop the window listener that would have sent the
    // release, so the drag is completed at the last cell the app was told about.
    let settled = forwarding.complete_held_drag(SGR);
    assert_eq!(
        settled.expect("an owed release").as_bytes(),
        b"\x1b[<0;5;3m"
    );
    assert!(!forwarding.is_dragging());
    assert_eq!(forwarding.complete_held_drag(SGR), None);
}

#[test]
fn a_mode_1000_drag_is_consumed_by_the_browser_but_reports_no_motion() {
    let mut forwarding = MouseForwarding::default();
    forwarding.press(SGR_CLICK, MouseButton::Left, (4, 2), NONE_MODIFIERS);
    let moved = forwarding.motion(SGR_CLICK, (5, 3), NONE_MODIFIERS);
    assert!(moved.consumed);
    assert_eq!(moved.report, None);
    let released = forwarding.release(SGR_CLICK, (5, 3), NONE_MODIFIERS);
    assert!(released.consumed);
    assert!(
        released
            .report
            .is_some_and(|report| report.as_bytes() == b"\x1b[<0;5;3m")
    );
    // A release with no press outstanding is the browser's own.
    let idle = forwarding.release(SGR_CLICK, (5, 3), NONE_MODIFIERS);
    assert!(!idle.consumed);
    assert_eq!(idle.report, None);
    assert_eq!(forwarding.pressed_button(), None);
}
