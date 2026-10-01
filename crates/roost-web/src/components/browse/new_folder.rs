//! New-folder prompt for the folder picker: the name field, the inline failure
//! the attempt produced, and Create/Cancel. Compact gets the app's standard
//! bottom sheet, desktop the centred dialog. The page owns the open/name/busy/
//! error state, the `FilesMkdir` call and the post-create navigation, and hands
//! the field its element id so its open path can focus it.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/NewFolderDialog.tsx` and the dialog half of
//! `browseNewFolder.ts`; the name rules are `store::folder_name_validation`'s.

use dioxus::prelude::*;

use crate::components::browse::dom::NEW_FOLDER_ID;
use crate::components::md::{Button, ButtonVariant, Dialog, TextField};

/// The dialog's headline.
pub const HEADLINE: &str = "New folder";

/// The new-folder dialog.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn NewFolderDialog(
    /// Whether the dialog is showing.
    open: bool,
    /// What the reader has typed.
    name: String,
    /// Whether a create is in flight.
    busy: bool,
    /// The validation or machine failure for this attempt, shown in place.
    error: Option<String>,
    /// The directory the folder lands in, quoted in the field's description.
    target_path: String,
    /// Whether the surface is a phone, which takes the bottom sheet.
    compact: bool,
    on_name: EventHandler<String>,
    on_close: EventHandler<()>,
    on_create: EventHandler<()>,
) -> Element {
    let description = format!("Creates a folder in {target_path}.");
    let create_disabled = busy || name.trim().is_empty();
    rsx! {
        Dialog {
            open,
            on_close,
            headline: Some(HEADLINE.to_owned()),
            class: compact.then(|| "roost-sheet--bottom".to_owned()),
            actions: rsx! {
                Button { variant: ButtonVariant::Outline, onclick: move |_| on_close.call(()), "Cancel" }
                Button {
                    variant: ButtonVariant::Default,
                    "data-testid": "newfolder-confirm",
                    disabled: create_disabled,
                    onclick: move |_| on_create.call(()),
                    if busy { "Creating\u{2026}" } else { "Create" }
                }
            },
            TextField {
                value: name.clone(),
                on_input: move |value: String| on_name.call(value),
                label: Some("Folder name".to_owned()),
                test_id: Some("newfolder-input".to_owned()),
                id: Some(NEW_FOLDER_ID.to_owned()),
                description: rsx! { {description} },
                error: error.clone().map(|message| rsx! { span { role: "alert", {message} } }),
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter {
                        event.prevent_default();
                        on_create.call(());
                    }
                },
            }
        }
    }
}
