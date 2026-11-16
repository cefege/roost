//! Folder-picker entries, loading and error states.
//! Called by `browse::picker`; listing/filter state and selection stay with it.
//! Folders tile across the available width; file rows retain dates and downloads.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_client_core::store::browse_entries::{BrowseEntry, relative_entry_time};

use crate::components::browse::BrowseStatus;
use crate::components::browse::dom::RESULTS_ID;
use crate::components::download::DownloadButton;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Chip, EmptyState, IconType, List, ListLayout, ListRow,
    Skeleton,
};
use crate::platform::worker_paths::palette::child_path;
use crate::terminal_href::child_file_href;

/// The placeholder rows painted while a directory listing loads.
const SKELETON_ROWS: usize = 8;

/// The content region.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn BrowseEntryList(
    /// What the region is showing.
    status: BrowseStatus,
    /// The caption under the skeletons, which names what is being waited for.
    loading_caption: String,
    /// The directories the filter admits.
    folders: Vec<BrowseEntry>,
    /// The files the filter admits.
    files: Vec<BrowseEntry>,
    /// Whether files are listed beside the folders.
    show_files: bool,
    /// The in-list filter, which the no-matches copy quotes.
    filter: String,
    /// The machine's fingerprint, for each child's file route.
    server_fp: String,
    /// The platform its path rules follow.
    worker_os: Option<String>,
    /// The resolved directory the entries are in, the base for each child path.
    cwd: String,
    /// The clock the relative-time strings are measured against.
    now_ms: i64,
    /// The keyboard cursor's row, or `-1` before the first arrow key.
    active_idx: i64,
    /// Open shell terminals per listed folder path.
    terminal_counts: BTreeMap<String, usize>,
    /// The reader-facing failure, when the last listing failed.
    error_message: Option<String>,
    on_select: EventHandler<i64>,
    on_drill: EventHandler<String>,
    on_clear_filter: EventHandler<()>,
    on_retry: EventHandler<()>,
) -> Element {
    let mut pointer_type = use_signal(String::new);
    let nothing_to_show = folders.is_empty() && (!show_files || files.is_empty());
    let filtering = !filter.trim().is_empty();
    let mut rows: Vec<Element> = Vec::new();
    for (index, entry) in folders.iter().enumerate() {
        let name = entry.name.clone();
        let child = child_path(worker_os.as_deref(), &cwd, &name)
            .unwrap_or_else(|| format!("{cwd}/{name}"));
        let terminals = terminal_counts
            .get(&child)
            .copied()
            .filter(|count| *count > 0);
        rows.push(rsx! {
            div {
                "data-browse-folder": "",
                onfocusin: move |_| on_select.call(index as i64),
                onpointerdown: move |event: PointerEvent| pointer_type.set(event.pointer_type()),
                onclick: {
                    let drill = name.clone();
                    move |event: MouseEvent| {
                        if pointer_type.peek().as_str() != "touch" && is_double_click(&event) {
                            on_drill.call(drill.clone());
                        }
                    }
                },
                ListRow {
                    dense: true,
                    leading_icon: Some(IconType::Folder),
                    headline: rsx! { span { title: name.clone(), {name.clone()} } },
                    trailing: terminals.map(|count| rsx! {
                        Chip {
                            label: count.to_string(),
                            icon: Some(IconType::Terminal),
                            title: Some(format!("{count} terminal{}", if count == 1 { "" } else { "s" })),
                            small: true,
                        }
                    }),
                    selected: active_idx == index as i64,
                    test_id: Some("browse-row".to_owned()),
                    onclick: {
                        let drill = name.clone();
                        move |_| {
                            if pointer_type.peek().as_str() == "touch" {
                                on_drill.call(drill.clone());
                            } else {
                                on_select.call(index as i64);
                            }
                        }
                    },
                }
            }
        });
    }
    let mut file_rows: Vec<Element> = Vec::new();
    if show_files {
        for entry in files.iter() {
            let name = entry.name.clone();
            // A listed file links through the same builder a printed path
            // does, so the two address one file the same way. A row with no
            // href beats a link to a route the viewer cannot read.
            let href = child_file_href(worker_os.as_deref(), &server_fp, &cwd, &name);
            let support = relative_entry_time(entry.mtime_ms, now_ms);
            let file_path = child_path(worker_os.as_deref(), &cwd, &name)
                .unwrap_or_else(|| format!("{cwd}/{name}"));
            // The Download button sits beside the row's anchor, not inside it:
            // a button nested in a link is not a separate control.
            file_rows.push(rsx! {
                div { class: "flex min-w-0 items-center gap-1",
                    div { class: "min-w-0 flex-1",
                        ListRow {
                            dense: true,
                            leading_icon: Some(IconType::FileText),
                            headline: rsx! { span { title: name.clone(), {name.clone()} } },
                            support: Some(rsx! { {support} }),
                            href,
                            test_id: Some("browse-file-row".to_owned()),
                        }
                    }
                    DownloadButton { worker_fp: server_fp.clone(), path: file_path, size: ButtonSize::IconSm }
                }
            });
        }
    }
    rsx! {
        div {
            class: "flex min-h-0 min-w-0 flex-1 flex-col gap-2 overflow-y-auto p-2 outline-none",
            id: RESULTS_ID,
            tabindex: "-1",
            match status {
                BrowseStatus::Loading => rsx! {
                    div { "data-testid": "browse-loading", role: "status", "aria-live": "polite", "aria-busy": "true",
                        div { "aria-hidden": "true",
                            List { layout: ListLayout::Grid,
                                for _ in 0..SKELETON_ROWS {
                                    ListRow { dense: true, headline: rsx! { Skeleton { class: "h-4 w-full" } } }
                                }
                            }
                        }
                        div { class: "px-2 py-1 text-sm text-muted-foreground", {loading_caption} }
                    }
                },
                BrowseStatus::Error => rsx! {
                    div { "data-testid": "browse-listing-error", role: "status", "aria-live": "polite",
                        EmptyState {
                            icon: IconType::CloudOff,
                            title: "Couldn't read this folder",
                            supporting: error_message.clone(),
                            action: rsx! {
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    "data-testid": "browse-retry",
                                    onclick: move |_| on_retry.call(()),
                                    "Retry"
                                }
                            },
                        }
                    }
                },
                BrowseStatus::Offline => rsx! {
                    div { "data-testid": "browse-offline",
                        EmptyState {
                            icon: IconType::CloudOff,
                            title: "Machine offline",
                            supporting: "Reconnect to this machine to browse its folders.",
                        }
                    }
                },
                BrowseStatus::Ready if nothing_to_show && filtering => rsx! {
                    div { "data-testid": "browse-no-matches",
                        EmptyState {
                            icon: IconType::SearchX,
                            title: "No matches",
                            supporting: format!(
                                "Nothing in this folder matches \u{201c}{}\u{201d}.",
                                filter.trim()
                            ),
                            action: rsx! {
                                Button {
                                    variant: ButtonVariant::Secondary,
                                    "data-testid": "browse-clear-filter",
                                    onclick: move |_| on_clear_filter.call(()),
                                    "Clear filter"
                                }
                            },
                        }
                    }
                },
                BrowseStatus::Ready if nothing_to_show => rsx! {
                    div { "data-testid": "browse-empty",
                        EmptyState {
                            icon: IconType::FolderOpen,
                            title: "Empty folder",
                            supporting: "No subfolders here. Create one, or open a terminal in this folder.",
                        }
                    }
                },
                BrowseStatus::Ready => rsx! {
                    if !folders.is_empty() {
                        List { layout: ListLayout::Grid, {rows.into_iter()} }
                    }
                    if show_files && !files.is_empty() {
                        List { layout: ListLayout::Stack, {file_rows.into_iter()} }
                    }
                },
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn is_double_click(event: &MouseEvent) -> bool {
    event
        .downcast::<web_sys::MouseEvent>()
        .is_some_and(|event| event.detail() == 2)
}

#[cfg(not(target_arch = "wasm32"))]
fn is_double_click(_: &MouseEvent) -> bool {
    false
}
