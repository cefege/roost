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
use libero::theme::Size;
use roost_client_core::store::browse_entries::{BrowseEntry, relative_entry_time};

use crate::components::browse::BrowseStatus;
use crate::components::browse::dom::RESULTS_ID;
use crate::components::download::DownloadButton;
use libero::components::{Badge, Box, Button, Flex, Pictogram, Skeleton, Text};
use libero::sx::sx;

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
    let nothing_to_show = folders.is_empty() && (!show_files || files.is_empty());
    let filtering = !filter.trim().is_empty();
    let results_sx = use_hook(|| {
        sx().display("flex")
            .min_height("0")
            .flex("1")
            .flex_direction("column")
            .gap("sm")
            .overflow_y("auto")
            .padding("sm")
            .outline("none")
    });
    let row_button_sx = use_hook(|| {
        sx().min_width("0")
            .justify_content("flex-start")
            .text_align("left")
    });
    let row_content_sx = use_hook(|| sx().width("100%").min_width("0"));
    let row_name_sx = use_hook(|| {
        sx().overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    let icon_sx = use_hook(|| sx().width("1rem").height("1rem").flex_shrink("0"));
    let file_row_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("xs")
    });
    let loading_sx = use_hook(|| {
        sx().display("grid").gap("sm").with(
            "grid-template-columns",
            "repeat(auto-fill, minmax(12rem, 1fr))",
        )
    });
    let skeleton_sx = use_hook(|| sx().height("2.5rem").width("100%"));
    let caption_sx = use_hook(|| {
        sx().padding_inline("sm")
            .padding_block("xs")
            .font_size("sm")
            .color("muted")
    });
    let notice_sx = use_hook(|| sx().padding_block("2rem").text_align("center"));
    let notice_icon_sx = use_hook(|| sx().width("2rem").height("2rem").color("muted"));
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
            Button {
                variant: "standard",
                full_width: true,
                selected: active_idx == index as i64,
                sx: row_button_sx.clone(),
                "data-testid": "browse-row",
                onclick: {
                    let drill = name.clone();
                    move |_| on_drill.call(drill.clone())
                },
                Flex { direction: "row", align: "center", gap: "sm", sx: row_content_sx.clone(),
                    Box { component: "span", sx: icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::folder::outlined, width: "1rem", height: "1rem" } }
                    Flex { direction: "column", sx: row_content_sx.clone(),
                        Text { size: "sm", sx: row_name_sx.clone(), title: name.clone(), "{name}" }
                        Text { size: "xs", color: "muted", "{support}" }
                    }
                    if let Some(count) = terminals {
                        Badge { size: "sm", variant: "tonal", title: format!("{count} terminals"),
                            Pictogram { icon: pictogram_icons_lucide::terminal::outlined, width: "0.75rem", height: "0.75rem" }
                            "{count}"
                        }
                    }
                }
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
                Flex { direction: "row", align: "center", gap: "xs", sx: file_row_sx.clone(),
                    if let Some(href) = href {
                        Button {
                            variant: "standard",
                            full_width: true,
                            to: href,
                            sx: row_button_sx.clone(),
                            "data-testid": "browse-file-row",
                            Flex { direction: "row", align: "center", gap: "sm", sx: row_content_sx.clone(),
                                Box { component: "span", sx: icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::file_text::outlined, width: "1rem", height: "1rem" } }
                                Flex { direction: "column", sx: row_content_sx.clone(),
                                    Text { size: "sm", sx: row_name_sx.clone(), title: name.clone(), "{name}" }
                                    Text { size: "xs", color: "muted", "{support}" }
                                }
                            }
                        }
                    } else {
                        Box { component: "span", sx: row_button_sx.clone(), "data-testid": "browse-file-row",
                            Flex { direction: "row", align: "center", gap: "sm", sx: row_content_sx.clone(),
                                Box { component: "span", sx: icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::file_text::outlined, width: "1rem", height: "1rem" } }
                                Flex { direction: "column", sx: row_content_sx.clone(),
                                    Text { size: "sm", sx: row_name_sx.clone(), title: name.clone(), "{name}" }
                                    Text { size: "xs", color: "muted", "{support}" }
                                }
                            }
                        }
                    }
                    DownloadButton { worker_fp: server_fp.clone(), path: file_path, size: Size::Sm }
                }
            });
        }
    }
    rsx! {
        Flex {
            direction: "column",
            gap: "sm",
            sx: results_sx,
            id: RESULTS_ID,
            tabindex: "-1",
            if let Some(header) = header {
                {header}
            }
            match status {
                BrowseStatus::Loading => rsx! {
                    Box { "data-testid": "browse-loading", role: "status", "aria-live": "polite", "aria-busy": "true",
                        Box { "aria-hidden": "true",
                            Box { sx: loading_sx.clone(),
                                for _ in 0..SKELETON_ROWS {
                                    Skeleton { sx: skeleton_sx.clone() }
                                }
                            }
                        }
                        Box { component: "span", sx: caption_sx.clone(), {loading_caption} }
                    }
                },
                BrowseStatus::Error => rsx! {
                    Box { "data-testid": "browse-listing-error", role: "status", "aria-live": "polite",
                        Flex { direction: "column", align: "center", gap: "sm", sx: notice_sx.clone(),
                            Box { component: "span", sx: notice_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::cloud_off::outlined, width: "2rem", height: "2rem" } }
                            Text { size: "lg", "Couldn't read this folder" }
                            if let Some(message) = error_message.clone() {
                                Text { size: "sm", color: "muted", "{message}" }
                            }
                            Button {
                                variant: "outlined",
                                "data-testid": "browse-retry",
                                onclick: move |_| on_retry.call(()),
                                "Retry"
                            }
                        }
                    }
                },
                BrowseStatus::Offline => rsx! {
                    Box { "data-testid": "browse-offline",
                        Flex { direction: "column", align: "center", gap: "sm", sx: notice_sx.clone(),
                            Box { component: "span", sx: notice_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::cloud_off::outlined, width: "2rem", height: "2rem" } }
                            Text { size: "lg", "Machine offline" }
                            Text { size: "sm", color: "muted", "Reconnect to this machine to browse its folders." }
                        }
                    }
                },
                BrowseStatus::Ready if nothing_to_show && filtering => rsx! {
                    Box { "data-testid": "browse-no-matches",
                        Flex { direction: "column", align: "center", gap: "sm", sx: notice_sx.clone(),
                            Box { component: "span", sx: notice_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::search_x::outlined, width: "2rem", height: "2rem" } }
                            Text { size: "lg", "No matches" }
                            Text { size: "sm", color: "muted",
                                "Nothing in this folder matches \u{201c}{filter.trim()}\u{201d}."
                            }
                            Button {
                                variant: "outlined",
                                "data-testid": "browse-clear-filter",
                                onclick: move |_| on_clear_filter.call(()),
                                "Clear filter"
                            }
                        }
                    }
                },
                BrowseStatus::Ready if nothing_to_show => rsx! {
                    Box { "data-testid": "browse-empty",
                        Flex { direction: "column", align: "center", gap: "sm", sx: notice_sx.clone(),
                            Box { component: "span", sx: notice_icon_sx.clone(), Pictogram { icon: pictogram_icons_lucide::folder_open::outlined, width: "2rem", height: "2rem" } }
                            Text { size: "lg", "Empty folder" }
                            Text { size: "sm", color: "muted", "No subfolders here. Create one, or open a terminal in this folder." }
                        }
                    }
                },
                BrowseStatus::Ready => rsx! {
                    Box { sx: loading_sx.clone(), {rows.into_iter()} }
                },
            }
        }
    }
}
