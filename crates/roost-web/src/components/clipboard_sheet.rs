//! The fleet clipboard history: every terminal copy on any device, newest
//! first, with Copy, Paste into the terminal on screen, Delete, and Clear all.
//!
//! Mounted once with the authorized overlays; opened by Mod+Shift+H or the
//! palette through the shortcut router's `clipboard_history` signal. It asks
//! the coordinator for the list on open; live Sync frames keep the store current
//! while it is open. Pull only: nothing here ever writes another device's
//! clipboard or raises a popup on it.

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::clipboard::{
    ClipboardClear, ClipboardDelete, ClipboardEntry, ClipboardList,
};
use roost_client_core::store::Store;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

use crate::components::hooks::use_copy_clipboard::copy_text_then;
use crate::components::notifications::store_write::write_store;
use crate::components::settings::format::relative_time;
use crate::components::terminal::dom::now_ms;
use crate::components::terminal::pane_registry::use_pane_registry;
use crate::components::ui::{
    AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter,
    AlertDialogHeader, AlertDialogTitle, Button, ButtonVariant, Dialog, DialogContent,
    DialogHeader, DialogTitle, EmptyState, IconButton, IconType, List, ListRow,
};
use crate::keyboard_shortcuts::use_shortcut_overlays;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;

/// Lines of an entry shown in its row; the rest is one Copy away.
const PREVIEW_LINES: usize = 2;

/// One row as the sheet renders it.
#[derive(Debug, Clone, PartialEq)]
struct EntryRow {
    id: String,
    text: String,
    preview: String,
    source: String,
}

/// Clipboard history overlay mounted with the other authorized overlays.
#[component]
pub fn ClipboardSheet() -> Element {
    let overlays = use_shortcut_overlays();
    let pump = use_store();
    let panes = use_pane_registry();
    let path = use_location();
    let open = overlays.clipboard_history.cloned();
    let mut selected = use_signal(|| 0usize);
    let mut confirm_clear = use_signal(|| false);

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

    let load_pump = pump.clone();
    use_effect(move || {
        if !*overlays.clipboard_history.read() {
            return;
        }
        selected.set(0);
        load_history(load_pump.clone());
    });

    let copy = {
        let pump = pump.clone();
        move |text: String| copy_entry(&pump, &text)
    };
    let paste = {
        let pump = pump.clone();
        let panes = panes.clone();
        let target = target_session.clone();
        let mut visible = overlays.clipboard_history;
        move |text: String| {
            let pasted = target
                .as_deref()
                .is_some_and(|session| panes.paste_text(session, &text));
            if pasted {
                visible.set(false);
            } else {
                notify(&pump, "Open a terminal to paste into", ToastKind::Warn);
            }
        }
    };
    let delete = {
        let pump = pump.clone();
        move |id: String| delete_entry(pump.clone(), id)
    };

    let keyboard_rows = rows.clone();
    let on_keydown = {
        let copy = copy.clone();
        let mut paste = paste.clone();
        let delete = delete.clone();
        move |event: KeyboardEvent| {
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
                Key::Named(NamedKey::Enter) => paste(row.text),
                Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => delete(row.id),
                Key::Character(key) if modifier && key.eq_ignore_ascii_case("c") => copy(row.text),
                _ => return,
            }
            event.prevent_default();
        }
    };

    let clear_pump = pump.clone();
    let on_open_change = {
        let mut visible = overlays.clipboard_history;
        EventHandler::new(move |next: bool| visible.set(next))
    };
    rsx! {
        Dialog { open, on_open_change,
            DialogContent { class: "sm:max-w-2xl", test_id: "clipboard-history",
                DialogHeader {
                    DialogTitle { "Clipboard history" }
                }
                div {
                    tabindex: "0",
                    "data-testid": "clipboard-history-list",
                    class: "flex flex-col gap-2",
                    onkeydown: on_keydown,
                    if rows.is_empty() {
                        EmptyState {
                            icon: IconType::ClipboardPaste,
                            title: "Nothing copied yet",
                            supporting: Some("Text you copy in any Roost terminal, on any device, appears here.".to_owned()),
                        }
                    } else {
                        div { class: "flex justify-end",
                            Button {
                                variant: ButtonVariant::Ghost,
                                onclick: move |_| confirm_clear.set(true),
                                "Clear all"
                            }
                        }
                        List {
                            for (index, row) in rows.into_iter().enumerate() {
                                ClipboardRow {
                                    key: "{row.id}",
                                    row,
                                    selected: index == *selected.read(),
                                    on_copy: copy.clone(),
                                    on_paste: paste.clone(),
                                    on_delete: delete.clone(),
                                }
                            }
                        }
                    }
                }
            }
        }
        AlertDialog {
            open: *confirm_clear.read(),
            on_open_change: move |next: bool| confirm_clear.set(next),
            AlertDialogContent {
                AlertDialogHeader {
                    AlertDialogTitle { "Clear clipboard history?" }
                    AlertDialogDescription { "Every entry is removed for every device. This cannot be undone." }
                }
                AlertDialogFooter {
                    AlertDialogCancel { "Cancel" }
                    Button {
                        variant: ButtonVariant::Destructive,
                        onclick: move |_| {
                            confirm_clear.set(false);
                            clear_history(clear_pump.clone());
                        },
                        "Clear all"
                    }
                }
            }
        }
    }
}

/// One entry: preview, where it came from, and its three actions.
#[component]
fn ClipboardRow(
    row: EntryRow,
    selected: bool,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<String>,
    on_delete: EventHandler<String>,
) -> Element {
    let copy_text = row.text.clone();
    let paste_text = row.text.clone();
    let delete_id = row.id.clone();
    rsx! {
        ListRow {
            selected,
            headline: rsx! { pre { class: "m-0 font-mono text-xs whitespace-pre-wrap break-words", "{row.preview}" } },
            support: Some(rsx! { "{row.source}" }),
            trailing: Some(rsx! {
                IconButton {
                    icon: IconType::Copy,
                    label: "Copy",
                    onclick: move |_| on_copy.call(copy_text.clone()),
                }
                IconButton {
                    icon: IconType::CornerDownLeft,
                    label: "Paste into the terminal",
                    onclick: move |_| on_paste.call(paste_text.clone()),
                }
                IconButton {
                    icon: IconType::Trash2,
                    label: "Delete",
                    onclick: move |_| on_delete.call(delete_id.clone()),
                }
            }),
        }
    }
}

/// The row text: the first lines, and "machine · session · 3 min ago".
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

/// Copy locally. Never a new history entry: the text is already in it.
fn copy_entry(pump: &Pump, text: &str) {
    let pump = pump.clone();
    copy_text_then(text, move |accepted| {
        if accepted {
            notify(&pump, "Copied", ToastKind::Ok);
        }
    });
}

fn load_history(pump: Pump) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&ClipboardList).await {
            Ok(entries) => pump.write_store(|store| store.clipboard_history.replace(entries)),
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard history load failed");
                notify(
                    &pump,
                    "Could not load the clipboard history",
                    ToastKind::Err,
                );
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardList);
}

fn delete_entry(pump: Pump, id: String) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&ClipboardDelete(id.clone())).await {
            Ok(_) => pump.write_store(|store| store.clipboard_history.remove(&id)),
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard delete failed");
                notify(&pump, "Could not delete the entry", ToastKind::Err);
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardDelete(id));
}

fn clear_history(pump: Pump) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        match pump.rpc().call(&ClipboardClear).await {
            Ok(_) => pump.write_store(|store| store.clipboard_history.replace(Vec::new())),
            Err(error) => {
                tracing::warn!(target: "clipboard", %error, "clipboard clear failed");
                notify(
                    &pump,
                    "Could not clear the clipboard history",
                    ToastKind::Err,
                );
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, ClipboardClear);
}

fn notify(pump: &Pump, message: &str, kind: ToastKind) {
    let id = ToastId::new(
        ToastSource::Host {
            name: "clipboard-history",
        },
        "clipboard-history",
    );
    write_store(pump, |store| {
        add_toast(
            store,
            id,
            message,
            kind,
            ToastOptions::with_ttl(Some(2_500)),
            now_ms(),
        );
    });
}
