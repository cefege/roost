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
use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{
    Badge, BadgeVariant, Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader,
    CardTitle, EmptyState, FieldError, Icon, IconType, List, ListRow, Select, SelectOption,
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
            class: "flex flex-col gap-5",
            "data-testid": "attachments-pane",
            Card {
                CardHeader {
                    CardTitle { "Attachments" }
                    CardDescription {
                        "Files dropped into a terminal land in .roost/media inside the session's folder (git-ignored). They're deleted after 7 days."
                    }
                }
                CardContent {
                    div { class: "flex items-end gap-3",
                        Select {
                            label: "Session".to_owned(),
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
                        div { class: "inline-flex items-center gap-2 text-sm text-muted-foreground",
                            Icon { icon: IconType::Info, class: "size-4".to_owned() }
                            {message}
                        }
                    }
                    if let Some(message) = state.error.clone() {
                        FieldError { {message} }
                    }
                }
            }
            Card {
                CardHeader {
                    CardTitle { "Files" }
                }
                CardContent {
                    if !state.loaded {
                        span { class: "text-sm text-muted-foreground", "Loading…" }
                    } else if state.entries.is_empty() {
                        EmptyState {
                            icon: IconType::FolderOpen,
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
    let (copy_path, path_copied) = use_copy_clipboard(None);
    let on_copy = {
        let path = abs_path.clone();
        move |_event: MouseEvent| copy_path(&path)
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
            headline: rsx! { span { class: "font-mono", {filename.clone()} } },
            support: Some(rsx! {
                span { class: "inline-flex items-center gap-3",
                    span { {format_bytes(entry.size_bytes as f64)} }
                    span { {format_age(now_ms, entry.mtime_ms)} }
                    Badge {
                        variant: if is_expiring(now_ms, entry.mtime_ms) { BadgeVariant::Destructive } else { BadgeVariant::Secondary },
                        {expiry}
                    }
                }
            }),
            trailing: Some(rsx! {
                Button {
                    variant: ButtonVariant::Ghost,
                    icon: Some(IconType::ClipboardPaste),
                    "data-testid": format!("attachments-inject-{filename}"),
                    onclick: on_inject,
                    "Inject"
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    icon: Some(if path_copied() { IconType::Check } else { IconType::Copy }),
                    "data-testid": format!("attachments-copy-{filename}"),
                    onclick: on_copy,
                    if path_copied() { "Copied" } else { "Copy" }
                }
                Button {
                    variant: ButtonVariant::Destructive,
                    icon: Some(IconType::Trash2),
                    "data-testid": format!("attachments-delete-{filename}"),
                    onclick: on_delete,
                    "Delete"
                }
            }),
        }
    }
}

/// The glyph a file's extension earns.
fn icon_for_file(filename: &str) -> IconType {
    let extension = filename
        .rsplit_once('.')
        .map(|(_, tail)| tail.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "heic" => IconType::FileImage,
        "mp4" | "mov" | "mkv" | "webm" => IconType::FileVideo,
        "mp3" | "wav" | "ogg" | "flac" | "m4a" => IconType::FileAudio,
        "pdf" => IconType::FileText,
        "zip" | "tar" | "gz" | "tgz" | "rar" | "7z" => IconType::FileArchive,
        "md" | "txt" | "log" => IconType::FileText,
        "json" | "yaml" | "yml" | "toml" | "csv" => IconType::FileJson,
        "ts" | "tsx" | "js" | "jsx" | "rs" | "py" | "go" | "c" | "cpp" | "h" | "html" | "css"
        | "swift" | "rb" => IconType::Code,
        _ => IconType::File,
    }
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
