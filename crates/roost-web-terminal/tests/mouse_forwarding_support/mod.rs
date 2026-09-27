//! Shared fixtures for the two mouse-forwarding suites: the report builders,
//! the two wire encoders, and the pane extents a reader gesture is judged
//! against.
//!
//! The encoder and the forwarding decision are separate suites because they are
//! separate questions — "which bytes does this report become" and "should this
//! gesture reach the shell at all" — and a shared file that held both would
//! grow past the cap for the sake of a handful of constants. Ported from
//! `apps/web/tests/terminalMouse.test.ts`.
#![allow(dead_code)]

use roost_protocol::cell::MouseTracking;
use roost_web_terminal::mouse_forward::{
    MouseButton, MouseGestureKind, MouseModifiers, MouseReport, MouseReportModes, NativeGesture,
    NativeScrollIntent, ScrollExtent, WheelDirection, forwarded_mouse_report, native_scroll_intent,
};

pub const fn modes(tracking: MouseTracking, sgr: bool) -> MouseReportModes {
    MouseReportModes { tracking, sgr }
}

pub const SGR: MouseReportModes = modes(MouseTracking::ButtonMotion, true);
pub const SGR_CLICK: MouseReportModes = modes(MouseTracking::PressRelease, true);
pub const X10: MouseReportModes = modes(MouseTracking::ButtonMotion, false);
pub const OFF: MouseReportModes = modes(MouseTracking::None, true);
pub const NONE_MODIFIERS: MouseModifiers = MouseModifiers {
    shift: false,
    alt: false,
    ctrl: false,
    meta: false,
};

pub const fn press(button: MouseButton, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Press { button }, col, row)
}

pub const fn release(button: MouseButton, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Release { button }, col, row)
}

pub const fn motion(button: MouseButton, held: bool, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Motion { button, held }, col, row)
}

pub const fn wheel(direction: WheelDirection, col: u32, row: u32) -> MouseReport {
    MouseReport::at(MouseGestureKind::Wheel(direction), col, row)
}

pub fn with(report: MouseReport, shift: bool, alt: bool, ctrl: bool, meta: bool) -> MouseReport {
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
pub fn sgr_text(frame: MouseReportModes, report: &MouseReport) -> Option<String> {
    let encoded = forwarded_mouse_report(frame, report)?;
    Some(String::from_utf8(encoded.as_bytes().to_vec()).expect("SGR is ascii"))
}

/// The report's exact bytes, for the X10 format where cells past 191 are not.
pub fn x10_bytes(report: &MouseReport) -> Vec<u8> {
    forwarded_mouse_report(X10, report)
        .expect("mode 1002 reports every press, motion and release")
        .as_bytes()
        .to_vec()
}

/// A pane with 200 px of scrollback, at the given offset.
pub const fn pane(scroll_top: f64) -> ScrollExtent {
    ScrollExtent {
        scroll_top,
        scroll_height: 300.0,
        client_height: 100.0,
    }
}

/// Reader intent for a native gesture that has NOT already been recorded as a
/// real scroll, which is the case every clamped-edge assertion is about.
pub fn intent(
    gesture: NativeGesture,
    extent: ScrollExtent,
    scroll_delta: f64,
) -> NativeScrollIntent {
    native_scroll_intent(gesture, extent, scroll_delta, false)
}

pub const NO_OVERFLOW: ScrollExtent = ScrollExtent {
    scroll_top: 0.0,
    scroll_height: 100.0,
    client_height: 100.0,
};
