//! The Shift+? shortcut overlay: every row of `help::shortcuts` grouped by
//! category, a text filter over it, and the "Copy diagnostic" button. Ports
//! `apps/web/src/components/palette/HelpOverlay.tsx`; mounted by
//! `HelpSurface`, so the open flag in `keyboard_shortcuts` is the only state.
//!
//! Nothing here reads the store: the overlay is a document about the keyboard,
//! and a row that depended on a WebSocket tick would repaint for no reason.

use dioxus::prelude::*;

use super::shortcuts::{ShortcutEntry, filtered_shortcuts, grouped_shortcuts, reader_platform};
use crate::components::notifications::clipboard::copy_text;
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
    let on_copy = EventHandler::new(|()| copy_diagnostic());
    rsx! {
        div { class: "flex min-h-0 flex-1 flex-col", "data-testid": "help-overlay",
            div { class: "flex flex-none justify-end px-5 pt-2",
                Button {
                    variant: ButtonVariant::Secondary,
                    onclick: move |_| on_copy.call(()),
                    "data-testid": "help-overlay-copy-diagnostic",
                    "Copy diagnostic"
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

/// Copy where the reader is, what they are reading it in, and when — the three
/// fields a bug report needs and the only ones that carry no credential.
///
/// The payload is the error boundary's writer, so one JSON shape is pasted out
/// of the app wherever it comes from; the two fields this surface has nothing
/// to say about are written empty.
fn copy_diagnostic() {
    #[cfg(target_arch = "wasm32")]
    {
        let payload = {
            let Some(window) = web_sys::window() else {
                return;
            };
            let location = window.location();
            let url = crate::platform::fragment_credential::credential_free_url(
                &location.pathname().unwrap_or_default(),
                &location.search().unwrap_or_default(),
                &location.hash().unwrap_or_default(),
            );
            let agent = window.navigator().user_agent().unwrap_or_default();
            let time = js_sys::Date::new_0()
                .to_iso_string()
                .as_string()
                .unwrap_or_default();
            crate::components::app_error_boundary::diagnostic_payload(&url, &agent, &time, "")
        };
        // A refusal is not an error this surface renders: the reader can still
        // read the same three facts off their own address bar.
        let _ = copy_text(&payload);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = copy_text;
}
