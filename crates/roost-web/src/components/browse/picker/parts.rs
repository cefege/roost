//! Browser-history navigation and remote folder creation.
//! Called by `browse::picker`; validates names before issuing a create request.
//! The store owns navigation and the coordinator relays filesystem operations.

use dioxus::prelude::*;
use roost_client_core::store::browse_state::intent::BrowseIntent;
use roost_client_core::store::folder_name_validation::validate_new_folder_name;

use crate::components::browse::listing;
use crate::components::browse::picker::NewFolderForm;
use crate::platform::worker_paths::palette::child_path;
use crate::pump::Pump;

/// Back or Forward, whichever `back` names.
#[must_use]
pub fn history_move(
    pump: &Pump,
    worker_fp: &str,
    ticket: &mut Signal<u64>,
    back: bool,
) -> EventHandler<()> {
    let pump = pump.clone();
    let worker_fp = worker_fp.to_owned();
    let mut ticket = *ticket;
    EventHandler::new(move |()| {
        let intent = if back {
            BrowseIntent::GoBack {
                worker_fp: worker_fp.clone(),
            }
        } else {
            BrowseIntent::GoForward {
                worker_fp: worker_fp.clone(),
            }
        };
        listing::apply(&pump, &mut ticket, intent);
    })
}

/// Validate a new folder's name, and ask the machine for it when the name
/// stands. The name is checked HERE rather than after the call so a rejected
/// name never leaves the browser, and the failure is named in the dialog rather
/// than raised as a card the reader has to find again.
pub fn commit_new_folder(
    pump: &Pump,
    worker_fp: &str,
    worker_os: Option<&str>,
    parent: &str,
    siblings: &[String],
    form: Signal<NewFolderForm>,
    ticket: &mut Signal<u64>,
) {
    let current = form.peek().clone();
    if current.busy {
        return;
    }
    let sibling_refs: Vec<&str> = siblings.iter().map(String::as_str).collect();
    let mut settled = form;
    if let Err(message) = validate_new_folder_name(&current.name, &sibling_refs) {
        let mut next = current;
        next.error = Some(message);
        settled.set(next);
        return;
    }
    let Some(target) = child_path(worker_os, parent, current.name.trim()) else {
        let mut next = current;
        next.error = Some("Enter a folder name.".to_owned());
        settled.set(next);
        return;
    };
    let busy = NewFolderForm {
        busy: true,
        error: None,
        ..current
    };
    settled.set(busy);
    let settle = {
        let pump = pump.clone();
        let worker_fp = worker_fp.to_owned();
        let mut ticket = *ticket;
        move |outcome: Result<String, String>| match outcome {
            Ok(landed) => {
                settled.set(NewFolderForm::default());
                listing::apply(
                    &pump,
                    &mut ticket,
                    BrowseIntent::Navigate {
                        worker_fp: worker_fp.clone(),
                        path: landed,
                    },
                );
            }
            Err(message) => {
                let mut next = settled.peek().clone();
                next.busy = false;
                next.error = Some(message);
                settled.set(next);
            }
        }
    };
    listing::create_folder(
        pump.clone(),
        worker_fp.to_owned(),
        target,
        EventHandler::new(settle),
    );
}
