//! The folder picker's content region: a dense responsive grid of folder and
//! file entries plus every state that region can show — loading skeletons, a
//! named listing failure, an offline machine, an empty folder, and a filter that
//! matched nothing. The page owns the listing, the filter, the terminal-count
//! subtitles and the keyboard cursor; this file paints them.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/BrowseEntryList.tsx`; the row filter and the
//! relative-time text are `store::browse_entries`'s.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_client_core::store::browse_entries::{BrowseEntry, relative_entry_time};

use crate::components::browse::BrowseStatus;
use crate::components::browse::dom::RESULTS_ID;
use crate::components::download::DownloadButton;
use crate::components::md::{
    Button, ButtonVariant, Chip, EmptyState, IconButtonSize, List, ListLayout, ListRow, Skeleton,
};
use crate::platform::worker_paths::palette::child_path;
use crate::terminal_href::child_file_href;

/// The placeholder rows a loading directory paints, one per grid line.
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
    /// Content that scrolls WITH the entries, so the picker keeps no permanent
    /// chrome above the grid.
    header: Option<Element>,
    on_drill: EventHandler<String>,
    on_clear_filter: EventHandler<()>,
    on_retry: EventHandler<()>,
) -> Element {
    let now_ms = now_ms;
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
        let support = relative_entry_time(entry.mtime_ms, now_ms);
        rows.push(rsx! {
            ListRow {
                dense: true,
                leading_icon: Some("folder".to_owned()),
                headline: rsx! { span { title: name.clone(), {name.clone()} } },
                support: Some(rsx! { {support} }),
                trailing: terminals.map(|count| rsx! {
                    Chip {
                        label: count.to_string(),
                        icon: Some("terminal".to_owned()),
                        title: Some(format!("{count} terminal{}", if count == 1 { "" } else { "s" })),
                        small: true,
                    }
                }),
                selected: active_idx == index as i64,
                test_id: Some("browse-row".to_owned()),
                onclick: {
                    let drill = name.clone();
                    move |_| on_drill.call(drill.clone())
                },
            }
        });
    }
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
            rows.push(rsx! {
                div { style: "display: flex; align-items: center; gap: var(--md-space-1); min-width: 0;",
                    div { style: "flex: 1 1 auto; min-width: 0;",
                        ListRow {
                            dense: true,
                            leading_icon: Some("description".to_owned()),
                            headline: rsx! { span { title: name.clone(), {name.clone()} } },
                            support: Some(rsx! { {support} }),
                            href,
                            test_id: Some("browse-file-row".to_owned()),
                        }
                    }
                    DownloadButton { worker_fp: server_fp.clone(), path: file_path, size: IconButtonSize::IconSm }
                }
            });
        }
    }
    rsx! {
        div { class: "df-browse-area", id: RESULTS_ID, tabindex: "-1",
            if let Some(header) = header {
                {header}
            }
            match status {
                BrowseStatus::Loading => rsx! {
                    div { "data-testid": "browse-loading", role: "status", "aria-live": "polite", "aria-busy": "true",
                        div { "aria-hidden": "true",
                            List { layout: ListLayout::Grid,
                                for _ in 0..SKELETON_ROWS {
                                    ListRow { dense: true, headline: rsx! { Skeleton {} } }
                                }
                            }
                        }
                        div { class: "df-browse-loading-caption md-label-s", {loading_caption} }
                    }
                },
                BrowseStatus::Error => rsx! {
                    div { "data-testid": "browse-listing-error", role: "status", "aria-live": "polite",
                        EmptyState {
                            icon: "cloud_off".to_owned(),
                            title: "Couldn't read this folder".to_owned(),
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
                            icon: "cloud_off".to_owned(),
                            title: "Machine offline".to_owned(),
                            supporting: Some("Reconnect to this machine to browse its folders.".to_owned()),
                        }
                    }
                },
                BrowseStatus::Ready if nothing_to_show && filtering => rsx! {
                    div { "data-testid": "browse-no-matches",
                        EmptyState {
                            icon: "search_off".to_owned(),
                            title: "No matches".to_owned(),
                            supporting: Some(format!(
                                "Nothing in this folder matches \u{201c}{}\u{201d}.",
                                filter.trim()
                            )),
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
                            icon: "folder_open".to_owned(),
                            title: "Empty folder".to_owned(),
                            supporting: Some(
                                "No subfolders here. Create one, or open a terminal in this folder."
                                    .to_owned()
                            ),
                        }
                    }
                },
                BrowseStatus::Ready => rsx! {
                    List { layout: ListLayout::Grid, {rows.into_iter()} }
                },
            }
        }
    }
}
