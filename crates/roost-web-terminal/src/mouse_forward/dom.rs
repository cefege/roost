//! The DOM half of pane mouse forwarding: the listeners, the hit test and the
//! reader-intent side effect of a native gesture. `pane::PaneMouse` decides;
//! this reads events into it and applies the outcome. Listener ownership
//! follows the pane's lifetimes: press and touch listeners for the mount, the
//! drag continuation on window (the pane routes `on_window_mouse_move/up` there
//! only while visible), and the non-passive wheel/touchmove classifiers from
//! `bind_wheel_and_touch_move` until `unbind`/`dispose`.
//! Ports the listener wiring of v2's `apps/web/src/renderer/terminalMouseForwarding.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{AddEventListenerOptions, Element, Event, MouseEvent};

use super::pane::{PaneMouse, PaneMouseModes};
use crate::cell_geometry::TerminalCellGeometry;
use crate::links::activation::LinkModifierKey;
use crate::reader_intent::ReaderIntentReason;

mod events;

use events::{
    cell_of, modifiers, on_mouse_down, on_touch_end, on_touch_move, on_touch_start, on_wheel, send,
};

/// What the pane supplies. Every read is live, per event: tracking, encoding
/// and cell metrics change with each accepted frame and layout.
pub trait MouseForwardingHost {
    /// Whether gestures belong to the application, and its tracking modes.
    fn modes(&self) -> PaneMouseModes;
    /// `CellGridRenderer::viewport_cell_geometry` — the PAINTED grid.
    fn viewport_cell_geometry(&self) -> Option<TerminalCellGeometry>;
    /// The last measured cell box `(width, height)`, zero before layout.
    fn cell_size(&self) -> (f64, f64);
    /// Re-probe the cell box; true when it produced a usable measurement.
    fn measure_cell(&self) -> bool;
    /// Client-space origin of the renderer's `.cell-viewport` (row 0's box),
    /// or `None` before a renderer exists.
    fn viewport_origin(&self) -> Option<(f64, f64)>;
    /// The pane's compact-sheet link arming.
    fn link_activation_armed(&self) -> bool;
    /// Hand an encoded report to the session's input lane.
    fn send_bytes(&self, bytes: &[u8]);
    /// Whether the reader is already parked for `native_scroll`, which a
    /// gesture that reached an edge before its listener ran upgrades.
    fn native_scroll_parked(&self) -> bool;
    /// `finish_live_selection_release` then `enter_reading(reason)`.
    fn enter_reading(&self, reason: ReaderIntentReason);
}

type Listener = Closure<dyn FnMut(Event)>;

struct ForwardingShared {
    display: Element,
    host: Rc<dyn MouseForwardingHost>,
    pane: RefCell<PaneMouse>,
    link_modifier: LinkModifierKey,
}

/// One pane's mouse forwarding.
pub struct TerminalMouseForwarding {
    shared: Rc<ForwardingShared>,
    pane_listeners: RefCell<Vec<(&'static str, Listener)>>,
    wheel_and_touch_move: RefCell<Vec<(&'static str, bool, Listener)>>,
}

impl std::fmt::Debug for TerminalMouseForwarding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalMouseForwarding")
            .finish_non_exhaustive()
    }
}

impl TerminalMouseForwarding {
    /// Attach the pane-local press and touch listeners to `display`, the
    /// pane's scroll container.
    pub fn attach(
        display: &Element,
        host: Rc<dyn MouseForwardingHost>,
        link_modifier: LinkModifierKey,
    ) -> Self {
        let forwarding = Self {
            shared: Rc::new(ForwardingShared {
                display: display.clone(),
                host,
                pane: RefCell::new(PaneMouse::default()),
                link_modifier,
            }),
            pane_listeners: RefCell::new(Vec::new()),
            wheel_and_touch_move: RefCell::new(Vec::new()),
        };
        forwarding.listen_pane("mousedown", None, on_mouse_down);
        forwarding.listen_pane("touchstart", Some(true), on_touch_start);
        forwarding.listen_pane("touchend", Some(true), on_touch_end);
        forwarding.listen_pane("touchcancel", Some(true), on_touch_end);
        forwarding
    }

    /// The window `mousemove` of a drag this pane forwarded.
    pub fn on_window_mouse_move(&self, event: &MouseEvent) {
        let shared = &self.shared;
        let modes = shared.host.modes();
        let Ok(mut pane) = shared.pane.try_borrow_mut() else {
            return;
        };
        let moved = pane.mouse_move(modes, event.default_prevented(), modifiers(event), || {
            cell_of(shared, event)
        });
        drop(pane);
        // The application owns this drag — it received the press — so the
        // browser must not start a native selection under it, even in a mode
        // that reports no motion.
        if moved.consumed {
            event.prevent_default();
        }
        send(shared, moved.report);
    }

    /// The window `mouseup` that ends a forwarded drag.
    pub fn on_window_mouse_up(&self, event: &MouseEvent) {
        let shared = &self.shared;
        let modes = shared.host.modes();
        let Ok(mut pane) = shared.pane.try_borrow_mut() else {
            return;
        };
        let released = pane.mouse_up(modes, event.default_prevented(), modifiers(event), || {
            cell_of(shared, event)
        });
        drop(pane);
        if released.consumed {
            event.prevent_default();
        }
        send(shared, released.report);
    }

    /// Settle the release an in-flight drag owes the application, on the
    /// transition that removes the window listener which would have sent it.
    pub fn complete_held_drag(&self) {
        let modes = self.shared.host.modes();
        let report = match self.shared.pane.try_borrow_mut() {
            Ok(mut pane) => pane.complete_held_drag(modes),
            Err(_) => return,
        };
        send(&self.shared, report);
    }

    /// Attach the non-passive wheel (capture) and touchmove classifiers. They
    /// must run BEFORE the browser scrolls, or explicit wheel/touch reader
    /// intent degrades to the weaker native-scroll fallback. Idempotent.
    pub fn bind_wheel_and_touch_move(&self) {
        let Ok(mut bound) = self.wheel_and_touch_move.try_borrow_mut() else {
            return;
        };
        if !bound.is_empty() {
            return;
        }
        for (kind, capture, react) in [
            ("wheel", true, on_wheel as fn(&ForwardingShared, &Event)),
            ("touchmove", false, on_touch_move),
        ] {
            let listener = self.listener(react);
            let options = AddEventListenerOptions::new();
            options.set_capture(capture);
            options.set_passive(false);
            let _ = self
                .shared
                .display
                .add_event_listener_with_callback_and_add_event_listener_options(
                    kind,
                    listener.as_ref().unchecked_ref(),
                    &options,
                );
            bound.push((kind, capture, listener));
        }
    }

    /// Remove the wheel and touchmove classifiers.
    pub fn unbind_wheel_and_touch_move(&self) {
        let Ok(mut bound) = self.wheel_and_touch_move.try_borrow_mut() else {
            return;
        };
        for (kind, capture, listener) in bound.drain(..) {
            let _ = self
                .shared
                .display
                .remove_event_listener_with_callback_and_bool(
                    kind,
                    listener.as_ref().unchecked_ref(),
                    capture,
                );
        }
    }

    /// Remove every listener this pane attached to its display.
    pub fn dispose(&self) {
        self.unbind_wheel_and_touch_move();
        let Ok(mut listeners) = self.pane_listeners.try_borrow_mut() else {
            return;
        };
        for (kind, listener) in listeners.drain(..) {
            let _ = self
                .shared
                .display
                .remove_event_listener_with_callback(kind, listener.as_ref().unchecked_ref());
        }
    }

    fn listener(&self, react: fn(&ForwardingShared, &Event)) -> Listener {
        let shared = Rc::clone(&self.shared);
        Closure::new(move |event: Event| react(&shared, &event))
    }

    fn listen_pane(
        &self,
        kind: &'static str,
        passive: Option<bool>,
        react: fn(&ForwardingShared, &Event),
    ) {
        let listener = self.listener(react);
        let callback = listener.as_ref().unchecked_ref();
        let _ = match passive {
            Some(passive) => {
                let options = AddEventListenerOptions::new();
                options.set_passive(passive);
                self.shared
                    .display
                    .add_event_listener_with_callback_and_add_event_listener_options(
                        kind, callback, &options,
                    )
            }
            None => self
                .shared
                .display
                .add_event_listener_with_callback(kind, callback),
        };
        if let Ok(mut listeners) = self.pane_listeners.try_borrow_mut() {
            listeners.push((kind, listener));
        }
    }
}

impl Drop for TerminalMouseForwarding {
    fn drop(&mut self) {
        self.dispose();
    }
}
