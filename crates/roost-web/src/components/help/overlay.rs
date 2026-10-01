//! The Shift+? shortcut overlay: every row of `help::shortcuts` grouped by
//! category, a text filter over it, and the "Copy diagnostic" button. Ports
//! `apps/web/src/components/palette/HelpOverlay.tsx`; mounted by
//! `HelpSurface`, so the open flag in `keyboard_shortcuts` is the only state.
//!
//! Nothing here reads the store: the overlay is a document about the keyboard,
//! and a row that depended on a WebSocket tick would repaint for no reason.

use dioxus::prelude::*;

use super::shortcuts::{ShortcutEntry, filtered_shortcuts, grouped_shortcuts, reader_platform};
use crate::components::md::{
    BindingChip, Button, ButtonVariant, List, ListRow, SectionTitle, Sheet, SheetSide, TextField,
};
use crate::components::notifications::clipboard::copy_text;
use crate::components::palette::dom::focus_by_test_id_next_frame;

/// The Shift+? modal, rendered by the route surface that owns it.
#[component]
pub fn HelpOverlayHost() -> Element {
    let overlays = crate::keyboard_shortcuts::use_shortcut_overlays();
    // Read THROUGH the flag rather than `peek`ing it: this host exists only to
    // appear when the shortcut sets the flag, and `peek` is a promise not to
    // subscribe, so the host rendered once closed and never came back.
    let open = overlays.help.cloned();
    let on_close = {
        let mut help = overlays.help;
        EventHandler::new(move |()| help.set(false))
    };
    rsx! {
        Sheet {
            open,
            on_close,
            headline: "Keyboard shortcuts",
            side: SheetSide::Center,
            class: "roost-dialog--wide roost-dialog--help",
            if open {
                HelpOverlayBody {}
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
    let on_input = move |value: String| filter.set(value);
    let on_copy = EventHandler::new(|()| copy_diagnostic());
    rsx! {
        div { class: "roost-help-overlay", "data-testid": "help-overlay",
            div { class: "roost-help-overlay__tools",
                Button {
                    variant: ButtonVariant::Secondary,
                    onclick: move |_| on_copy.call(()),
                    "data-testid": "help-overlay-copy-diagnostic",
                    "Copy diagnostic"
                }
            }
            div { style: "padding: var(--md-space-3) var(--md-space-5) var(--md-space-2);",
                TextField {
                    value: filter(),
                    on_input,
                    placeholder: "Filter shortcuts…",
                    aria_label: "Filter shortcuts",
                    test_id: "help-overlay-filter",
                    style: "inline-size: 100%;",
                }
            }
            div { style: "flex: 1 1 auto; min-block-size: 0; overflow-y: auto; padding: var(--md-space-2) var(--md-space-5) var(--md-space-4);",
                if groups.is_empty() {
                    p { class: "md-body-s", style: "color: var(--text-lo); margin-block-start: var(--md-space-2);",
                        "No matches."
                    }
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
        section { style: "margin-block-end: var(--md-space-4);",
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
        div { class: "roost-help-overlay__action", "data-testid": "help-overlay-action", "data-action-id": action_id,
            ListRow {
                headline: rsx! { span { style: "color: var(--text-hi);", {entry.label} } },
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
