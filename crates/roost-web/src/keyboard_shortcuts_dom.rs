//! The window capture-phase keydown listener behind `keyboard_shortcuts`: it
//! gathers `KeydownContext` from the document and the store, asks
//! `keydown_action`, and performs the answer. wasm32 only; installed once by
//! `App`. Ports `installKeyboardShortcuts` and the DOM probes of
//! `apps/web/src/lib/keyboardShortcuts.ts`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::prefs::terminal_font::{
    TERMINAL_FONT_DEFAULT_PX, TERMINAL_FONT_TV_DEFAULT_PX,
};
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::store::sidebar::SidebarIntent;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{HtmlElement, KeyboardEvent};

use crate::input_nav::NavModality;
use crate::keyboard_shortcuts::{KeydownAction, KeydownContext, ShortcutOverlays, keydown_action};
use crate::platform::browser_platform::{browser_platform, shortcut_key};
use crate::pump::Pump;
use crate::router_state::navigate_path;
use crate::routes::settings_pane_href;

/// The settings pane ⌘, opens.
const SETTINGS_PANE: &str = "machines";

/// The listener, for the life of the application root.
pub struct KeyboardShortcutsGuard {
    listener: Closure<dyn FnMut(KeyboardEvent)>,
}

impl std::fmt::Debug for KeyboardShortcutsGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KeyboardShortcutsGuard")
    }
}

impl Drop for KeyboardShortcutsGuard {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback_and_bool(
                "keydown",
                self.listener.as_ref().unchecked_ref(),
                true,
            );
        }
    }
}

/// v2 `terminalOwnsKeyboard`: a deck (or pane) is on screen. Keyed on the
/// STABLE deck element, never on per-slot focus, which drops out during every
/// layout reconcile.
pub fn terminal_owns_keyboard(document: &web_sys::Document) -> bool {
    let present = |selector: &str| document.query_selector(selector).ok().flatten().is_some();
    present("[data-testid=\"terminal-deck\"]") || present("[data-pane]")
}

/// Install the router on `window` (capture phase, ahead of the terminal).
pub fn install_keyboard_shortcuts(
    pump: Pump,
    overlays: ShortcutOverlays,
    path: Signal<String>,
    modality: Signal<NavModality>,
) -> KeyboardShortcutsGuard {
    let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let target = event
            .target()
            .and_then(|target| target.dyn_into::<HtmlElement>().ok());
        let tag = target
            .as_ref()
            .map(|element| element.tag_name())
            .unwrap_or_default();
        let text_field = tag == "INPUT" || tag == "TEXTAREA";
        let active = document.active_element();
        let focus_on_body = active.is_none()
            || active.as_ref().is_some_and(|element| {
                element.tag_name() == "BODY" || element.tag_name() == "HTML"
            });
        let (cursor_row_selected, has_cursor_targets) = {
            let core = pump.core();
            let core = core.borrow();
            let cursor = &core.store().sidebar.cursor;
            (
                cursor.cursor_session_id().is_some(),
                cursor.has_cursor_targets(),
            )
        };
        let context = KeydownContext {
            platform: browser_platform(),
            default_prevented: event.default_prevented(),
            palette_open: *overlays.palette.peek(),
            help_open: *overlays.help.peek(),
            controller_map_open: *overlays.controller_map.peek(),
            terminal_owns_keyboard: terminal_owns_keyboard(&document),
            target_in_terminal_input: target
                .as_ref()
                .and_then(|element| element.closest(".wterm").ok().flatten())
                .is_some(),
            focus_on_body,
            target_is_text_field: text_field,
            target_editable: text_field
                || target
                    .as_ref()
                    .is_some_and(HtmlElement::is_content_editable),
            directional_input_active: modality.peek().directional_input_active(),
            cursor_row_selected,
            has_cursor_targets,
        };
        let action = keydown_action(&shortcut_key(&event), &context);
        if action.prevents_default() {
            event.prevent_default();
        }
        perform(&pump, overlays, path, &modality.peek(), action);
    });
    if let Some(window) = web_sys::window() {
        let _ = window.add_event_listener_with_callback_and_bool(
            "keydown",
            listener.as_ref().unchecked_ref(),
            true,
        );
    }
    tracing::info!(target: "shortcuts", "keyboard shortcuts installed");
    KeyboardShortcutsGuard { listener }
}

fn perform(
    pump: &Pump,
    overlays: ShortcutOverlays,
    path: Signal<String>,
    modality: &NavModality,
    action: KeydownAction,
) {
    let toggle = |flag: Signal<bool>| {
        let mut flag = flag;
        let next = !*flag.peek();
        flag.set(next);
    };
    match action {
        KeydownAction::Ignore => {}
        KeydownAction::ClosePalette => {
            let mut palette = overlays.palette;
            palette.set(false);
        }
        KeydownAction::TogglePalette => toggle(overlays.palette),
        KeydownAction::ToggleHelp => toggle(overlays.help),
        KeydownAction::StepTermFont(delta) => {
            pump.dispatch(ClientEvent::Shell(ShellIntent::StepTermFont { delta }))
        }
        KeydownAction::ResetTermFont => {
            let default_px = if modality.tv_mode_active() {
                TERMINAL_FONT_TV_DEFAULT_PX
            } else {
                TERMINAL_FONT_DEFAULT_PX
            };
            pump.dispatch(ClientEvent::Shell(ShellIntent::ResetTermFont {
                default_px,
            }));
        }
        KeydownAction::OpenSettings => navigate_path(path, settings_pane_href(SETTINGS_PANE)),
        KeydownAction::ActivateCursor => {
            let target = pump
                .core()
                .borrow()
                .store()
                .sidebar
                .cursor
                .cursor_session_id()
                .map(str::to_owned);
            if let Some(session_id) = target {
                navigate_path(path, crate::routes::session_href(&session_id));
            }
        }
        KeydownAction::MoveCursor(delta) => {
            pump.dispatch(ClientEvent::Sidebar(SidebarIntent::MoveCursor(delta)))
        }
    }
}

/// Push the terminal font preference onto `<html>` (`--term-font-size`) so a
/// pane's first measurement uses it; re-applied whenever it changes.
pub fn apply_term_font_size(px: u32) {
    if let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.document_element())
        .and_then(|root| root.dyn_into::<HtmlElement>().ok())
    {
        let _ = root
            .style()
            .set_property("--term-font-size", &format!("{px}px"));
    }
}
