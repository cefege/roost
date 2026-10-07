//! The terminal display's contextmenu surface. Mouse reporting keeps physical
//! right-clicks when active; Shift retains the terminal's selection bypass.
//! Copy, paste, and find stay on the pane's existing guarded action paths.

use dioxus::prelude::*;

use super::pane_handle::PaneHandle;
use crate::components::context_menu::terminal_menu::{
    TerminalActionSheet, TerminalFloatingMenu, should_open_terminal_context_menu, uses_action_sheet,
};

#[derive(Debug, Clone)]
struct MenuTarget {
    client_x: f64,
    client_y: f64,
    selection: String,
}

/// The terminal display and its contextual actions.
#[component]
pub fn TerminalContextMenu(
    session_id: String,
    display_style: String,
    focusable: bool,
    mouse_forwarded: bool,
    compact: bool,
    handle: PaneHandle,
    on_display_mounted: EventHandler<MountedEvent>,
) -> Element {
    let mut target = use_signal(|| None::<MenuTarget>);
    let on_context_menu = move |event: MouseEvent| {
        event.prevent_default();
        let (shift_held, button, trusted) = context_event_modifiers(&event);
        if should_open_terminal_context_menu(mouse_forwarded, shift_held, button, trusted) {
            let point = event.client_coordinates();
            target.set(Some(MenuTarget {
                client_x: point.x,
                client_y: point.y,
                selection: selection_text(),
            }));
        }
    };
    rsx! {
        div {
            "data-testid": "terminal-display",
            tabindex: focusable.then_some("0"),
            style: display_style,
            onmounted: move |event| on_display_mounted.call(event),
            oncontextmenu: on_context_menu,
        }
        if let Some(menu) = target() {
            if uses_action_sheet(compact) {
                TerminalActionSheet {
                    menu_id: format!("terminal-context-menu-{session_id}"),
                    selection: menu.selection,
                    on_copy: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.copy_selection();
                            target.set(None);
                        }
                    },
                    on_paste: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.paste_from_clipboard();
                            target.set(None);
                        }
                    },
                    on_find: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.open_find();
                            target.set(None);
                        }
                    },
                    on_cancel: move |_| target.set(None),
                }
            } else {
                TerminalFloatingMenu {
                    x: menu.client_x,
                    y: menu.client_y,
                    menu_id: format!("terminal-context-menu-{session_id}"),
                    selection: menu.selection,
                    on_copy: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.copy_selection();
                            target.set(None);
                        }
                    },
                    on_paste: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.paste_from_clipboard();
                            target.set(None);
                        }
                    },
                    on_find: {
                        let handle = handle.clone();
                        let mut target = target;
                        move |_| {
                            handle.open_find();
                            target.set(None);
                        }
                    },
                    on_close: move |_| target.set(None),
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn context_event_modifiers(event: &MouseEvent) -> (bool, i16, bool) {
    event
        .data()
        .downcast::<web_sys::MouseEvent>()
        .map(|native| (native.shift_key(), native.button(), native.is_trusted()))
        .unwrap_or((false, 0, true))
}

#[cfg(not(target_arch = "wasm32"))]
fn context_event_modifiers(_event: &MouseEvent) -> (bool, i16, bool) {
    (false, 0, true)
}

#[cfg(target_arch = "wasm32")]
fn selection_text() -> String {
    web_sys::window()
        .and_then(|window| window.get_selection().ok().flatten())
        .and_then(|selection| selection.to_string().as_string())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn selection_text() -> String {
    String::new()
}
