//! Deciding whether one browser pointer or touch gesture reaches the terminal
//! application, and in which wire format. The pane constructs the `dom`
//! adapter; everything else touches no DOM, session or timer, so every rule
//! below is `#[test]`-covered natively.
//!
//! `mouse_forward::report` owns the value type and the two wire formats,
//! `mouse_forward::forwarding` owns the decision, the drag state and the native
//! scroll intent, `mouse_forward::pane` the listener-level state machine, and
//! `mouse_forward::dom` (wasm only) the listeners themselves. The gate is what
//! the FOREGROUND APPLICATION asked for (DECSET 1000/1002, read off the core),
//! never alt-screen occupancy: vim, less and man occupy the alt screen without
//! ever requesting the mouse, and forwarding to them swallowed the click with
//! no native fallback.
//!
//! Depends on `roost-protocol`'s cell frame and on `reader_intent`'s reasons;
//! it re-implements neither. Ports v2's `apps/web/src/renderer/terminalMouse.ts`
//! and `apps/web/src/renderer/terminalMouseForwarding.ts`.

#[cfg(target_arch = "wasm32")]
pub mod dom;
pub mod forwarding;
pub mod pane;
pub mod report;

#[cfg(target_arch = "wasm32")]
pub use dom::{MouseForwardingHost, TerminalMouseForwarding};
pub use pane::{GridCell, PaneMouse, PaneMouseModes, TouchOutcome, WheelOutcome, fallback_cell};

pub use forwarding::{
    ForwardedGesture, MOUSE_FORWARD_DEFAULT, MouseForwarding, NativeGesture, NativeScrollIntent,
    ScrollExtent, TOUCH_FALLBACK_CELL_PX, forwarded_mouse_report, mouse_gestures_forwarded,
    native_scroll_intent, should_forward, touch_travel_notches,
};
pub use report::{
    EncodedMouseReport, MAX_MOUSE_REPORT_BYTES, MouseButton, MouseGestureKind, MouseModifiers,
    MouseReport, MouseReportEncoding, MouseReportModes, WheelDirection, encode_mouse_report,
    mouse_button_from_dom,
};
