//! Which bytes one mouse report becomes, and whether it is sent at all. Every
//! case is a gate the browser used to get wrong by encoding SGR unconditionally
//! whenever a pane happened to occupy the alt screen: whether the app asked at
//! all, whether THIS mode reports THIS gesture, and which of the two wire
//! formats it asked for.
//!
//! Test names are v2's, from `apps/web/tests/renderer/terminalMouse.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod mouse_forwarding_support;

use mouse_forwarding_support::*;
use roost_protocol::cell::MouseTracking;
use roost_web_terminal::mouse_forward::{
    MOUSE_FORWARD_DEFAULT, MouseButton, WheelDirection, forwarded_mouse_report,
    mouse_button_from_dom, mouse_gestures_forwarded, should_forward,
};

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
