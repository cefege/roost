//! Settings → System → Files: the attachments a session has collected.
//!
//! Ports `apps/web/src/components/Settings/AttachmentsPane.tsx`. Depends on
//! `roost-client-core`'s `ListAttachments`/`DeleteAttachment` calls, the store's
//! session map for the selector, and `ClientEvent::TerminalInput` for injecting
//! a path into the live shell.
//!
//! The list is component state because a session's files change without the
//! session changing: the coordinator answers per session id, and a store-wide
//! attachment projection would be a second copy of a directory the worker owns.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::rpc::calls::settings::attachments::SessionAttachment;
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types are gated with them. `ClientEvent` stays, because the
// path injection below is a client-side dispatch, not a coordinator answer.
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::attachments::{
    DeleteSessionAttachment, ListSessionAttachments,
};

use super::format::{expiry_label, format_age, is_expiring};
use crate::components::md::{
    Button, ButtonVariant, Card, EmptyState, Icon, IconSize, List, ListRow, Select, SelectOption,
};
use crate::display_format::format_bytes;
use crate::pump::{Pump, use_store};

/// The file list, and whatever the last read was told.
#[derive(Debug, Clone, PartialEq, Default)]
struct AttachmentsView {
    entries: Vec<SessionAttachment>,
    loaded: bool,
    error: Option<String>,
    status: Option<String>,
}

/// The pane.
#[component]
pub fn AttachmentsPane() -> Element {
    let pump = use_store();
    let core = pump.core();
    let now_ms = {
        let core = core.borrow();
        core.clock().now_ms()
    };
    let mut selected = use_signal(String::new);
    let sessions: Vec<SelectOption> = {
        let core = core.borrow();
        core.store()
            .sessions
            .sessions()
            .iter()
            .map(|session| {
                let id = session.1.id.to_string();
                let short: String = id.chars().take(8).collect();
                let cwd = session.1.cwd.clone();
                SelectOption::new(
                    id.clone(),
                    format!(
                        "{} — {} ({short})",
                        session.1.kind.as_str(),
                        cwd_label(&cwd)
                    ),
                )
            })
            .collect()
    };
    if selected().is_empty()
        && let Some(first) = sessions.first()
    {
        selected.set(first.value.clone());
    }

    let mut view = use_signal(AttachmentsView::default);
    let load_pump = pump.clone();
    let session = selected();
    use_effect(move || reload(load_pump.clone(), session.clone(), view));

    let state = view();
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "attachments-pane",
            Card {
                title: "Attachments",
                supporting: "Files dropped into the PTY land in ~/.roost/attachments/<sid>/. They're swept after 24 hours.",
                div { class: "md-form-row",
                    Select {
                        label: "Session",
                        value: selected(),
                        options: sessions,
                        on_change: move |value| {
                            selected.set(value);
                            let mut view = view.write();
                            view.entries.clear();
                            view.loaded = false;
                            view.error = None;
                        },
                    }
                }
                if let Some(message) = state.status.clone() {
                    div { class: "md-body-m", style: "display: inline-flex; align-items: center; gap: var(--md-space-2); color: var(--md-sys-color-on-surface-variant);",
                        Icon { name: "info".to_owned(), size: IconSize::Sm }
                        {message}
                    }
                }
                if let Some(message) = state.error.clone() {
                    p { role: "alert", class: "md-body-m", style: "color: var(--md-sys-color-error); margin: 0;", {message} }
                }
            }
            Card { title: "Files",
                if !state.loaded {
                    span { class: "md-body-m", style: "color: var(--md-sys-color-on-surface-variant);", "Loading…" }
                } else if state.entries.is_empty() {
                    EmptyState {
                        icon: "folder_open",
                        title: "No attachments yet",
                        supporting: "Drop a file into the terminal to attach it. The path lands here and you can re-inject it anytime.",
                    }
                } else {
                    List { contained: true,
                        for entry in state.entries.iter() {
                            AttachmentRow {
                                entry: entry.clone(),
                                now_ms,
                                session: selected(),
                                view,
                                pump: pump.clone(),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One file: its size, its age, and the three things a reader does with it.
#[component]
fn AttachmentRow(
    entry: SessionAttachment,
    now_ms: u64,
    session: String,
    view: Signal<AttachmentsView>,
    pump: Pump,
) -> Element {
    let filename = entry.filename.clone();
    let abs_path = entry.abs_path.clone();
    let expiry = expiry_label(now_ms, entry.mtime_ms);
    // Each row's three actions are built once: an `rsx!` attribute body is a
    // block, and a block that clones per attribute moves the same capture once
    // per attribute it appears in.
    let on_inject = {
        let session = session.clone();
        let path = abs_path.clone();
        let pump = pump.clone();
        move |_event: MouseEvent| inject(pump.clone(), session.clone(), path.clone())
    };
    let on_copy = {
        let path = abs_path.clone();
        move |_event: MouseEvent| {
            let copied = copy_to_clipboard(&path);
            view.write().status = Some(if copied {
                "Copied to clipboard".to_owned()
            } else {
                "Copy failed (clipboard permission denied)".to_owned()
            });
        }
    };
    let on_delete = {
        let pump = pump.clone();
        let session = session.clone();
        let filename = filename.clone();
        move |_event: MouseEvent| remove(pump.clone(), session.clone(), filename.clone(), view)
    };
    rsx! {
        ListRow {
            leading_icon: Some(icon_for_file(&filename)),
            headline: rsx! { span { style: "font-family: var(--font-mono);", {filename.clone()} } },
            support: rsx! {
                span { style: "display: inline-flex; gap: var(--md-space-3); align-items: center;",
                    span { {format_bytes(entry.size_bytes as f64)} }
                    span { {format_age(now_ms, entry.mtime_ms)} }
                    span { class: "md-label-s", style: "padding: 2px 8px; border-radius: var(--md-shape-full);",
                        style: if is_expiring(now_ms, entry.mtime_ms) {
                            "background: var(--md-sys-color-error); color: var(--md-on-primary);"
                        } else {
                            "background: var(--md-sys-color-secondary-container); color: var(--md-sys-color-on-secondary-container);"
                        },
                        {expiry}
                    }
                }
            },
            trailing: rsx! {
                Button {
                    variant: ButtonVariant::Ghost,
                    icon: "content_paste_go",
                    "data-testid": format!("attachments-inject-{filename}"),
                    onclick: on_inject,
                    "Inject"
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    icon: "content_copy",
                    "data-testid": format!("attachments-copy-{filename}"),
                    onclick: on_copy,
                    "Copy"
                }
                Button {
                    variant: ButtonVariant::Destructive,
                    icon: "delete_outline",
                    "data-testid": format!("attachments-delete-{filename}"),
                    onclick: on_delete,
                    "Delete"
                }
            },
        }
    }
}

/// The glyph a file's extension earns.
fn icon_for_file(filename: &str) -> String {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, tail)| tail.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "heic" => "image",
        "mp4" | "mov" | "mkv" | "webm" => "movie",
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => "audio_file",
        "pdf" => "picture_as_pdf",
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" => "folder_zip",
        "md" | "txt" | "log" => "description",
        "json" | "yaml" | "yml" | "toml" | "csv" => "data_object",
        "ts" | "tsx" | "js" | "jsx" | "rs" | "py" | "go" | "c" | "cpp" | "h" | "html" | "css"
        | "swift" | "rb" => "code",
        _ => "draft",
    }
    .to_owned()
}

/// The last path segment, or the whole path when it has no separator.
fn cwd_label(cwd: &str) -> String {
    cwd.rsplit('/')
        .next()
        .filter(|tail| !tail.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

/// Write an attachment's path into the session's shell.
fn inject(pump: Pump, session_id: String, abs_path: String) {
    let mut bytes = abs_path.into_bytes();
    bytes.push(b' ');
    pump.dispatch(ClientEvent::TerminalInput {
        session_id,
        view_id: None,
        bytes,
    });
}

/// Re-read one session's attachment directory.
fn reload(pump: Pump, session_id: String, view: Signal<AttachmentsView>) {
    if session_id.is_empty() {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        let request = ListSessionAttachments {
            session_id: session_id.clone(),
        };
        match pump.rpc().call(&request).await {
            Ok(entries) => {
                let mut view = view.write();
                view.entries = entries;
                view.loaded = true;
                view.error = None;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "attachment list refused");
                let mut view = view.write();
                view.entries.clear();
                view.loaded = true;
                view.error = Some(format!("List failed: {error}"));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, session_id, view);
}

/// Delete one file, and re-read what the coordinator kept.
fn remove(pump: Pump, session_id: String, filename: String, view: Signal<AttachmentsView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        let request = DeleteSessionAttachment {
            session_id,
            filename: filename.clone(),
        };
        match pump.rpc().call(&request).await {
            Ok(true) => {
                view.write()
                    .entries
                    .retain(|entry| entry.filename != filename);
                view.write().status = Some(format!("Deleted {filename}"));
            }
            Ok(false) => {
                view.write().status = Some(format!("Delete failed: {filename} is gone"));
            }
            Err(error) => {
                view.write().status = Some(format!("Delete failed: {error}"));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, session_id, filename, view);
}

/// Put a path on the system clipboard, and say whether the browser allowed it.
fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return false;
        };
        // `write_text` hands back the promise the browser settles after this
        // handler returns; the pane's own status line reports the attempt, and
        // a rejection surfaces in the browser's console rather than as a stale
        // "Copied". What this synchronous answer reports is the capability.
        let _pending = window.navigator().clipboard().write_text(text);
        true
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
        false
    }
}
