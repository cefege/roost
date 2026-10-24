//! New-folder prompt for the folder picker: the name field, the inline failure
//! the attempt produced, and Create/Cancel. Compact gets a phone-sized dialog,
//! desktop a centred dialog. The page owns the open/name/busy/error state,
//! the `FilesMkdir` call and the post-create navigation, and hands the field
//! its element id so its open path can focus it.
//!
//! Called by `browse::picker`. Ports
//! `apps/web/src/components/browse/NewFolderDialog.tsx` and the dialog half of
//! `browseNewFolder.ts`; the name rules are `store::folder_name_validation`'s.

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;
use dioxus::prelude::*;

use crate::components::browse::dom::NEW_FOLDER_ID;
use libero::components::{Button, Dialog, Flex, TextField};
use libero::hooks::{ModalScope, use_modal};

/// The dialog's headline.
pub const HEADLINE: &str = "New folder";

#[derive(Clone)]
struct NewFolderDialogArgs {
    name: String,
    busy: bool,
    error: Option<String>,
    target_path: String,
    compact: bool,
    on_name: EventHandler<String>,
    on_close: EventHandler<()>,
    on_create: EventHandler<()>,
}

/// The new-folder dialog.
#[allow(clippy::too_many_arguments)]
#[component]
pub fn NewFolderDialog(
    open: bool,
    name: String,
    busy: bool,
    error: Option<String>,
    target_path: String,
    compact: bool,
    on_name: EventHandler<String>,
    on_close: EventHandler<()>,
    on_create: EventHandler<()>,
) -> Element {
    let mut latest = use_signal(|| NewFolderDialogArgs {
        name: name.clone(),
        busy,
        error: error.clone(),
        target_path: target_path.clone(),
        compact,
        on_name,
        on_close,
        on_create,
    });
    use_effect(use_reactive(
        (
            &name,
            &busy,
            &error,
            &target_path,
            &compact,
            &on_name,
            &on_close,
            &on_create,
        ),
        move |(name, busy, error, target_path, compact, on_name, on_close, on_create)| {
            latest.set(NewFolderDialogArgs {
                name,
                busy,
                error,
                target_path,
                compact,
                on_name,
                on_close,
                on_create,
            });
        },
    ));
    let modal = use_modal({
        let latest = latest;
        move |_: ModalScope<()>| {
            let args = latest();
            let description = format!("Creates a folder in {}.", args.target_path);
            let create_disabled = args.busy || args.name.trim().is_empty();
            rsx! {
                Dialog {
                    title: HEADLINE,
                    full_screen: if args.compact { "phone" } else { "never" },
                    ondismiss: move |_| {
                        latest().on_close.call(());
                        true
                    },
                    "data-testid": "newfolder-dialog",
                    Flex { direction: "column", gap: "md",
                        TextField {
                            value: Some(args.name.clone()),
                            oninput: move |value: String| latest().on_name.call(value),
                            label: "Folder name",
                            description,
                            id: NEW_FOLDER_ID,
                            "data-testid": "newfolder-input",
                            status: args.error.as_deref(),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Named(NamedKey::Enter) {
                                    event.prevent_default();
                                    latest().on_create.call(());
                                }
                            },
                        }
                        Flex { direction: "row", justify: "end", gap: "sm",
                            Button {
                                variant: "outlined",
                                onclick: move |_| latest().on_close.call(()),
                                "Cancel"
                            }
                            Button {
                                variant: "filled",
                                "data-testid": "newfolder-confirm",
                                disabled: create_disabled,
                                onclick: move |_| latest().on_create.call(()),
                                if args.busy { "Creating\u{2026}" } else { "Create" }
                            }
                        }
                    }
                }
            }
        }
    });
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    use_effect(use_reactive((&open,), move |(open,)| {
        generation.borrow_mut().invalidate();
        if open {
            let token = generation.borrow_mut().begin();
            let generation = Rc::clone(&generation);
            modal.open().onresult(move |_| {
                generation.borrow_mut().finish(token);
            });
        } else {
            modal.close();
        }
    }));
    rsx! {}
}
