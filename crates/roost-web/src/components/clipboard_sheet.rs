//! Fleet clipboard history with local Copy, terminal Paste and coordinator deletion.
//! Mounted once with authorized overlays; the shortcut flag owns visibility.
//! Libero owns modal focus and dismissal, and completion generations protect replacements.

mod actions;
mod content;

use dioxus::prelude::*;
use libero::{
    components::{Button, Dialog, Flex, Text},
    hooks::{ModalScope, use_id, use_modal},
};
use roost_client_core::client::rpc::calls::clipboard::ClipboardEntry;
use roost_client_core::store::{Store, selectors::session_by_id, toasts::ToastKind};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;
use crate::components::settings::format::relative_time;
use crate::components::terminal::dom::now_ms;
use crate::components::terminal::pane_registry::use_pane_registry;
use crate::keyboard_shortcuts::use_shortcut_overlays;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;
use actions::{clear_history, copy_entry, delete_entry, load_history, notify};
use content::ClipboardContent;

const PREVIEW_LINES: usize = 2;

#[derive(Debug, Clone, PartialEq)]
struct EntryRow {
    id: String,
    text: String,
    preview: String,
    source: String,
}

#[component]
pub fn ClipboardSheet() -> Element {
    let overlays = use_shortcut_overlays();
    let pump = use_store();
    let panes = use_pane_registry();
    let path = use_location();
    let mut selected = use_signal(|| 0usize);
    let mut confirm_clear = use_signal(|| false);
    let mut confirm_busy = use_signal(|| false);
    let history_fence = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let clear_fence = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let history_synced = use_hook(|| Rc::new(Cell::new(false)));
    let clear_synced = use_hook(|| Rc::new(Cell::new(false)));
    let message_id = use_id().read().clone();

    let _ = pump.revision().read();
    let (rows, target_session) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now = now_ms();
        let rows: Vec<EntryRow> = store
            .clipboard_history
            .entries()
            .iter()
            .map(|entry| entry_row(store, entry, now))
            .collect();
        let target = active_session_for_path(store, &BrowserWorkerPaths, &path())
            .map(|session| session.id.as_str().to_owned());
        (rows, target)
    };
    let count = rows.len();
    let copy = {
        let pump = pump.clone();
        EventHandler::new(move |text: String| copy_entry(&pump, &text))
    };
    let paste = {
        let pump = pump.clone();
        let mut visible = overlays.clipboard_history;
        EventHandler::new(move |text: String| {
            let pasted = target_session
                .as_deref()
                .is_some_and(|session| panes.paste_text(session, &text));
            if pasted {
                visible.set(false);
            } else {
                notify(&pump, "Open a terminal to paste into", ToastKind::Warn);
            }
        })
    };
    let delete = {
        let pump = pump.clone();
        EventHandler::new(move |id: String| delete_entry(pump.clone(), id))
    };
    let keyboard_rows = rows.clone();
    let on_keydown = EventHandler::new(move |event: KeyboardEvent| {
        let current = *selected.peek();
        let Some(row) = keyboard_rows.get(current).cloned() else {
            return;
        };
        let modifier = event.modifiers().meta() || event.modifiers().ctrl();
        match event.key() {
            Key::Named(NamedKey::ArrowDown) => {
                selected.set((current + 1).min(count.saturating_sub(1)))
            }
            Key::Named(NamedKey::ArrowUp) => selected.set(current.saturating_sub(1)),
            Key::Named(NamedKey::Enter) => paste.call(row.text),
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => delete.call(row.id),
            Key::Character(key) if modifier && key.eq_ignore_ascii_case("c") => copy.call(row.text),
            _ => return,
        }
        event.prevent_default();
    });

    let history = use_modal(move |_: ModalScope<()>| {
        rsx! {
            Dialog { title: "Clipboard history", size: "min(42rem, 100%)",
                "data-testid": "clipboard-history",
                ClipboardContent { rows: rows.clone(), selected: selected(),
                    on_keydown, on_copy: copy, on_paste: paste, on_delete: delete,
                    on_clear: move |_| confirm_clear.set(true),
                }
            }
        }
    });
    let action_fence = clear_fence.clone();
    let load_pump = pump.clone();
    let clear = use_modal(move |scope: ModalScope<u64>| {
        let Some(generation) = scope.try_args() else {
            return rsx! {};
        };
        let pump = pump.clone();
        let fence = action_fence.clone();
        rsx! {
            Dialog { title: "Clear clipboard history?", role: "alertdialog",
                aria_describedby: message_id.clone(),
                ondismiss: Callback::new(move |_| !*confirm_busy.peek()),
                Flex { direction: "column", gap: "sm",
                    Text { id: message_id.clone(), "Every entry is removed for every device. This cannot be undone." }
                    Flex { direction: "row", justify: "end", gap: "xs",
                        Button { variant: "outlined", disabled: Some(confirm_busy()),
                            onclick: move |_| scope.close(), "Cancel"
                        }
                        Button { color: "error", loading: Some(confirm_busy()),
                            onclick: move |_| {
                                if *confirm_busy.peek() { return; }
                                confirm_busy.set(true);
                                let fence = fence.clone();
                                clear_history(pump.clone(), move |accepted| {
                                    if !fence.borrow().is_current(generation) || scope.try_args() != Some(generation) { return; }
                                    if accepted { scope.close(); }
                                    else { confirm_busy.set(false); }
                                });
                            },
                            "Clear all"
                        }
                    }
                }
            }
        }
    });

    let synchronizer = history_fence.clone();
    use_effect(move || {
        let mut visible = overlays.clipboard_history;
        let open = *visible.read();
        if history_synced.replace(open) == open {
            return;
        }
        synchronizer.borrow_mut().invalidate();
        history.close();
        if open {
            selected.set(0);
            load_history(load_pump.clone());
            let generation = synchronizer.borrow_mut().begin();
            let fence = synchronizer.clone();
            history.open().onresult(move |_| {
                let current = fence.borrow_mut().finish(generation);
                if current {
                    visible.set(false);
                }
            });
        } else {
            confirm_clear.set(false);
        }
    });
    let synchronizer = clear_fence.clone();
    use_effect(move || {
        let open = *confirm_clear.read();
        if clear_synced.replace(open) == open {
            return;
        }
        synchronizer.borrow_mut().invalidate();
        clear.close();
        confirm_busy.set(false);
        if open {
            let generation = synchronizer.borrow_mut().begin();
            let fence = synchronizer.clone();
            clear.open_with(generation).onresult(move |_| {
                let current = fence.borrow_mut().finish(generation);
                if current {
                    confirm_clear.set(false);
                }
            });
        }
    });
    use_drop(move || {
        history_fence.borrow_mut().invalidate();
        clear_fence.borrow_mut().invalidate();
    });
    rsx! {}
}

fn entry_row(store: &Store, entry: &ClipboardEntry, now: u64) -> EntryRow {
    let mut preview: String = entry
        .text
        .lines()
        .take(PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    if entry.text.lines().count() > PREVIEW_LINES {
        preview.push_str(" …");
    }
    let machine = store
        .workers
        .get(&entry.source_worker_fp)
        .map(|worker| worker.label.clone())
        .filter(|label| !label.is_empty());
    let session =
        session_by_id(store, &entry.source_session_id).map(|session| session_title(store, session));
    let when = relative_time(now, u64::try_from(entry.created_at_ms).unwrap_or(0));
    let source = [machine, session, Some(when)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    EntryRow {
        id: entry.id.clone(),
        text: entry.text.clone(),
        preview,
        source,
    }
}
