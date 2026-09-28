//! The listener bodies of pane mouse forwarding: each reads one DOM event into
//! `PaneMouse`, applies the outcome, and — for a native gesture — asks
//! `native_scroll_intent` whether the reader parks. A file split of `dom`,
//! whose `ForwardingShared` these read. Ports the `onMouseDownFwd`,
//! `onWheelForward`, `onTouch*`, `enterReadingForNativeScroll` and `cellOf`
//! bodies of v2's `apps/web/src/renderer/terminalMouseForwarding.ts`.

use wasm_bindgen::JsCast;
use web_sys::{Element, Event, MouseEvent, TouchEvent, WheelEvent};

use super::ForwardingShared;
use crate::cell_geometry::cell_from_point;
use crate::cell_row::TERMINAL_LINK_CLASS;
use crate::element_style::scroll_top_of;
use crate::links::activation::{LinkActivationGesture, withhold_press};
use crate::mouse_forward::forwarding::{
    NativeGesture, NativeScrollIntent, ScrollExtent, native_scroll_intent,
};
use crate::mouse_forward::pane::{GridCell, TouchOutcome, WheelOutcome, fallback_cell};
use crate::mouse_forward::report::{EncodedMouseReport, MouseModifiers};

pub(super) fn on_mouse_down(shared: &ForwardingShared, event: &Event) {
    let Some(event) = event.dyn_ref::<MouseEvent>() else {
        return;
    };
    let modes = shared.host.modes();
    let withheld = || {
        // The exact local link gesture wins over DECSET mouse reporting; a bare
        // anchor click still reaches a mouse-aware TUI.
        let over_link = event
            .target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .and_then(|target| target.closest(&format!("a.{TERMINAL_LINK_CLASS}")).ok().flatten())
            .is_some();
        let gesture = LinkActivationGesture {
            button: event.button(),
            ctrl: event.ctrl_key(),
            meta: event.meta_key(),
            shift: event.shift_key(),
            alt: event.alt_key(),
        };
        let armed = shared.host.link_activation_armed();
        withhold_press(over_link, &gesture, armed, shared.link_modifier, event.button() == 1)
            .is_some()
    };
    let Ok(mut pane) = shared.pane.try_borrow_mut() else {
        return;
    };
    let pressed = pane.mouse_down(
        modes,
        event.default_prevented(),
        withheld,
        event.button(),
        modifiers(event),
        || cell_of(shared, event),
    );
    drop(pane);
    if pressed.is_some() {
        event.prevent_default();
    }
    send(shared, pressed);
}

pub(super) fn on_wheel(shared: &ForwardingShared, event: &Event) {
    let Some(event) = event.dyn_ref::<WheelEvent>() else {
        return;
    };
    let modes = shared.host.modes();
    let Ok(pane) = shared.pane.try_borrow() else {
        return;
    };
    let outcome = pane.wheel(
        modes,
        event.default_prevented(),
        event.delta_y(),
        modifiers(event),
        || cell_of(shared, event),
    );
    drop(pane);
    match outcome {
        WheelOutcome::Ignored => {}
        WheelOutcome::Forwarded(report) => {
            event.prevent_default();
            shared.host.send_bytes(report.as_bytes());
        }
        WheelOutcome::NativeScroll { scroll_delta } => {
            enter_reading_for_native_scroll(shared, NativeGesture::Wheel, scroll_delta);
        }
    }
}

pub(super) fn on_touch_start(shared: &ForwardingShared, event: &Event) {
    let Some(event) = event.dyn_ref::<TouchEvent>() else {
        return;
    };
    let touches = event.touches();
    let first = touches.get(0);
    let client_y = first.as_ref().map_or(0.0, |touch| f64::from(touch.client_y()));
    let forward_active = shared.host.modes().forward_active;
    let Ok(mut pane) = shared.pane.try_borrow_mut() else {
        return;
    };
    pane.touch_start(
        forward_active,
        event.default_prevented(),
        touches.length(),
        client_y,
        || match first.as_ref() {
            Some(touch) => cell_at(shared, f64::from(touch.client_x()), client_y),
            None => (1, 1),
        },
    );
}

pub(super) fn on_touch_move(shared: &ForwardingShared, event: &Event) {
    let Some(event) = event.dyn_ref::<TouchEvent>() else {
        return;
    };
    let touches = event.touches();
    let client_y = touches.get(0).map_or(0.0, |touch| f64::from(touch.client_y()));
    let modes = shared.host.modes();
    let (_, cell_height) = shared.host.cell_size();
    let Ok(mut pane) = shared.pane.try_borrow_mut() else {
        return;
    };
    let outcome = pane.touch_move(
        modes,
        event.default_prevented(),
        touches.length(),
        client_y,
        cell_height,
    );
    drop(pane);
    match outcome {
        TouchOutcome::Ignored => {}
        TouchOutcome::NativeScroll { scroll_delta } => {
            enter_reading_for_native_scroll(shared, NativeGesture::Touch, scroll_delta);
        }
        TouchOutcome::Forwarded { report, notches } => {
            if let Some(report) = report {
                for _ in 0..notches {
                    shared.host.send_bytes(report.as_bytes());
                }
            }
            // Suppress the native scroll only after forwarding.
            event.prevent_default();
        }
    }
}

pub(super) fn on_touch_end(shared: &ForwardingShared, _event: &Event) {
    if let Ok(mut pane) = shared.pane.try_borrow_mut() {
        pane.touch_end();
    }
}

/// Reader intent only precedes a native gesture that can actually move the
/// display: parking at a clamped edge freezes live painting with nothing to
/// scroll.
fn enter_reading_for_native_scroll(
    shared: &ForwardingShared,
    gesture: NativeGesture,
    scroll_delta: f64,
) {
    let display = &shared.display;
    let extent = ScrollExtent {
        scroll_top: scroll_top_of(display),
        scroll_height: f64::from(display.scroll_height()),
        client_height: f64::from(display.client_height()),
    };
    let fallback = shared.host.native_scroll_parked();
    if let NativeScrollIntent::Park(reason) =
        native_scroll_intent(gesture, extent, scroll_delta, fallback)
    {
        shared.host.enter_reading(reason);
    }
}

/// Hit-test against the PAINTED grid, never the scroll container: the history
/// spacer and scrollback sheet sit above `.cell-viewport` inside it, so a
/// container-relative row is off by painted history minus `scrollTop`.
pub(super) fn cell_of(shared: &ForwardingShared, event: &MouseEvent) -> GridCell {
    cell_at(shared, f64::from(event.client_x()), f64::from(event.client_y()))
}

fn cell_at(shared: &ForwardingShared, client_x: f64, client_y: f64) -> GridCell {
    if let Some(geometry) = shared.host.viewport_cell_geometry() {
        return cell_from_point(geometry, client_x, client_y);
    }
    let (mut width, mut height) = shared.host.cell_size();
    if width == 0.0 || height == 0.0 {
        if !shared.host.measure_cell() {
            return (1, 1);
        }
        (width, height) = shared.host.cell_size();
    }
    let (left, top) = shared.host.viewport_origin().unwrap_or_else(|| {
        let rect = shared.display.get_bounding_client_rect();
        (rect.left(), rect.top())
    });
    fallback_cell(left, top, width, height, client_x, client_y)
}

pub(super) fn modifiers(event: &MouseEvent) -> MouseModifiers {
    MouseModifiers {
        shift: event.shift_key(),
        alt: event.alt_key(),
        ctrl: event.ctrl_key(),
        meta: event.meta_key(),
    }
}

pub(super) fn send(shared: &ForwardingShared, report: Option<EncodedMouseReport>) {
    if let Some(report) = report {
        shared.host.send_bytes(report.as_bytes());
    }
}
