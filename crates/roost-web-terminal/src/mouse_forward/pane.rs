//! One pane's pointer, wheel and touch listeners, minus the DOM: which gestures
//! reach the application, which stay native and park the reader, and the
//! press/touch state between events. `forwarding` owns the per-gesture rule and
//! the drag; this adds the listener-level gates (an already-prevented event, the
//! pane's forwarding switch, the touch threshold and its carried remainder).
//! `dom` reads events into it. Ports the listener state machine of v2's
//! `apps/web/src/renderer/terminalMouseForwarding.ts`.

use super::forwarding::{
    ForwardedGesture, MouseForwarding, TOUCH_FALLBACK_CELL_PX, forwarded_mouse_report,
    touch_travel_notches,
};
use super::report::{
    EncodedMouseReport, MouseGestureKind, MouseModifiers, MouseReport, MouseReportModes,
    WheelDirection, mouse_button_from_dom,
};

/// A 1-based grid cell, as the terminal numbers them.
pub type GridCell = (u32, u32);

/// What the pane knows about the application at the moment of one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneMouseModes {
    /// `mouse_gestures_forwarded(pref, tracking)`: gestures belong to the
    /// application rather than to native selection and scrolling.
    pub forward_active: bool,
    /// Tracking and encoding off the newest accepted frame.
    pub report: MouseReportModes,
}

/// What a wheel notch does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelOutcome {
    /// Already prevented, or no vertical travel: nothing at all.
    Ignored,
    /// Send the report and prevent the browser's scroll.
    Forwarded(EncodedMouseReport),
    /// Leave it native. `scroll_delta` is the resulting `scrollTop` direction,
    /// which `native_scroll_intent` judges before any reader parks.
    NativeScroll {
        /// Negative toward history, positive toward the live tail.
        scroll_delta: f64,
    },
}

/// What one `touchmove` does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchOutcome {
    /// Below one cell of travel, already prevented, or not a one-finger drag.
    Ignored,
    /// A native drag of at least one cell: judge reader intent for it.
    NativeScroll {
        /// Negative toward history, positive toward the live tail.
        scroll_delta: f64,
    },
    /// The application owns the drag: send `report` once per notch, then
    /// prevent the native scroll. `report` is `None` only when the frame's
    /// tracking no longer reports a wheel; the scroll is still prevented.
    Forwarded {
        /// The one wheel report every notch of this move repeats.
        report: Option<EncodedMouseReport>,
        /// Whole cells of travel.
        notches: u32,
    },
}

/// The press and touch state of one pane.
#[derive(Debug, Default)]
pub struct PaneMouse {
    drag: MouseForwarding,
    /// The finger's reference position, `None` outside a one-finger touch.
    touch_y: Option<f64>,
    /// Whether the touch started while the application owned gestures.
    touch_forwarding: bool,
    /// The cell a forwarded touch reports its notches at.
    touch_cell: GridCell,
}

impl PaneMouse {
    /// Whether the application is holding a button from this pane.
    pub const fn is_dragging(&self) -> bool {
        self.drag.is_dragging()
    }

    /// A wheel notch. `cell_of` is only asked when the notch may be forwarded.
    pub fn wheel(
        &self,
        modes: PaneMouseModes,
        default_prevented: bool,
        delta_y: f64,
        modifiers: MouseModifiers,
        cell_of: impl FnOnce() -> GridCell,
    ) -> WheelOutcome {
        if default_prevented || delta_y == 0.0 {
            return WheelOutcome::Ignored;
        }
        let native = WheelOutcome::NativeScroll {
            scroll_delta: delta_y,
        };
        if !modes.forward_active {
            return native;
        }
        let (col, row) = cell_of();
        let direction = if delta_y < 0.0 {
            WheelDirection::Up
        } else {
            WheelDirection::Down
        };
        let gesture = MouseReport {
            kind: MouseGestureKind::Wheel(direction),
            col,
            row,
            modifiers,
        };
        // A Shift/Alt bypass falls through to the native path, which is still
        // guarded by whether the gesture could move the display at all.
        forwarded_mouse_report(modes.report, &gesture).map_or(native, WheelOutcome::Forwarded)
    }

    /// A press on the pane. `withheld` is the terminal's own claim on the
    /// press — its link gesture, or the deck's middle button — and is asked
    /// only once the application owns gestures. `Some` means send the report
    /// and prevent the default; the drag is recorded then and not before.
    pub fn mouse_down(
        &mut self,
        modes: PaneMouseModes,
        default_prevented: bool,
        withheld: impl FnOnce() -> bool,
        dom_button: i16,
        modifiers: MouseModifiers,
        cell_of: impl FnOnce() -> GridCell,
    ) -> Option<EncodedMouseReport> {
        if default_prevented || !modes.forward_active || withheld() {
            return None;
        }
        let button = mouse_button_from_dom(dom_button)?;
        self.drag.press(modes.report, button, cell_of(), modifiers)
    }

    /// Pointer motion on the window while a press is outstanding.
    pub fn mouse_move(
        &mut self,
        modes: PaneMouseModes,
        default_prevented: bool,
        modifiers: MouseModifiers,
        cell_of: impl FnOnce() -> GridCell,
    ) -> ForwardedGesture {
        if default_prevented || !self.drag.is_dragging() || !modes.forward_active {
            return ForwardedGesture::NATIVE;
        }
        self.drag.motion(modes.report, cell_of(), modifiers)
    }

    /// A button coming up on the window. A release another handler already
    /// consumed, or one arriving after the pane stopped forwarding, ends the
    /// drag without a report.
    pub fn mouse_up(
        &mut self,
        modes: PaneMouseModes,
        default_prevented: bool,
        modifiers: MouseModifiers,
        cell_of: impl FnOnce() -> GridCell,
    ) -> ForwardedGesture {
        if !self.drag.is_dragging() {
            return ForwardedGesture::NATIVE;
        }
        if default_prevented || !modes.forward_active {
            self.drag.forget_press();
            return ForwardedGesture::NATIVE;
        }
        self.drag.release(modes.report, cell_of(), modifiers)
    }

    /// Settle the release an in-flight drag owes the application, for the
    /// transition that removes the window listener which would have sent it.
    pub fn complete_held_drag(&mut self, modes: PaneMouseModes) -> Option<EncodedMouseReport> {
        let button = self.drag.pressed_button()?;
        let report = self.drag.complete_held_drag(modes.report);
        tracing::debug!(
            target: "mouse",
            ?button,
            reported = report.is_some(),
            "mouse.held_drag_completed"
        );
        report
    }

    /// A touch began. Only a one-finger touch is tracked; `cell_of` is asked
    /// only when the application owns the gesture.
    pub fn touch_start(
        &mut self,
        forward_active: bool,
        default_prevented: bool,
        touch_count: u32,
        client_y: f64,
        cell_of: impl FnOnce() -> GridCell,
    ) {
        self.touch_end();
        if default_prevented || touch_count != 1 {
            return;
        }
        self.touch_y = Some(client_y);
        self.touch_forwarding = forward_active;
        if forward_active {
            self.touch_cell = cell_of();
        }
    }

    /// A finger moved. A touch becomes intent only after one cell of vertical
    /// travel, and a forwarded drag carries its sub-notch remainder forward so
    /// a slow drag still reports every cell it crosses.
    pub fn touch_move(
        &mut self,
        modes: PaneMouseModes,
        default_prevented: bool,
        touch_count: u32,
        client_y: f64,
        cell_height: f64,
    ) -> TouchOutcome {
        let Some(start) = self.touch_y else {
            return TouchOutcome::Ignored;
        };
        if default_prevented || touch_count != 1 {
            return TouchOutcome::Ignored;
        }
        let travel = client_y - start;
        let Some((notches, direction)) = touch_travel_notches(cell_height, travel) else {
            return TouchOutcome::Ignored;
        };
        if !self.touch_forwarding || !modes.forward_active {
            self.touch_y = Some(client_y);
            return TouchOutcome::NativeScroll {
                scroll_delta: -travel,
            };
        }
        let step = if cell_height > 0.0 {
            cell_height
        } else {
            TOUCH_FALLBACK_CELL_PX
        };
        let remainder = travel - travel.signum() * f64::from(notches) * step;
        self.touch_y = Some(client_y - remainder);
        let (col, row) = self.touch_cell;
        TouchOutcome::Forwarded {
            report: forwarded_mouse_report(
                modes.report,
                &MouseReport::at(MouseGestureKind::Wheel(direction), col, row),
            ),
            notches,
        }
    }

    /// The touch ended or was cancelled.
    pub fn touch_end(&mut self) {
        self.touch_y = None;
        self.touch_forwarding = false;
    }
}

/// The cell under a point before the renderer can report its painted grid
/// geometry: 1-based from the viewport's origin, clamped only from below,
/// because there is no known grid to clamp to yet.
pub fn fallback_cell(
    origin_left: f64,
    origin_top: f64,
    cell_width: f64,
    cell_height: f64,
    client_x: f64,
    client_y: f64,
) -> GridCell {
    let axis = |position: f64, origin: f64, size: f64| -> u32 {
        let cell = 1.0 + ((position - origin) / size).floor();
        if cell.is_finite() && cell > 1.0 {
            cell as u32
        } else {
            1
        }
    };
    (
        axis(client_x, origin_left, cell_width),
        axis(client_y, origin_top, cell_height),
    )
}
