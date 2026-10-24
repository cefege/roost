//! Clipboard history content passed explicitly into Libero's modal outlet.
//! Rows keep the host's selected entry and copy/paste/delete command ownership.
//! No store or terminal context is resolved from this portaled subtree.

use super::EntryRow;
use dioxus::prelude::*;
use libero::{
    components::{ActionIcon, Button, Flex, Paper, Pictogram, Text, Title},
    sx::sx,
};

#[component]
pub(super) fn ClipboardContent(
    rows: Vec<EntryRow>,
    selected: usize,
    on_keydown: EventHandler<KeyboardEvent>,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<String>,
    on_delete: EventHandler<String>,
    on_clear: EventHandler<()>,
) -> Element {
    rsx! {
        Flex { direction: "column", gap: "xs", tabindex: "0",
            "data-testid": "clipboard-history-list", onkeydown: on_keydown,
            if rows.is_empty() {
                Flex { direction: "column", gap: "sm", align: "center",
                    Pictogram { icon: pictogram_icons_lucide::clipboard_paste::outlined, width: "24", height: "24" }
                    Title { component: "h3", size: "sm", "Nothing copied yet" }
                    Text { size: "sm", color: "muted",
                        "Text you copy in any Roost terminal, on any device, appears here."
                    }
                }
            } else {
                Flex { direction: "row", justify: "end",
                    Button { variant: "standard", onclick: move |_| on_clear.call(()), "Clear all" }
                }
                for (index, row) in rows.into_iter().enumerate() {
                    ClipboardRow { key: "{row.id}", row, selected: index == selected,
                        on_copy, on_paste, on_delete,
                    }
                }
            }
        }
    }
}

#[component]
fn ClipboardRow(
    row: EntryRow,
    selected: bool,
    on_copy: EventHandler<String>,
    on_paste: EventHandler<String>,
    on_delete: EventHandler<String>,
) -> Element {
    let text_sx = use_hook(|| sx().min_width("0").flex("1"));
    let preview_sx = use_hook(|| {
        sx().margin("0")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .with("font-family", "var(--term-font-family)")
    });
    let row_sx = use_hook(|| sx().padding("xs"));
    let copy_text = row.text.clone();
    let paste_text = row.text.clone();
    let delete_id = row.id.clone();
    rsx! {
        Paper { bordered: true, color: if selected { "primary" } else { "surface" },
            sx: row_sx, "data-selected": selected,
            Flex { direction: "row", align: "center", gap: "xs", wrap: false,
                Flex { direction: "column", gap: "xs", sx: text_sx,
                    Text { component: "pre", size: "xs", sx: preview_sx, "{row.preview}" }
                    Text { size: "xs", color: "muted", "{row.source}" }
                }
                ActionIcon { icon: pictogram_icons_lucide::copy::outlined, aria_label: "Copy", variant: "standard",
                    onclick: move |_| on_copy.call(copy_text.clone()),
                }
                ActionIcon { icon: pictogram_icons_lucide::corner_down_left::outlined,
                    aria_label: "Paste into the terminal", variant: "standard",
                    onclick: move |_| on_paste.call(paste_text.clone()),
                }
                ActionIcon { icon: pictogram_icons_lucide::trash_2::outlined, aria_label: "Delete", variant: "standard",
                    onclick: move |_| on_delete.call(delete_id.clone()),
                }
            }
        }
    }
}
