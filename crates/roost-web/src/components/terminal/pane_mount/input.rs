//! The pane's keyboard: the off-screen textarea controller whose bytes go to
//! the PTY as `ClientEvent::TerminalInput`, the document keydown that reserves
//! copy/paste/find chords and recovers focus, the display's focus-on-press,
//! and the multiline paste guard. Ports
//! `apps/web/src/components/terminal/cell-terminal-input.ts`, the focus arms
//! of `cell-terminal-interactions.ts` and the keydown of `cell-terminal-lifecycle.ts`.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_web_terminal::input::{Modifiers, TerminalInputController, TerminalInputOptions, TerminalKeyEvent};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, EventTarget, KeyboardEvent, MouseEvent};

use super::PaneShared;
use crate::components::terminal::pane_input::{
    ChordModifiers, PasteDecision, ReservedChord, controller_data, paste_decision, reserved_chord,
    terminal_text_bytes,
};
use crate::components::terminal::pane_state::{PaneFlags, set_if_changed};

/// Selectors that own focus themselves; the pane never steals it from them.
const FOCUS_OWNERS: &str = "input, textarea, select, button, [contenteditable], [role=\"dialog\"], [data-focus-owner]";

/// Write bytes to the session's PTY through the core's input path.
pub(super) fn send_bytes(shared: &PaneShared, bytes: Vec<u8>) {
    if bytes.is_empty() || shared.disposed.get() {
        return;
    }
    shared.renderer.borrow_mut().prepare_live_interaction();
    shared.pump.dispatch(ClientEvent::TerminalInput {
        session_id: shared.session_id.clone(),
        view_id: Some(shared.view_id.clone()),
        bytes,
    });
}

/// The controller's data, with the one-shot Ctrl latch applied.
fn on_controller_data(shared: &PaneShared, data: &str) {
    let mut armed = shared.ui.ctrl_armed;
    let latched = *armed.peek();
    if latched {
        armed.set(false);
    }
    send_bytes(shared, controller_data(data, latched).into_bytes());
}

/// Composed text, framed like a paste.
pub(super) fn send_text(shared: &PaneShared, text: &str, submit: bool) {
    let bracketed = shared.modes.get().bracketed_paste;
    send_bytes(shared, terminal_text_bytes(text, bracketed, submit));
}

/// A paste, through the multiline guard.
pub(super) fn paste_text(shared: &PaneShared, text: &str) {
    match paste_decision(text, shared.modes.get().bracketed_paste) {
        PasteDecision::Ignore => {}
        PasteDecision::Send => send_text(shared, text, false),
        PasteDecision::Confirm { lines } => {
            tracing::info!(target: "input", session_id = %shared.session_id, lines,
                "multiline paste awaiting confirmation");
            set_if_changed(shared.ui.pending_paste, Some(text.to_owned()));
        }
    }
}

/// One named key through the textarea encoder.
pub(super) fn dispatch_named_key(shared: &PaneShared, key: &str) {
    if let Some(controller) = shared.input.borrow().as_ref() {
        controller.dispatch_keydown(&TerminalKeyEvent::new(key));
    }
}

/// Create the textarea controller and the focus listeners.
pub(super) fn attach(shared: &Rc<PaneShared>) {
    let weak = shared.weak_self();
    let keys = shared.weak_self();
    let focus = shared.weak_self();
    let data = shared.weak_self();
    let options = TerminalInputOptions {
        cursor_keys_application: Box::new(move || {
            weak.upgrade().is_some_and(|shared| shared.modes.get().cursor_keys_app)
        }),
        focus_events_enabled: Box::new(move || {
            focus.upgrade().is_some_and(|shared| shared.modes.get().focus_events)
        }),
        on_data: Box::new(move |text: &str| {
            if let Some(shared) = data.upgrade() {
                on_controller_data(&shared, text);
            }
        }),
        on_paste: Box::new(move |text: &str, _event: &web_sys::ClipboardEvent| {
            if let Some(shared) = keys.upgrade() {
                paste_text(&shared, text);
            }
        }),
        aria_label: Some(format!("Terminal input — {}", shared.title.borrow())),
        tv_mode_active: false,
    };
    match TerminalInputController::new(shared.display.as_ref(), options) {
        Ok(controller) => *shared.input.borrow_mut() = Some(controller),
        Err(error) => {
            tracing::error!(target: "input", session_id = %shared.session_id, ?error,
                "terminal input controller failed to attach");
        }
    }
    let display: &EventTarget = shared.display.as_ref();
    listen(shared, display, "mousedown", false, on_display_mouse_down);
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        listen(shared, document.as_ref(), "keydown", true, on_document_key_down);
    }
}

fn listen(
    shared: &Rc<PaneShared>,
    target: &EventTarget,
    kind: &'static str,
    capture: bool,
    handler: fn(&PaneShared, &Event),
) {
    let weak = shared.weak_self();
    let listener = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            handler(&shared, &event);
        }
    });
    let _ = target.add_event_listener_with_callback_and_bool(
        kind,
        listener.as_ref().unchecked_ref(),
        capture,
    );
    shared.browser.borrow_mut().keep_listener(target.clone(), kind, capture, listener);
}

/// Destroy the textarea controller.
pub(super) fn detach(shared: &PaneShared) {
    if let Some(mut controller) = shared.input.borrow_mut().take() {
        controller.destroy();
    }
}

fn may_own_focus(shared: &PaneShared) -> bool {
    let state = shared.state.borrow();
    !state.flags.pending && state.flags.view_active() && state.flags.focused && state.page_visible
}

/// Focus moves to this pane's textarea when it becomes the focused pane.
pub(super) fn focus_if_owner(shared: &PaneShared, previous: PaneFlags, flags: PaneFlags) {
    if !may_own_focus(shared) {
        set_if_changed(shared.ui.ctrl_armed, false);
        set_if_changed(shared.ui.link_armed, false);
        return;
    }
    let became_owner = !previous.focused
        || !previous.view_active()
        || previous.pending != flags.pending;
    if became_owner && let Some(controller) = shared.input.borrow().as_ref() {
        controller.force_focus();
    }
}

fn on_display_mouse_down(shared: &PaneShared, event: &Event) {
    if !may_own_focus(shared) {
        return;
    }
    shared.renderer.borrow_mut().finish_live_selection_release();
    let Some(mouse) = event.dyn_ref::<MouseEvent>() else {
        return;
    };
    if mouse.button() != 0 {
        return;
    }
    let on_control = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|element| element.closest("button, input, textarea, a").ok().flatten())
        .is_some();
    if on_control {
        return;
    }
    if let Some(controller) = shared.input.borrow().as_ref() {
        controller.force_focus();
    }
}

fn on_document_key_down(shared: &PaneShared, event: &Event) {
    let Some(key_event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    if event.default_prevented() || !may_own_focus(shared) {
        return;
    }
    let modifiers = ChordModifiers {
        meta: key_event.meta_key(),
        ctrl: key_event.ctrl_key(),
        shift: key_event.shift_key(),
        alt: key_event.alt_key(),
    };
    if let Some(chord) = reserved_chord(&key_event.key(), modifiers) {
        event.prevent_default();
        event.stop_propagation();
        run_reserved_chord(shared, chord);
        return;
    }
    let document = web_sys::window().and_then(|window| window.document());
    let active = document.as_ref().and_then(web_sys::Document::active_element);
    let input = shared.input.borrow();
    let Some(controller) = input.as_ref() else {
        return;
    };
    if controller.owns_target(active.as_ref()) {
        return;
    }
    let body_focused = match (&active, document.as_ref()) {
        (None, _) => true,
        (Some(active), Some(document)) => {
            document.body().is_some_and(|body| body.is_same_node(Some(active.as_ref())))
                || document
                    .document_element()
                    .is_some_and(|root| root.is_same_node(Some(active.as_ref())))
        }
        _ => false,
    };
    if body_focused {
        let alt_graph = key_event.get_modifier_state("AltGraph");
        if key_event.meta_key() || (key_event.alt_key() && !alt_graph) || key_event.is_composing() {
            return;
        }
        let key = key_event.key();
        if key == "Control" || key == "Shift" {
            return;
        }
        controller.force_focus();
        let terminal_key = TerminalKeyEvent {
            key: &key,
            modifiers: Modifiers {
                shift: key_event.shift_key(),
                alt: key_event.alt_key(),
                ctrl: key_event.ctrl_key(),
                meta: key_event.meta_key(),
            },
            alt_graph,
            is_composing: key_event.is_composing(),
        };
        if controller.dispatch_keydown(&terminal_key) {
            event.prevent_default();
            event.stop_propagation();
        }
        return;
    }
    if key_event.meta_key() || key_event.ctrl_key() || key_event.alt_key() {
        return;
    }
    if active.is_some_and(|active| active.closest(FOCUS_OWNERS).ok().flatten().is_some()) {
        return;
    }
    let key = key_event.key();
    if key.chars().count() != 1 || key == " " {
        return;
    }
    tracing::debug!(target: "input", session_id = %shared.session_id, "focus.recover keydown");
    controller.force_focus();
}

fn run_reserved_chord(shared: &PaneShared, chord: ReservedChord) {
    match chord {
        ReservedChord::OpenFind => set_if_changed(shared.ui.find_open, true),
        ReservedChord::Copy => copy_selection(),
        ReservedChord::Paste => paste_from_clipboard(shared),
    }
}

fn copy_selection() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let text: String = window
        .get_selection()
        .ok()
        .flatten()
        .and_then(|selection| selection.to_string().as_string())
        .unwrap_or_default();
    if text.is_empty() {
        return;
    }
    let _ = window.navigator().clipboard().write_text(&text);
}

fn paste_from_clipboard(shared: &PaneShared) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let promise = window.navigator().clipboard().read_text();
    let weak = shared.weak_self();
    wasm_bindgen_futures::spawn_local(async move {
        let text = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default();
        if let Some(shared) = weak.upgrade().filter(|shared| !shared.disposed.get()) {
            paste_text(&shared, &text);
        }
    });
}
