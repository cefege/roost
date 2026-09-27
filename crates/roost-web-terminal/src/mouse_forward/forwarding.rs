//! Whether one browser pointer gesture reaches the terminal application, and
//! what the terminal does with the ones that do not.
//!
//! This is the decision half of v2's `terminalMouseForwarding.ts`, and every
//! part of it is a function of data the caller already has: the frame's tracking
//! mode, the pointer's cell, the modifiers the event carried, and the pane's
//! own state. Time, event targets and DOM handles stay with the adapter, so the
//! whole matrix is testable without a pane or a session.
//!
//! Two rules in this area are not about the application at all and are therefore
//! the ones that surprise: a native wheel or touch gesture only parks the reader
//! when it could actually have scrolled the display, and a press that is the
//! terminal's own link gesture is withheld from the application entirely — that
//! second rule is `links::activation::withhold_press`, which a mousedown adapter
//! asks before it calls anything in here.

use roost_protocol::cell::MouseTracking;

use super::report::{
    EncodedMouseReport, MouseButton, MouseGestureKind, MouseModifiers, MouseReport,
    MouseReportEncoding, MouseReportModes, WheelDirection, encode_mouse_report,
};
use crate::reader_intent::ReaderIntentReason;

/// Mouse forwarding is ON unless the user has turned it off.
///
/// A PRODUCT DEFAULT, ported from v2's `mouseForwardPref.ts` and unchanged
/// here. It is on because the gate is precise: forwarding is decided by what the
/// foreground application asked for, so an app that never requested tracking
/// never receives events either way and an opt-in would cost every mouse-aware
/// TUI its mouse while buying nothing. The persisted value is read through
/// `roost_client_core::store::prefs`, which carries this same default, and
/// `MOUSE_FORWARD_KEY` is the one stored key.
pub const MOUSE_FORWARD_DEFAULT: bool = true;

/// Whether pointer and touch gestures belong to the terminal application.
///
/// One predicate, so the gesture handlers and the pane's `touch-action` can
/// never disagree about who owns a drag.
pub fn mouse_gestures_forwarded(mouse_forward_enabled: bool, tracking: MouseTracking) -> bool {
    mouse_forward_enabled && tracking != MouseTracking::None
}

/// Which wire format a gesture is forwarded in, or `None` when the browser
/// keeps it native.
///
/// `None` is the whole native story, and each arm is a distinct way the gesture
/// belongs to the browser rather than the application: the application
/// requested no tracking; this mode does not report this gesture; or Shift/Alt
/// asked for native selection instead.
///
/// Shift/Alt is consulted only for a gesture that STARTS a forwarded
/// interaction. Abandoning an in-flight drag halfway would leave the
/// application holding a button it never sees released, so a modifier pressed
/// mid-drag is reported rather than obeyed.
///
/// A button with no mouse-report encoding is not representable: the
/// `MouseButton` the caller builds with has already refused the back/forward
/// DOM buttons, which is where v2's `button > 2` check now lives.
pub fn should_forward(
    modes: MouseReportModes,
    gesture: &MouseReport,
) -> Option<MouseReportEncoding> {
    if matches!(modes.tracking, MouseTracking::None) {
        return None;
    }
    match gesture.kind {
        MouseGestureKind::Wheel(_) | MouseGestureKind::Press { .. } => {
            if bypasses_to_selection(gesture) {
                return None;
            }
            Some(modes.encoding())
        }
        MouseGestureKind::Release { .. } => Some(modes.encoding()),
        MouseGestureKind::Motion { held, .. } => {
            // 1000 reports presses only; 1002 adds motion, but strictly while a
            // button is held. Any-motion 1003 is folded to 0 by the core, so a
            // hover is never reportable in any mode this build can see.
            if modes.tracking != MouseTracking::ButtonMotion || !held {
                return None;
            }
            Some(modes.encoding())
        }
    }
}

/// The bytes to write to the PTY for a gesture the application is owed, or
/// `None` when it must stay native. The rule and the wire are separate on
/// purpose: the rule is the only gate, and the encoding is total, so the two
/// cannot disagree about which gesture is which.
pub fn forwarded_mouse_report(
    modes: MouseReportModes,
    gesture: &MouseReport,
) -> Option<EncodedMouseReport> {
    let encoding = should_forward(modes, gesture)?;
    Some(encode_mouse_report(encoding, gesture))
}

/// Whether this gesture asked for native selection instead of the application.
const fn bypasses_to_selection(gesture: &MouseReport) -> bool {
    gesture.modifiers.shift || gesture.modifiers.alt
}

impl MouseReportModes {
    /// The wire format the frame asked for.
    const fn encoding(self) -> MouseReportEncoding {
        if self.sgr {
            MouseReportEncoding::Sgr1006
        } else {
            MouseReportEncoding::LegacyX10
        }
    }
}

/// What one forwarded gesture did, so the adapter knows whether to stop the
/// browser as well.
///
/// These are genuinely independent. A mode-1000 drag consumes the gesture —
/// the browser must not start a native selection under a button the application
/// is holding — while sending no bytes at all, because that mode reports no
/// motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardedGesture {
    /// The browser must not also act on this gesture.
    pub consumed: bool,
    /// The bytes the application receives, when this mode reports this gesture.
    pub report: Option<EncodedMouseReport>,
}

impl ForwardedGesture {
    /// The browser keeps the gesture: nothing was forwarded.
    const NATIVE: Self = Self {
        consumed: false,
        report: None,
    };

    /// The browser was stopped and the application was sent nothing.
    const CONSUMED_ONLY: Self = Self {
        consumed: true,
        report: None,
    };
}

/// The press/motion/release state one pane's drag owns.
///
/// A pane has at most one forwarded drag at a time, because the application
/// receives at most one press without a release. The middle button never
/// reaches here: it is withheld before the press is forwarded, because the deck
/// owns it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseForwarding {
    /// The button the application is currently holding.
    pressed: Option<MouseButton>,
    /// The cell the application last saw a motion at. A move that lands on the
    /// same cell reports nothing, so a held-still pointer does not flood the PTY.
    last_motion_cell: Option<Cell>,
}

/// A 1-based grid cell, as the terminal numbers them.
type Cell = (u32, u32);

impl MouseForwarding {
    /// Whether the application is holding a button from this pane.
    pub const fn is_dragging(&self) -> bool {
        self.pressed.is_some()
    }

    /// The button the application is holding.
    pub const fn pressed_button(&self) -> Option<MouseButton> {
        self.pressed
    }

    /// A press at a cell. Bytes are sent only when the application is owed them;
    /// the drag is recorded then and not before.
    pub fn press(
        &mut self,
        modes: MouseReportModes,
        button: MouseButton,
        cell: Cell,
        modifiers: MouseModifiers,
    ) -> Option<EncodedMouseReport> {
        let report = forwarded_mouse_report(
            modes,
            &MouseReport {
                kind: MouseGestureKind::Press { button },
                col: cell.0,
                row: cell.1,
                modifiers,
            },
        )?;
        self.pressed = Some(button);
        self.last_motion_cell = Some(cell);
        Some(report)
    }

    /// A motion, which only means anything while a press is outstanding.
    ///
    /// The application owns this drag — it received the press — so the gesture
    /// is consumed even when this mode reports no motion, and even when the
    /// pointer has not moved to a new cell.
    pub fn motion(
        &mut self,
        modes: MouseReportModes,
        cell: Cell,
        modifiers: MouseModifiers,
    ) -> ForwardedGesture {
        let Some(button) = self.pressed else {
            return ForwardedGesture::NATIVE;
        };
        if self.last_motion_cell == Some(cell) {
            return ForwardedGesture::CONSUMED_ONLY;
        }
        // The cell is recorded before the report is computed, so a drag the
        // application no longer accepts does not re-report every cell it visits.
        self.last_motion_cell = Some(cell);
        ForwardedGesture {
            consumed: true,
            report: forwarded_mouse_report(
                modes,
                &MouseReport {
                    kind: MouseGestureKind::Motion {
                        button,
                        held: true,
                    },
                    col: cell.0,
                    row: cell.1,
                    modifiers,
                },
            ),
        }
    }

    /// A release at a cell, or the state clearing for an unrelated one.
    ///
    /// The gesture is consumed only when a release is actually sent. A pane
    /// whose application has since dropped tracking lets this one reach the
    /// browser, which is v2's behaviour and the reason the state is cleared
    /// first: the drag is over either way.
    pub fn release(
        &mut self,
        modes: MouseReportModes,
        cell: Cell,
        modifiers: MouseModifiers,
    ) -> ForwardedGesture {
        let Some(button) = self.pressed.take() else {
            return ForwardedGesture::NATIVE;
        };
        self.last_motion_cell = None;
        let report = forwarded_mouse_report(
            modes,
            &MouseReport {
                kind: MouseGestureKind::Release { button },
                col: cell.0,
                row: cell.1,
                modifiers,
            },
        );
        ForwardedGesture {
            consumed: report.is_some(),
            report,
        }
    }

    /// Drop an outstanding press without reporting it, for a release whose own
    /// gesture another handler already consumed.
    pub fn forget_press(&mut self) {
        self.pressed = None;
        self.last_motion_cell = None;
    }

    /// Settle a drag the pane is about to stop tracking.
    ///
    /// The application already received the press, so the drag must be COMPLETED
    /// rather than abandoned when the window listeners that would have sent the
    /// release go away — the same rule Shift/Alt states. The last cell it saw is
    /// where its drag ended: no later pointer position was ever reported, so
    /// inventing one would move the application's cursor to a cell the user
    /// never pointed at. No modifiers are sent, because none were ever read for
    /// this release.
    pub fn complete_held_drag(&mut self, modes: MouseReportModes) -> Option<EncodedMouseReport> {
        let button = self.pressed?;
        let cell = self.last_motion_cell?;
        self.forget_press();
        forwarded_mouse_report(
            modes,
            &MouseReport {
                kind: MouseGestureKind::Release { button },
                col: cell.0,
                row: cell.1,
                modifiers: MouseModifiers::default(),
            },
        )
    }
}

/// The cell height a touch notch is measured in before the pane has laid out.
pub const TOUCH_FALLBACK_CELL_PX: f64 = 18.0;

/// Which native gesture established an intent, which is also the reason the
/// reader parks under. The two are distinct reasons so a later scroll is not
/// mistaken for the gesture that started the park.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeGesture {
    /// A wheel notch.
    Wheel,
    /// A touch drag.
    Touch,
}

impl NativeGesture {
    /// The reader-intent reason this gesture parks under.
    pub const fn reason(self) -> ReaderIntentReason {
        match self {
            Self::Wheel => ReaderIntentReason::Wheel,
            Self::Touch => ReaderIntentReason::Touch,
        }
    }
}

/// The scroll container's range, as the browser reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollExtent {
    /// Current scroll offset in CSS pixels.
    pub scroll_top: f64,
    /// Total scrollable height.
    pub scroll_height: f64,
    /// Visible height.
    pub client_height: f64,
}

/// What a native wheel or touch gesture does to reader intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeScrollIntent {
    /// The gesture could move the display, so it parks the reader for its own
    /// reason — which also finishes any selection the reader was dragging.
    Park(ReaderIntentReason),
    /// The gesture is clamped at an edge. It moves nothing, so live painting
    /// must stay enabled: entering reader mode here would freeze a pane whose
    /// output is still arriving.
    Clamped,
}

/// Whether a native gesture establishes reader intent.
///
/// `scroll_delta` is the resulting `scrollTop` direction — negative toward
/// history, positive toward the live tail — because a wheel's own `deltaY` and a
/// finger's travel both need that inversion before they mean anything here.
///
/// `native_scroll_fallback_active` says the scroll handler has ALREADY parked
/// this reader for a real scroll. Chromium can run a passive wheel listener only
/// after its compositor has scrolled the container, so the weaker fallback
/// reason is upgraded to the gesture's explicit one. A genuinely clamped gesture
/// starts live and therefore stays live.
pub fn native_scroll_intent(
    gesture: NativeGesture,
    extent: ScrollExtent,
    scroll_delta: f64,
    native_scroll_fallback_active: bool,
) -> NativeScrollIntent {
    let max_scroll_top = (extent.scroll_height - extent.client_height).max(0.0);
    let can_move = if scroll_delta < 0.0 {
        extent.scroll_top > 0.0
    } else {
        extent.scroll_top < max_scroll_top
    };
    if can_move || native_scroll_fallback_active {
        NativeScrollIntent::Park(gesture.reason())
    } else {
        NativeScrollIntent::Clamped
    }
}

/// The whole wheel notches one touch move reports, or `None` when the finger
/// has not travelled a full cell yet.
///
/// A touch becomes forwarding intent only after one cell-height of vertical
/// travel; below that shared native/forwarding threshold a tap changes no state
/// at all. A finger moving DOWN scrolls toward history, which is the wheel-up
/// notch.
pub fn touch_travel_notches(cell_height: f64, travel: f64) -> Option<(u32, WheelDirection)> {
    let step = if cell_height > 0.0 {
        cell_height
    } else {
        TOUCH_FALLBACK_CELL_PX
    };
    let notches = (travel.abs() / step) as u32;
    if notches == 0 {
        return None;
    }
    let direction = if travel > 0.0 {
        WheelDirection::Up
    } else {
        WheelDirection::Down
    };
    Some((notches, direction))
}
