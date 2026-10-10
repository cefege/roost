//! The terminal display's contextmenu surface. Mouse reporting keeps physical
//! right-clicks when active; Shift retains the terminal's selection bypass.
//! Copy, paste, and find stay on the pane's existing guarded action paths.

use dioxus::prelude::*;

use super::pane_handle::PaneHandle;
use crate::components::context_menu::terminal_menu::{
    TerminalActionSheet, TerminalFloatingMenu, should_open_terminal_context_menu, uses_action_sheet,
};
use crate::components::deck::terminal_deck_model::deck_folder_for;
use crate::components::deck::terminal_deck_spawn::{SpawnAnchor, start_deck_agent};
use crate::components::layout::portal::Portal;
use crate::pump::{Pump, use_store};
use roost_client_core::deck::DeckSpawn;
use roost_client_core::store::layout::find_leaf_of_tab;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::folder_path_of;

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
    let pump = use_store();
    let mut target = use_signal(|| None::<MenuTarget>);
    let agent_pump = pump.clone();
    let agent_session_id = session_id.clone();
    let mut agent_target = target;
    let on_start_agent = Some(EventHandler::new(move |()| {
        start_agent_for_session(&agent_pump, &agent_session_id, compact);
        agent_target.set(None);
    }));
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
        // Portaled: the deck's transform makes it the containing block of a
        // `position: fixed` menu, which then lands offset by the deck's origin.
        if let Some(menu) = target() {
            Portal {
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
                        on_start_agent,
                        on_cancel: move |_| target.set(None),
                    }
                } else {
                    TerminalFloatingMenu {
                        key: "{menu.client_x}-{menu.client_y}",
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
                        on_start_agent,
                        on_close: move |_| target.set(None),
                    }
                }
            }
        }
    }
}

fn start_agent_for_session(pump: &Pump, session_id: &str, compact: bool) {
    let launch = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let Some(session) = session_by_id(store, session_id) else {
            return;
        };
        let folder = deck_folder_for(store, &crate::platform::BrowserWorkerPaths, session);
        let layout = store.deck.resolve_layout(&folder);
        let pane_id = find_leaf_of_tab(&layout.root, session_id)
            .map(|leaf| leaf.pane_id.clone())
            .unwrap_or_else(|| layout.focused_pane_id.clone());
        (
            DeckSpawn::NewTab { pane_id },
            SpawnAnchor {
                tab_id: session.id.as_str().to_owned(),
                worker_fp: session.worker_fp.as_str().to_owned(),
                folder: folder_path_of(session).to_owned(),
            },
        )
    };
    start_deck_agent(pump.clone(), launch.0, launch.1, compact);
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
