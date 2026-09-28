//! The pane's listener-level mouse state machine: which presses the terminal
//! keeps for itself, how a drag the application holds is settled when the
//! pane stops listening, and how a forwarded touch carries its sub-cell travel.
//!
//! Test names are v2's, from `apps/web/tests/terminalMouseForwarding.dom.test.ts`
//! and `apps/web/tests/cellTerminalVisibility.test.ts` ("terminal mouse drag
//! across a foreground withdraw"); the rest pin `terminalMouseForwarding.ts`
//! listener rules that suite drives through its fake display.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::cell::MouseTracking;
use roost_web_terminal::links::activation::{
    LinkActivationGesture, LinkModifierKey, withhold_press,
};
use roost_web_terminal::mouse_forward::{
    MouseModifiers, MouseReportModes, PaneMouse, PaneMouseModes, TouchOutcome, WheelOutcome,
    fallback_cell,
};

const FORWARDING: PaneMouseModes = PaneMouseModes {
    forward_active: true,
    report: MouseReportModes {
        tracking: MouseTracking::ButtonMotion,
        sgr: true,
    },
};

const NATIVE: PaneMouseModes = PaneMouseModes {
    forward_active: false,
    ..FORWARDING
};

const NONE: MouseModifiers = MouseModifiers {
    shift: false,
    alt: false,
    ctrl: false,
    meta: false,
};

fn text(report: &[u8]) -> String {
    String::from_utf8(report.to_vec()).expect("SGR is ascii")
}

#[test]
fn armed_and_physical_modifier_terminal_links_bypass_pty_bytes_while_bare_clicks_forward() {
    for key in [LinkModifierKey::Control, LinkModifierKey::Meta] {
        let mut pane = PaneMouse::default();
        let mut down = |modified: bool, armed: bool| {
            let gesture = LinkActivationGesture {
                button: 0,
                ctrl: modified && key == LinkModifierKey::Control,
                meta: modified && key == LinkModifierKey::Meta,
                shift: false,
                alt: false,
            };
            let withheld = || withhold_press(true, &gesture, armed, key, false).is_some();
            pane.mouse_down(FORWARDING, false, withheld, 0, NONE, || (1, 1))
                .is_some()
        };
        let physical = down(true, false);
        assert!(!physical, "the platform link gesture is the terminal's");
        let bare = down(false, false);
        assert!(bare, "a bare click on a link reaches a mouse-aware TUI");
        let armed = down(false, true);
        assert!(!armed, "compact arming is the link gesture too");
        let disarmed = down(false, false);
        assert!(disarmed);
        let sent = [physical, bare, armed, disarmed].iter().filter(|pressed| **pressed).count();
        assert_eq!(sent, 2, "only the two bare clicks reached the PTY");
    }
}

#[test]
fn the_middle_button_and_an_already_handled_press_are_never_forwarded() {
    let mut pane = PaneMouse::default();
    let middle = LinkActivationGesture {
        button: 1,
        ..LinkActivationGesture::default()
    };
    let deck = || withhold_press(false, &middle, false, LinkModifierKey::Control, true).is_some();
    assert_eq!(pane.mouse_down(FORWARDING, false, deck, 1, NONE, || (1, 1)), None);
    assert_eq!(pane.mouse_down(FORWARDING, true, || false, 0, NONE, || (1, 1)), None);
    assert_eq!(pane.mouse_down(NATIVE, false, || false, 0, NONE, || (1, 1)), None);
    assert!(!pane.is_dragging());
}

#[test]
fn an_interrupted_drag_is_released_exactly_once_and_never_resumes_as_a_phantom() {
    let mut pane = PaneMouse::default();
    let mut reports: Vec<String> = Vec::new();
    let press = pane
        .mouse_down(FORWARDING, false, || false, 0, NONE, || (3, 2))
        .expect("mode 1002 reports the press");
    reports.push(text(press.as_bytes()));
    let moved = pane.mouse_move(FORWARDING, false, NONE, || (5, 2));
    reports.push(text(moved.report.expect("a held drag reports motion").as_bytes()));
    assert_eq!(reports, ["\x1b[<0;3;2M", "\x1b[<32;5;2M"]);
    // The pane withdraws: the window listener that would have sent the release
    // is removed, so the release is settled now, at the last cell reported.
    let settled = pane.complete_held_drag(FORWARDING).expect("the owed release");
    assert_eq!(text(settled.as_bytes()), "\x1b[<0;5;2m");
    // Neither a later motion nor the real mouseup resumes the drag.
    let phantom = pane.mouse_move(FORWARDING, false, NONE, || (9, 2));
    assert!(!phantom.consumed && phantom.report.is_none());
    let late_up = pane.mouse_up(FORWARDING, false, NONE, || (9, 2));
    assert!(!late_up.consumed && late_up.report.is_none());
    assert_eq!(pane.complete_held_drag(FORWARDING), None, "settled exactly once");
}

#[test]
fn a_release_after_forwarding_is_switched_off_ends_the_drag_without_a_report() {
    let mut pane = PaneMouse::default();
    pane.mouse_down(FORWARDING, false, || false, 0, NONE, || (3, 2));
    let motion = pane.mouse_move(NATIVE, false, NONE, || (4, 2));
    assert!(!motion.consumed, "a pane that stopped forwarding lets the browser have it");
    let released = pane.mouse_up(NATIVE, false, NONE, || (4, 2));
    assert!(!released.consumed && released.report.is_none());
    assert!(!pane.is_dragging());
}

#[test]
fn a_wheel_the_page_already_handled_or_with_no_travel_changes_nothing() {
    let pane = PaneMouse::default();
    let never = || -> (u32, u32) { panic!("no cell is needed for an ignored notch") };
    assert_eq!(pane.wheel(FORWARDING, true, 20.0, NONE, never), WheelOutcome::Ignored);
    assert_eq!(pane.wheel(FORWARDING, false, 0.0, NONE, never), WheelOutcome::Ignored);
    assert_eq!(
        pane.wheel(NATIVE, false, -20.0, NONE, never),
        WheelOutcome::NativeScroll { scroll_delta: -20.0 }
    );
    let shift = MouseModifiers { shift: true, ..NONE };
    assert_eq!(
        pane.wheel(FORWARDING, false, -20.0, shift, || (1, 1)),
        WheelOutcome::NativeScroll { scroll_delta: -20.0 },
        "a Shift bypass falls through to the native path"
    );
    match pane.wheel(FORWARDING, false, -20.0, NONE, || (2, 3)) {
        WheelOutcome::Forwarded(report) => assert_eq!(text(report.as_bytes()), "\x1b[<64;2;3M"),
        other => panic!("a tracked wheel is forwarded, got {other:?}"),
    }
}

#[test]
fn a_forwarded_touch_carries_its_sub_cell_travel_into_the_next_move() {
    let mut pane = PaneMouse::default();
    pane.touch_start(true, false, 1, 100.0, || (4, 7));
    // 25 px at a 10 px cell: two notches toward history, 5 px carried.
    match pane.touch_move(FORWARDING, false, 1, 125.0, 10.0) {
        TouchOutcome::Forwarded { report, notches } => {
            assert_eq!(notches, 2);
            assert_eq!(text(report.expect("a wheel notch").as_bytes()), "\x1b[<64;4;7M");
        }
        other => panic!("the application owns this drag, got {other:?}"),
    }
    // 6 px more crosses the next cell only because the 5 px were kept.
    assert!(matches!(
        pane.touch_move(FORWARDING, false, 1, 131.0, 10.0),
        TouchOutcome::Forwarded { notches: 1, .. }
    ));
    pane.touch_end();
    assert_eq!(
        pane.touch_move(FORWARDING, false, 1, 200.0, 10.0),
        TouchOutcome::Ignored,
        "a finished touch reports nothing"
    );
}

#[test]
fn a_native_touch_moves_its_reference_to_the_finger_and_ignores_sub_cell_travel() {
    let mut pane = PaneMouse::default();
    pane.touch_start(false, false, 1, 100.0, || panic!("a native touch needs no cell"));
    assert_eq!(pane.touch_move(FORWARDING, false, 1, 105.0, 10.0), TouchOutcome::Ignored);
    assert_eq!(
        pane.touch_move(FORWARDING, false, 1, 120.0, 10.0),
        TouchOutcome::NativeScroll { scroll_delta: -20.0 },
        "a touch that started native stays native, finger down scrolls toward history"
    );
    assert_eq!(pane.touch_move(FORWARDING, false, 1, 125.0, 10.0), TouchOutcome::Ignored);
    pane.touch_start(true, false, 2, 100.0, || (1, 1));
    assert_eq!(
        pane.touch_move(FORWARDING, false, 2, 150.0, 10.0),
        TouchOutcome::Ignored,
        "a pinch is never a scroll gesture"
    );
}

#[test]
fn a_press_before_the_first_frame_resolves_from_the_viewport_origin() {
    assert_eq!(fallback_cell(10.0, 20.0, 8.0, 16.0, 10.0, 20.0), (1, 1));
    assert_eq!(fallback_cell(10.0, 20.0, 8.0, 16.0, 35.0, 60.0), (4, 3));
    assert_eq!(
        fallback_cell(10.0, 20.0, 8.0, 16.0, 0.0, 0.0),
        (1, 1),
        "left of or above the viewport clamps to the first cell"
    );
    assert_eq!(
        fallback_cell(0.0, 0.0, 8.0, 16.0, 8000.0, 16.0),
        (1001, 2),
        "with no grid known yet there is no upper bound to clamp to"
    );
}
