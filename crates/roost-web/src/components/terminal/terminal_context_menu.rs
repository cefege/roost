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
use crate::pump::{Pump, use_store};
use roost_client_core::deck::DeckSpawn;
use roost_client_core::store::layout::find_leaf_of_tab;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::sidebar::folder_groups::folder_path_of;

#[derive(Debug, Clone)]
struct MenuTarget {
    generation: u64,
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
    let mut next_generation = use_hook(|| CopyValue::new(0_u64));
    let opened_generation = target.read().as_ref().map(|request| request.generation);
    let close = EventHandler::new(move |()| close_menu_target(target, opened_generation));
    let agent_pump = pump.clone();
    let agent_session_id = session_id.clone();
    let on_start_agent = Some(EventHandler::new(move |()| {
        start_agent_for_session(&agent_pump, &agent_session_id, compact);
        close.call(());
    }));
    let on_context_menu = move |event: MouseEvent| {
        event.prevent_default();
        let (shift_held, button, trusted) = context_event_modifiers(&event);
        if should_open_terminal_context_menu(mouse_forwarded, shift_held, button, trusted) {
            let point = event.client_coordinates();
            let generation = next_generation().wrapping_add(1);
            next_generation.set(generation);
            target.set(Some(MenuTarget {
                generation,
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
                        key: "{menu.generation}",
                        menu_id: format!("terminal-context-menu-{session_id}"),
                        selection: menu.selection,
                        on_copy: {
                            let handle = handle.clone();
                            move |_| {
                                handle.copy_selection();
                                close.call(());
                            }
                        },
                        on_paste: {
                            let handle = handle.clone();
                            move |_| {
                                handle.paste_from_clipboard();
                                close.call(());
                            }
                        },
                        on_find: {
                            let handle = handle.clone();
                            move |_| {
                                handle.open_find();
                                close.call(());
                            }
                        },
                        on_start_agent,
                        on_cancel: close,
                    }
                } else {
                    TerminalFloatingMenu {
                        key: "{menu.generation}",
                        x: menu.client_x,
                        y: menu.client_y,
                        selection: menu.selection,
                        on_copy: {
                            let handle = handle.clone();
                            move |_| {
                                handle.copy_selection();
                                close.call(());
                            }
                        },
                        on_find: {
                            let handle = handle.clone();
                            move |_| {
                                handle.open_find();
                                close.call(());
                            }
                        },
                        on_start_agent,
                        on_close: close,
                    }
                }
        }
    }
}

fn close_menu_target(mut target: Signal<Option<MenuTarget>>, generation: Option<u64>) {
    if generation.is_some()
        && target
            .try_peek()
            .is_ok_and(|target| target.as_ref().map(|request| request.generation) == generation)
    {
        target.set(None);
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

#[cfg(test)]
mod tests {
    use super::{MenuTarget, close_menu_target};
    use dioxus::prelude::*;

    fn replacement_fixture() -> Element {
        use_hook(|| {
            let mut target = Signal::new(Some(request(1)));
            close_menu_target(target, Some(1));
            assert!(target.peek().is_none());
            target.set(Some(request(2)));
            close_menu_target(target, Some(1));
            assert_eq!(
                target.peek().as_ref().map(|request| request.generation),
                Some(2)
            );
            close_menu_target(target, Some(2));
            assert!(target.peek().is_none());
        });
        rsx! {}
    }

    fn request(generation: u64) -> MenuTarget {
        MenuTarget {
            generation,
            client_x: 0.0,
            client_y: 0.0,
            selection: String::new(),
        }
    }

    #[test]
    fn a_previous_drawer_completion_cannot_close_the_replacement_terminal_menu() {
        VirtualDom::new(replacement_fixture).rebuild_in_place();
    }
}
