//! The Shift+? shortcut overlay: every row of `help::shortcuts` grouped by
//! category, a text filter over it, and the "Copy diagnostic" button. Ports
//! `apps/web/src/components/palette/HelpOverlay.tsx`; mounted by
//! `HelpSurface`, so the open flag in `keyboard_shortcuts` is the only state.
//!
//! Nothing here reads the store: the overlay is a document about the keyboard,
//! and a row that depended on a WebSocket tick would repaint for no reason.

use dioxus::prelude::*;
use libero::components::{Box, Button, Dialog, List, ListItem, ListSubheader, TextField};
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;

use super::shortcuts::{ShortcutEntry, filtered_shortcuts, grouped_shortcuts, reader_platform};
use crate::components::app_error_boundary::diagnostic;
use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::palette::dom::focus_by_test_id_next_frame;

/// The Shift+? modal, rendered by the route surface that owns it.
#[component]
pub fn HelpOverlayHost() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    let help = overlays.help;
    let open = help.cloned();
    let dialog_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("0")
            .overflow("hidden")
            .padding("0")
            .media(
                "(min-width: 640px)",
                sx().max_width("var(--roost-dialog-wide-max-inline-size)"),
            )
    });
    let modal = use_modal(move |_: ModalScope<()>| {
        let mut help = help;
        rsx! {
            Dialog {
                title: "Keyboard shortcuts",
                sx: dialog_sx.clone(),
                ondismiss: move |_| {
                    help.set(false);
                    true
                },
                HelpOverlayBody {}
            }
        }
    });
    use_effect(use_reactive((&open,), move |(is_open,)| {
        if is_open && !modal.is_open() {
            modal.open();
        } else if !is_open && modal.is_open() {
            modal.close();
        }
    }));
    rsx! {}
}

/// The filterable catalogue. Mounted only while the overlay is open, so the
/// filter starts empty and the field takes focus without any reset bookkeeping.
#[component]
fn HelpOverlayBody() -> Element {
    use_hook(|| focus_by_test_id_next_frame("help-overlay-filter"));
    let mut filter = use_signal(String::new);
    let platform = reader_platform();
    let groups = grouped_shortcuts(&filtered_shortcuts(&filter(), platform));
    let (copy_to_clipboard, copied) = use_copy_clipboard(None);
    let filter_sx = use_hook(|| sx().width("100%"));
    let overlay_root_sx = use_hook(|| {
        sx().display("flex")
            .min_height("0")
            .flex("1")
            .flex_direction("column")
    });
    let action_bar_sx = use_hook(|| {
        sx().display("flex")
            .justify_content("flex-end")
            .padding_inline("1.25rem")
            .padding_top("1.25rem")
    });
    let filter_frame_sx = use_hook(|| {
        sx().padding_inline("1.25rem")
            .padding_top("0.75rem")
            .padding_bottom("0.5rem")
    });
    let shortcut_list_sx = use_hook(|| {
        sx().min_height("0")
            .flex("1")
            .overflow_y("auto")
            .padding_inline("1.25rem")
            .padding_top("0.5rem")
            .padding_bottom("1rem")
    });
    let empty_filter_sx = use_hook(|| {
        sx().margin_top("0.5rem")
            .font_size("sm")
            .color("var(--muted-foreground)")
    });
    rsx! {
        Box { component: "div", sx: overlay_root_sx.clone(), "data-testid": "help-overlay",
            Box { component: "div", sx: action_bar_sx.clone(),
                Button {
                    variant: "outlined",
                    onclick: move |_| copy_to_clipboard(&diagnostic("")),
                    "data-testid": "help-overlay-copy-diagnostic",
                    if copied() { "Copied" } else { "Copy diagnostic" }
                }
            }
            Box { component: "div", sx: filter_frame_sx.clone(),
                TextField {
                    sx: filter_sx.clone(),
                    value: filter(),
                    oninput: move |value: String| filter.set(value),
                    placeholder: "Filter shortcuts…",
                    aria_label: "Filter shortcuts",
                    id: "help-overlay-filter",
                    "data-testid": "help-overlay-filter",
                }
            }
            Box { component: "div", sx: shortcut_list_sx.clone(),
                if groups.is_empty() {
                    Box { component: "p", sx: empty_filter_sx.clone(), "No matches." }
                }
                for (category, entries) in groups {
                    ShortcutGroup { category, entries, platform }
                }
            }
        }
    }
}

/// One category: its caps label over the rows it owns.
#[component]
fn ShortcutGroup(
    category: &'static str,
    entries: Vec<ShortcutEntry>,
    platform: crate::platform::browser_platform::BrowserPlatform,
) -> Element {
    let group_sx = use_hook(|| sx().margin_bottom("1rem"));
    rsx! {
        Box { component: "section", sx: group_sx.clone(),
            List { "aria-label": category,
                ListSubheader { {category} }
                for entry in entries {
                    ShortcutRow { entry, platform }
                }
            }
        }
    }
}

/// One row: the action on the left, its key cap on the right.
#[component]
fn ShortcutRow(
    entry: ShortcutEntry,
    platform: crate::platform::browser_platform::BrowserPlatform,
) -> Element {
    let keycap_sx = use_hook(|| {
        sx().border_radius("sm")
            .border_width("1px")
            .border_color("var(--border)")
            .background_color("var(--muted)")
            .padding_inline("sm")
            .padding_block("xs")
            .font_family("monospace")
            .font_size("xs")
    });
    let row_sx = use_hook(|| {
        sx().display("flex")
            .width("100%")
            .align_items("center")
            .justify_content("space-between")
            .gap("md")
    });
    let label_sx = use_hook(|| sx().color("var(--foreground)"));
    let action_id = entry.action_id();
    let binding = entry.binding_label(platform);
    rsx! {
        ListItem { "data-testid": "help-overlay-action", "data-action-id": action_id,
            Box { component: "div", sx: row_sx.clone(),
                Box { component: "span", sx: label_sx.clone(), {entry.label} }
                Box { component: "kbd", sx: keycap_sx.clone(),
                    {binding}
                }
            }
        }
    }
}
