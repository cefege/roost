//! The pane's pointer and selection interactions: the native-selection guard
//! (`PaneSelection`) and the paint hold it drives, mouse forwarding to the
//! application, the link attachment, copy-on-select, and the document/window
//! listeners that exist only while the pane is visibly active. Driven by the
//! pane mount's flag, lifecycle and input paths. Ports
//! `apps/web/src/components/terminal/cell-terminal-interactions.ts`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use dioxus::prelude::*;
use roost_client_core::store::selectors::session_by_id;
use roost_web_terminal::cell_geometry::{TerminalCellGeometry, measure_terminal_cell_box};
use roost_web_terminal::input::PaneSelection;
use roost_web_terminal::links::activation::LinkModifierKey;
use roost_web_terminal::links::{TerminalLinkAttachment, TerminalLinkOptions};
use roost_web_terminal::mouse_forward::{
    MouseForwardingHost, MouseReportModes, PaneMouseModes, TerminalMouseForwarding,
    mouse_gestures_forwarded,
};
use roost_web_terminal::{LiveInteractionResult, ReaderIntentReason};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, EventTarget, MouseEvent};

use super::{PaneShared, input, link_targets, paint};
use crate::platform::browser_platform::{BrowserPlatform, browser_platform};

type Listener = Closure<dyn FnMut(Event)>;

/// The pane's interaction resources, each created at mount.
#[derive(Default)]
pub(in crate::components::terminal) struct PaneInteractions {
    selection: RefCell<Option<PaneSelection>>,
    mouse: RefCell<Option<Rc<TerminalMouseForwarding>>>,
    links: RefCell<Option<TerminalLinkAttachment>>,
    foreground: RefCell<Vec<(EventTarget, &'static str, Listener)>>,
}

/// v2 `foregroundWorkActive`: the view is active and the page is visible.
fn foreground_active(shared: &PaneShared) -> bool {
    let state = shared.state.borrow();
    state.flags.view_active() && state.page_visible
}

/// Create the selection guard, the link attachment and mouse forwarding.
pub(super) fn attach(shared: &Rc<PaneShared>) {
    let display: &web_sys::Element = shared.display.as_ref();
    let modifier = LinkModifierKey::for_platform(browser_platform() == BrowserPlatform::MacOs);
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        let weak = shared.weak_self();
        let selection = PaneSelection::new(&document, display, move |held| {
            if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
                apply_selection_hold(&shared, held);
            }
        });
        *shared.interactions.selection.borrow_mut() = Some(selection);
    }
    let mut options = TerminalLinkOptions::new(modifier);
    options.resolve_file = Some(link_targets::file_resolver(shared.weak_self()));
    options.on_open_file = Some(link_targets::file_opener(shared.weak_self()));
    let (remote, armed, hover) = (shared.weak_self(), shared.weak_self(), shared.weak_self());
    options.github_owner_repo = Some(Box::new(move || {
        let shared = remote.upgrade()?;
        let core = shared.pump.core();
        let core = core.try_borrow().ok()?;
        session_by_id(core.store(), &shared.session_id)?
            .git_remote
            .clone()
            .flatten()
    }));
    options.link_activation_armed = Some(Box::new(move || {
        armed
            .upgrade()
            .is_some_and(|shared| *shared.ui.link_armed.peek())
    }));
    options.on_armed_hover_change = Some(Box::new(move |active| {
        if let Some(shared) = hover.upgrade().filter(|shared| !shared.disposed.get()) {
            let result = shared.renderer.borrow_mut().set_armed_hold(active);
            after_hold_change(&shared, result);
        }
    }));
    options.initial_active = foreground_active(shared);
    *shared.interactions.links.borrow_mut() = TerminalLinkAttachment::attach(display, options);
    let host: Rc<dyn MouseForwardingHost> = Rc::new(MouseHost {
        pane: shared.weak_self(),
    });
    let mouse = TerminalMouseForwarding::attach(display, host, modifier);
    mouse.bind_wheel_and_touch_move();
    *shared.interactions.mouse.borrow_mut() = Some(Rc::new(mouse));
    set_foreground(shared, foreground_active(shared));
}

/// Tear every interaction down.
pub(super) fn detach(shared: &PaneShared) {
    detach_foreground(shared);
    if let Some(mut links) = shared.interactions.links.borrow_mut().take() {
        links.dispose();
    }
    if let Some(mouse) = shared.interactions.mouse.borrow_mut().take() {
        mouse.dispose();
    }
    shared.interactions.selection.borrow_mut().take();
}

/// The foreground gate moved: attach or drop the global listeners.
pub(super) fn sync_foreground(shared: &PaneShared) {
    set_foreground(shared, !shared.disposed.get() && foreground_active(shared));
}

fn set_foreground(shared: &PaneShared, active: bool) {
    if let Some(links) = shared.interactions.links.borrow().as_ref() {
        links.set_active(active);
    }
    let attached = !shared.interactions.foreground.borrow().is_empty();
    if !active {
        detach_foreground(shared);
        return;
    }
    if attached {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let window_target: EventTarget = window.clone().into();
    if let Some(document) = window.document() {
        listen(shared, document.into(), "selectionchange", |shared, _| {
            on_selection_change(shared)
        });
    }
    listen(shared, window_target.clone(), "pointerup", |shared, _| {
        copy_on_select(shared)
    });
    listen(shared, window_target.clone(), "keyup", |shared, _| {
        copy_on_select(shared)
    });
    listen(
        shared,
        window_target.clone(),
        "mousemove",
        |shared, event| {
            with_mouse(shared, event, TerminalMouseForwarding::on_window_mouse_move);
        },
    );
    listen(shared, window_target, "mouseup", |shared, event| {
        with_mouse(shared, event, TerminalMouseForwarding::on_window_mouse_up);
    });
    tracing::debug!(target: "terminal", session_id = %shared.session_id, "pane foreground listeners attached");
    // The selection hold is armed edge-only by `selectionchange`, which was
    // absent while withdrawn: re-derive it from the live selection.
    on_selection_change(shared);
}

fn detach_foreground(shared: &PaneShared) {
    let listeners: Vec<_> = shared
        .interactions
        .foreground
        .borrow_mut()
        .drain(..)
        .collect();
    if listeners.is_empty() {
        return;
    }
    for (target, kind, listener) in listeners {
        let _ = target.remove_event_listener_with_callback(kind, listener.as_ref().unchecked_ref());
    }
    // The window mouseup just removed is the only clearer of a forwarded press.
    let mouse = shared.interactions.mouse.borrow().clone();
    if let Some(mouse) = mouse {
        mouse.complete_held_drag();
    }
    tracing::debug!(target: "terminal", session_id = %shared.session_id, "pane foreground listeners detached");
}

fn listen(
    shared: &PaneShared,
    target: EventTarget,
    kind: &'static str,
    react: fn(&PaneShared, &Event),
) {
    let weak = shared.weak_self();
    let listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            react(&shared, &event);
        }
    });
    let _ = target.add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref());
    shared
        .interactions
        .foreground
        .borrow_mut()
        .push((target, kind, listener));
}

fn with_mouse(
    shared: &PaneShared,
    event: &Event,
    react: fn(&TerminalMouseForwarding, &MouseEvent),
) {
    let mouse = shared.interactions.mouse.borrow().clone();
    if let (Some(mouse), Some(event)) = (mouse, event.dyn_ref::<MouseEvent>()) {
        react(&mouse, event);
    }
}

fn on_selection_change(shared: &PaneShared) {
    if let Ok(mut selection) = shared.interactions.selection.try_borrow_mut()
        && let Some(selection) = selection.as_mut()
    {
        selection.sync_native_selection_hold();
    }
    paint::refresh_presentation(shared);
}

/// The guard's hold sink: a held selection parks the reader first.
fn apply_selection_hold(shared: &PaneShared, held: bool) {
    let result = {
        let mut renderer = shared.renderer.borrow_mut();
        if held {
            renderer.enter_reading(ReaderIntentReason::Selection);
        }
        renderer.set_selection_hold(held)
    };
    if result.anchor_changed {
        paint::backfill_after_anchor_change(shared);
    }
}

fn after_hold_change(shared: &PaneShared, result: LiveInteractionResult) {
    if result.anchor_changed {
        paint::backfill_after_anchor_change(shared);
    }
}

fn copy_on_select(shared: &PaneShared) {
    let enabled = shared
        .pump
        .core()
        .try_borrow()
        .is_ok_and(|core| core.store().prefs.copy_on_select);
    let selection = web_sys::window().and_then(|window| window.get_selection().ok().flatten());
    let Some(selection) = selection.filter(|selection| enabled && !selection.is_collapsed()) else {
        return;
    };
    if selection
        .anchor_node()
        .is_some_and(|anchor| shared.display.contains(Some(&anchor)))
    {
        input::copy_selection();
    }
}

/// Before live input: end reader holds and drop a pane-owned selection.
pub(super) fn prepare_live_interaction(shared: &PaneShared) {
    let renderer = &shared.renderer;
    let result = match shared.interactions.selection.try_borrow_mut() {
        Ok(mut selection) => match selection.as_mut() {
            Some(selection) => selection.prepare_live_interaction(renderer),
            None => renderer.borrow_mut().prepare_live_interaction(),
        },
        Err(_) => renderer.borrow_mut().prepare_live_interaction(),
    };
    release_links(shared);
    after_hold_change(shared, result);
}

/// Leaving the visible surface ends every reader interval and the link hold.
pub(super) fn release_paint_holds(shared: &PaneShared) {
    let renderer = &shared.renderer;
    let released = match shared.interactions.selection.try_borrow_mut() {
        Ok(mut selection) => match selection.as_mut() {
            Some(selection) => selection.release_paint_holds(renderer),
            None => renderer.borrow_mut().set_selection_hold(false),
        },
        Err(_) => renderer.borrow_mut().set_selection_hold(false),
    };
    let armed = renderer.borrow_mut().set_armed_hold(false);
    let result = LiveInteractionResult {
        reconciled: released.reconciled || armed.reconciled,
        anchor_changed: released.anchor_changed || armed.anchor_changed,
    };
    release_links(shared);
    after_hold_change(shared, result);
}

/// Drop a link hover/arming hold; the pane lost the keyboard or went live.
pub(super) fn release_links(shared: &PaneShared) {
    if let Some(links) = shared.interactions.links.borrow().as_ref() {
        links.release_interaction();
    }
}

/// What mouse forwarding reads off the pane, live per event.
struct MouseHost {
    pane: Weak<PaneShared>,
}

impl MouseHost {
    fn pane(&self) -> Option<Rc<PaneShared>> {
        self.pane.upgrade().filter(|shared| !shared.disposed.get())
    }
}

impl MouseForwardingHost for MouseHost {
    fn modes(&self) -> PaneMouseModes {
        let shared = self.pane();
        let modes = shared
            .as_ref()
            .map(|shared| shared.modes.get())
            .unwrap_or_default();
        let pref = shared.is_some_and(|shared| {
            shared
                .pump
                .core()
                .try_borrow()
                .is_ok_and(|core| core.store().prefs.mouse_forward)
        });
        PaneMouseModes {
            forward_active: mouse_gestures_forwarded(pref, modes.mouse_tracking),
            report: MouseReportModes {
                tracking: modes.mouse_tracking,
                sgr: modes.mouse_sgr,
            },
        }
    }
    fn viewport_cell_geometry(&self) -> Option<TerminalCellGeometry> {
        let shared = self.pane()?;
        let renderer = shared.renderer.try_borrow().ok()?;
        renderer.viewport_cell_geometry()
    }
    fn cell_size(&self) -> (f64, f64) {
        self.pane()
            .and_then(|shared| shared.cell.get())
            .map_or((0.0, 0.0), |cell| (cell.width, cell.height))
    }
    fn measure_cell(&self) -> bool {
        let Some(shared) = self.pane() else {
            return false;
        };
        let measured = measure_terminal_cell_box(shared.display.as_ref());
        if measured.is_some() {
            shared.cell.set(measured);
        }
        measured.is_some()
    }
    fn viewport_origin(&self) -> Option<(f64, f64)> {
        let shared = self.pane()?;
        let renderer = shared.renderer.try_borrow().ok()?;
        let rect = renderer.prediction_host().get_bounding_client_rect();
        Some((rect.left(), rect.top()))
    }
    fn link_activation_armed(&self) -> bool {
        self.pane()
            .is_some_and(|shared| *shared.ui.link_armed.peek())
    }
    fn send_bytes(&self, bytes: &[u8]) {
        if let Some(shared) = self.pane() {
            input::send_bytes(&shared, bytes.to_vec(), false);
        }
    }
    fn native_scroll_parked(&self) -> bool {
        self.pane().is_some_and(|shared| {
            shared.renderer.try_borrow().is_ok_and(|renderer| {
                renderer.reader_reason() == Some(ReaderIntentReason::NativeScroll)
            })
        })
    }
    fn enter_reading(&self, reason: ReaderIntentReason) {
        if let Some(shared) = self.pane()
            && let Ok(mut renderer) = shared.renderer.try_borrow_mut()
        {
            renderer.finish_live_selection_release();
            renderer.enter_reading(reason);
        }
    }
}
