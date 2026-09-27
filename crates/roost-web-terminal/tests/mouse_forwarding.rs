//! Decision matrix for the mouse-report encoder, and the forwarding decision
//! above it. Every case is a gate the browser used to get wrong by encoding SGR
//! unconditionally whenever a pane happened to occupy the alt screen: whether
//! the app asked at all, whether THIS mode reports THIS gesture, which of the
//! two wire formats it asked for, and whether the user asked for native
//! selection instead for one gesture.
//!
//! Test names are v2's, from `apps/web/tests/renderer/terminalMouse.test.ts`
//! and `apps/web/tests/terminalMouseForwarding.dom.test.ts`.

use roost_protocol::cell::MouseTracking;
use roost_web_terminal::mouse_forward::{
    MOUSE_FORWARD_DEFAULT, MouseButton, MouseForwarding, MouseGestureKind, MouseModifiers,
    MouseReport, MouseReportEncoding, MouseReportModes, NativeGesture, NativeScrollIntent,
    ScrollExtent, WheelDirection, forwarded_mouse_report, mouse_button_from_dom,
    mouse_gestures_forwarded, native_scroll_intent, should_forward, touch_travel_notches,
};
use roost_web_terminal::reader_intent::ReaderIntentReason;

const fn modes(tracking: MouseTracking, sgr: bool) -> MouseReportModes {
    MouseReportModes { tracking, sgr }
}

const SGR: MouseReportModes = modes(MouseTracking::ButtonMotion, true);
const SGR_CLICK: MouseReportModes = modes(MouseTracking::PressRelease, true);
const X10: MouseReportModes = modes(MouseTracking::ButtonMotion, false);
const OFF: MouseReportModes = modes(MouseTracking::None, true);
const NONE_MODIFIERS: MouseModifiers = MouseModifiers {
    shift: false,
    alt: false,
    ctrl: false,
    meta: false,
};

const fn press(button: MouseButton, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Press { button }, col, row)
}

const fn release(button: MouseButton, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Release { button }, col, row)
}

const fn motion(button: MouseButton, held: bool, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Motion { button, held }, col, row)
}

const fn wheel(direction: WheelDirection, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Wheel(direction), col, row)
}

fn with(report: MouseReport, shift: bool, alt: bool, ctrl: bool, meta: bool) -> MouseReport {
    MouseReport {
        modifiers: MouseModifiers {
            shift,
            alt,
            ctrl,
            meta,
        },
        ..report
    }
}

/// The report as text, for the SGR format where every byte is printable ASCII.
fn sgr_text(frame: MouseReportModes, report: &MouseReport) -> Option<String> {
    let encoded = forwarded_mouse_report(frame, report)?;
    Some(String::from_utf8(encoded.as_bytes().to_vec()).expect("SGR is ascii"))
}

/// The report's exact bytes, for the X10 format where cells past 191 are not.
fn x10_bytes(report: &MouseReport) -> Vec<u8> {
    forwarded_mouse_report(X10, report)
        .expect("mode 1002 reports every press, motion and release")
        .as_bytes()
        .to_vec()
}

/// A pane with 200 px of scrollback, at the given offset.
const fn pane(scroll_top: f64) -> ScrollExtent {
    ScrollExtent {
        scroll_top,
        scroll_height: 300.0,
        client_height: 100.0,
    }
}

/// Reader intent for a native gesture that has NOT already been recorded as a
/// real scroll, which is the case every clamped-edge assertion is about.
fn intent(gesture: NativeGesture, extent: ScrollExtent, scroll_delta: f64) -> NativeScrollIntent {
    native_scroll_intent(gesture, extent, scroll_delta, false)
}

#[test]
fn an_app_that_never_requested_tracking_gets_nothing_whatever_the_gesture() {
    for report in [
        press(MouseButton::Left, 4, 2),
        release(MouseButton::Left, 4, 2),
        motion(MouseButton::Left, true, 4, 2),
        wheel(WheelDirection::Up, 4, 2),
        wheel(WheelDirection::Down, 4, 2),
    ] {
        assert_eq!(forwarded_mouse_report(OFF, &report), None);
    }
    let off_x10 = modes(MouseTracking::None, false);
    assert_eq!(
        should_forward(off_x10, &press(MouseButton::Left, 1, 1)),
        None
    );
}

#[test]
fn mode_1000_reports_press_and_release_but_never_motion() {
    let down = press(MouseButton::Left, 7, 3);
    assert_eq!(sgr_text(SGR_CLICK, &down).as_deref(), Some("\x1b[<0;7;3M"));
    let up = release(MouseButton::Left, 7, 3);
    assert_eq!(sgr_text(SGR_CLICK, &up).as_deref(), Some("\x1b[<0;7;3m"));
    assert_eq!(
        forwarded_mouse_report(SGR_CLICK, &motion(MouseButton::Left, true, 8, 3)),
        None
    );
}

#[test]
fn mode_1002_reports_motion_only_while_a_button_is_held() {
    let left = motion(MouseButton::Left, true, 8, 3);
    assert_eq!(sgr_text(SGR, &left).as_deref(), Some("\x1b[<32;8;3M"));
    let right = motion(MouseButton::Right, true, 8, 3);
    assert_eq!(sgr_text(SGR, &right).as_deref(), Some("\x1b[<34;8;3M"));
    // A hover: mode 1003 (any-motion) is folded to 0 by the core, so an unheld
    // move is never reportable in any mode Roost can see.
    assert_eq!(
        forwarded_mouse_report(SGR, &motion(MouseButton::Left, false, 8, 3)),
        None
    );
}

#[test]
fn buttons_and_modifier_bits_land_in_cb_and_shift_alt_keep_the_gesture_native() {
    assert_eq!(
        sgr_text(SGR, &press(MouseButton::Right, 3, 4)).as_deref(),
        Some("\x1b[<2;3;4M")
    );
    for (ctrl, meta, expected) in [
        (false, true, "\x1b[<8;1;1M"),
        (true, false, "\x1b[<16;1;1M"),
        (true, true, "\x1b[<24;1;1M"),
    ] {
        let report = with(press(MouseButton::Left, 1, 1), false, false, ctrl, meta);
        assert_eq!(sgr_text(SGR, &report).as_deref(), Some(expected));
    }
    // Shift/Alt are Roost's bypass, so they neither forward nor set bit 4.
    for (shift, alt) in [(true, false), (false, true)] {
        for report in [
            press(MouseButton::Left, 1, 1),
            wheel(WheelDirection::Up, 1, 1),
        ] {
            assert_eq!(
                forwarded_mouse_report(SGR, &with(report, shift, alt, false, false)),
                None
            );
        }
    }
    // Mid-drag the app already owns the button: a modifier pressed after the
    // press must not strand it holding a button it never sees released.
    let held = motion(MouseButton::Left, true, 2, 2);
    assert_eq!(
        sgr_text(SGR, &with(held, true, false, false, false)).as_deref(),
        Some("\x1b[<32;2;2M")
    );
    let let_go = release(MouseButton::Left, 2, 2);
    assert_eq!(
        sgr_text(SGR, &with(let_go, false, true, false, false)).as_deref(),
        Some("\x1b[<0;2;2m")
    );
}

#[test]
fn wheel_notches_are_buttons_64_and_65_in_both_encodings() {
    assert_eq!(
        sgr_text(SGR, &wheel(WheelDirection::Up, 1, 1)).as_deref(),
        Some("\x1b[<64;1;1M")
    );
    assert_eq!(
        sgr_text(SGR, &wheel(WheelDirection::Down, 1, 1)).as_deref(),
        Some("\x1b[<65;1;1M")
    );
    let notch = wheel(WheelDirection::Down, 2, 5);
    assert_eq!(
        sgr_text(SGR_CLICK, &notch).as_deref(),
        Some("\x1b[<65;2;5M")
    );
    assert_eq!(
        x10_bytes(&wheel(WheelDirection::Up, 1, 1)),
        [0x1b, 0x5b, 0x4d, 96, 33, 33]
    );
    assert_eq!(
        x10_bytes(&wheel(WheelDirection::Down, 1, 1)),
        [0x1b, 0x5b, 0x4d, 97, 33, 33]
    );
}

#[test]
fn without_decset_1006_the_report_is_legacy_x10_bytes_release_included() {
    assert_eq!(
        x10_bytes(&press(MouseButton::Left, 1, 1)),
        [0x1b, 0x5b, 0x4d, 32, 33, 33]
    );
    assert_eq!(
        x10_bytes(&press(MouseButton::Right, 10, 4)),
        [0x1b, 0x5b, 0x4d, 34, 42, 36]
    );
    assert_eq!(
        x10_bytes(&motion(MouseButton::Left, true, 10, 4)),
        [0x1b, 0x5b, 0x4d, 64, 42, 36]
    );
    // X10 has no per-button release: every release is "all buttons up" (3).
    assert_eq!(
        x10_bytes(&release(MouseButton::Right, 10, 4)),
        [0x1b, 0x5b, 0x4d, 35, 42, 36]
    );
    let ctrl = with(press(MouseButton::Left, 1, 1), false, false, true, false);
    assert_eq!(x10_bytes(&ctrl), [0x1b, 0x5b, 0x4d, 48, 33, 33]);
}

#[test]
fn x10_coordinates_clamp_at_cell_223_while_sgr_stays_exact() {
    // xterm's MOUSE_LIMIT: cell 223 is the last one the biased byte can name,
    // and it names it as 255. Clamping the BYTE at 223 instead would collapse
    // every column past 191 onto 191 — inside the width of an ordinary pane.
    assert_eq!(
        x10_bytes(&press(MouseButton::Left, 191, 200)),
        [0x1b, 0x5b, 0x4d, 32, 223, 232]
    );
    assert_eq!(
        x10_bytes(&press(MouseButton::Left, 223, 223)),
        [0x1b, 0x5b, 0x4d, 32, 255, 255]
    );
    assert_eq!(
        x10_bytes(&press(MouseButton::Left, 400, 300)),
        [0x1b, 0x5b, 0x4d, 32, 255, 255]
    );
    // Every byte stays one byte: a String would UTF-8 expand everything past 0x7f.
    let far = forwarded_mouse_report(X10, &press(MouseButton::Left, 400, 300));
    assert_eq!(far.expect("a press is reported").len(), 6);
    let far_cell = press(MouseButton::Left, 400, 300);
    assert_eq!(
        sgr_text(SGR, &far_cell).as_deref(),
        Some("\x1b[<0;400;300M")
    );
}

#[test]
fn buttons_with_no_mouse_report_encoding_are_left_alone() {
    // Back/forward (DOM 3/4) have no Cb in either format, so they never become a
    // MouseButton and the encoder is never asked to name one. Middle does have a
    // Cb, and is withheld at the call site for the deck's bring-to-front gesture.
    assert_eq!(mouse_button_from_dom(3), None);
    assert_eq!(mouse_button_from_dom(4), None);
    assert_eq!(mouse_button_from_dom(-1), None);
    assert_eq!(mouse_button_from_dom(1), Some(MouseButton::Middle));
    assert_eq!(
        sgr_text(SGR, &press(MouseButton::Middle, 1, 1)).as_deref(),
        Some("\x1b[<1;1;1M")
    );
}

#[test]
fn the_mouse_forward_default_is_on_and_is_the_default_the_store_loads() {
    // v2 read `localStorage.getItem("roostMouseForward") !== "0"`, so an absent
    // key meant ON. The product reason is named in `MOUSE_FORWARD_DEFAULT`: the
    // gate is the application's mode, so an opt-in would cost every mouse-aware
    // TUI its mouse and buy nothing.
    assert!(MOUSE_FORWARD_DEFAULT);
    let stored_default = roost_client_core::store::prefs::Prefs::default().mouse_forward;
    assert_eq!(stored_default, MOUSE_FORWARD_DEFAULT);
    assert!(mouse_gestures_forwarded(
        MOUSE_FORWARD_DEFAULT,
        MouseTracking::PressRelease
    ));
    assert!(mouse_gestures_forwarded(
        MOUSE_FORWARD_DEFAULT,
        MouseTracking::ButtonMotion
    ));
    // Alt-screen occupancy is NOT the question: an app that never asked keeps
    // the browser's own selection and scroll.
    assert!(!mouse_gestures_forwarded(
        MOUSE_FORWARD_DEFAULT,
        MouseTracking::None
    ));
    assert!(!mouse_gestures_forwarded(
        false,
        MouseTracking::ButtonMotion
    ));
}

// ── native wheel and touch reader intent ──────────────────────────────────

/// A pane with no overflow at all: nothing a gesture could scroll.
const NO_OVERFLOW: ScrollExtent = ScrollExtent {
    scroll_top: 0.0,
    scroll_height: 100.0,
    client_height: 100.0,
};

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
