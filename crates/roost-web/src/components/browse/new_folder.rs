//! New-folder prompt for the folder picker: the name field, the inline failure
//! the attempt produced, and Create/Cancel. Compact gets the bottom sheet,
//! desktop the centred dialog. The page owns the open/name/busy/error state,
//! the `FilesMkdir` call and the post-create navigation, and hands the field
//! its element id so its open path can focus it.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/NewFolderDialog.tsx` and the dialog half of
//! `browseNewFolder.ts`; the name rules are `store::folder_name_validation`'s.

use dioxus::prelude::*;

use crate::components::browse::dom::NEW_FOLDER_ID;
use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader,
    DialogTitle, Sheet, SheetContent, SheetSide, TextField,
};

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
    let on_open_change = move |still_open: bool| {
        if !still_open {
            on_close.call(());
        }
    };
    let body = rsx! {
        NewFolderBody {
            name,
            busy,
            error,
            target_path,
            on_name,
            on_close,
            on_create,
        }
    };
    if !open {
        return rsx! {};
    }
    if compact {
        return rsx! {
            Sheet { open: true, on_open_change,
                SheetContent { side: SheetSide::Bottom, test_id: "newfolder-dialog",
                    {body}
                }
            }
        };
    }
    rsx! {
        Dialog { open: true, on_open_change,
            DialogContent { test_id: "newfolder-dialog",
                {body}
            }
        }
    }
}

/// The header, field and footer both presentations share.
#[component]
fn NewFolderBody(
    name: String,
    busy: bool,
    error: Option<String>,
    target_path: String,
    on_name: EventHandler<String>,
    on_close: EventHandler<()>,
    on_create: EventHandler<()>,
) -> Element {
    let description = format!("Creates a folder in {target_path}.");
    let create_disabled = busy || name.trim().is_empty();
    let error_element = error.map(|message| rsx! { {message} });
    rsx! {
        DialogHeader {
            DialogTitle { {HEADLINE} }
        }
        DialogBody {
            TextField {
                value: name,
                on_input: move |value: String| on_name.call(value),
                label: Some("Folder name".to_owned()),
                test_id: Some("newfolder-input".to_owned()),
                id: Some(NEW_FOLDER_ID.to_owned()),
                description: rsx! { {description} },
                error: error_element,
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Named(NamedKey::Enter) {
                        event.prevent_default();
                        on_create.call(());
                    }
                },
            }
        }
        DialogFooter {
            Button {
                variant: ButtonVariant::Outline,
                onclick: move |_| on_close.call(()),
                "Cancel"
            }
            Button {
                variant: ButtonVariant::Default,
                "data-testid": "newfolder-confirm",
                disabled: create_disabled,
                onclick: move |_| on_create.call(()),
                if busy { "Creating\u{2026}" } else { "Create" }
            }
        }
    }
}
