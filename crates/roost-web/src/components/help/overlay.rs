//! The Shift+? shortcut overlay: every row of `help::shortcuts` grouped by
//! category, a text filter over it, and the "Copy diagnostic" button. Ports
//! `apps/web/src/components/palette/HelpOverlay.tsx`; mounted by
//! `HelpSurface`, so the open flag in `keyboard_shortcuts` is the only state.
//!
//! Nothing here reads the store: the overlay is a document about the keyboard,
//! and a row that depended on a WebSocket tick would repaint for no reason.

use dioxus::prelude::*;

use super::shortcuts::{ShortcutEntry, filtered_shortcuts, grouped_shortcuts, reader_platform};
use crate::components::app_error_boundary::diagnostic;
use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::palette::dom::focus_by_test_id_next_frame;
use crate::components::ui::{
    BindingChip, Button, ButtonVariant, Dialog, DialogContent, DialogHeader, DialogTitle, Input,
    List, ListRow, SectionTitle,
};

/// The Shift+? modal, rendered by the route surface that owns it.
#[component]
pub fn HelpOverlayHost() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    // Read THROUGH the flag rather than `peek`ing it: this host exists only to
    // appear when the shortcut sets the flag, and `peek` is a promise not to
    // subscribe, so the host rendered once closed and never came back.
    let open = overlays.help.cloned();
    let on_open_change = {
        let mut help = overlays.help;
        EventHandler::new(move |is_open: bool| help.set(is_open))
    };
    rsx! {
        Dialog { open, on_open_change,
            DialogContent {
                class: "flex sm:max-w-[var(--roost-dialog-wide-max-inline-size)] flex-col gap-0 overflow-hidden p-0",
                DialogHeader { class: "px-5 pt-5 text-start", DialogTitle { "Keyboard shortcuts" } }
                if open {
                    HelpOverlayBody {}
                }
            }
        }
    }
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
    rsx! {
        div { class: "flex min-h-0 flex-1 flex-col", "data-testid": "help-overlay",
            div { class: "flex flex-none justify-end px-5 pt-2",
                Button {
                    variant: ButtonVariant::Secondary,
                    // The error boundary's payload, so one JSON shape leaves the
                    // app wherever it is copied from; here it carries no error.
                    onclick: move |_| copy_to_clipboard(&diagnostic("")),
                    "data-testid": "help-overlay-copy-diagnostic",
                    if copied() { "Copied" } else { "Copy diagnostic" }
                }
            }
            div { class: "flex-none px-5 pt-3 pb-2",
                Input {
                    class: "w-full",
                    value: filter(),
                    oninput: move |event: FormEvent| filter.set(event.value()),
                    placeholder: "Filter shortcuts…",
                    aria_label: "Filter shortcuts",
                    test_id: "help-overlay-filter",
                }
            }
            div { class: "min-h-0 flex-1 overflow-y-auto px-5 pt-2 pb-4",
                if groups.is_empty() {
                    p { class: "mt-2 text-sm text-muted-foreground", "No matches." }
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
    rsx! {
        section { class: "mb-4",
            SectionTitle { {category} }
            List {
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
    let action_id = entry.action_id();
    let binding = entry.binding_label(platform);
    rsx! {
        // The `data-action-id` a caller addresses a row by is not a native list
        // row attribute, so it rides on the wrapper that owns the row's identity.
        div { "data-testid": "help-overlay-action", "data-action-id": action_id,
            ListRow {
                headline: rsx! { span { class: "text-foreground", {entry.label} } },
                trailing: rsx! { BindingChip { {binding} } },
            }
        }
    }
}
