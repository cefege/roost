//! The page's four side questions, kept beside it for the line cap: where a
//! floating menu anchors, what the recents strip offers, what Back and Forward
//! ask the store for, and what a Create press commits.
//!
//! Called by `browse::picker`. Depends on the store, the path codec and the
//! coordinator; it holds no state of its own.

use dioxus::prelude::*;
use roost_client_core::store::browse_state::intent::BrowseIntent;
use roost_client_core::store::folder_name_validation::validate_new_folder_name;

use crate::components::browse::BrowseView;
use crate::components::browse::dom;
use crate::components::browse::listing;
use crate::components::browse::path_bar::CrumbMenuPos;
use crate::components::browse::picker::NewFolderForm;
use crate::components::context_menu::{AnchoredMenuPos, anchored_menu_pos};
use crate::components::md::{Chip, SectionTitle};
use crate::platform::worker_paths::palette::child_path;
use crate::pump::Pump;

/// The recents strip, shown only while the browser still stands where it opened
/// and this machine has somewhere it has been.
#[must_use]
pub fn recents_header(view: &BrowseView, on_pick: EventHandler<String>) -> Option<Element> {
    if view.cwd != view.home || view.recents.is_empty() {
        return None;
    }
    let worker_os = view.worker_os.clone();
    let mut chips = Vec::with_capacity(view.recents.len());
    for recent in &view.recents {
        let folder = recent.clone();
        let label =
            crate::platform::worker_paths::worker_path_basename(worker_os.as_deref(), recent)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| recent.clone());
        let target = folder.clone();
        chips.push(rsx! {
            Chip {
                label,
                icon: Some("folder".to_owned()),
                title: Some(recent.clone()),
                test_id: Some("browse-recent".to_owned()),
                onclick: move |_| on_pick.call(target.clone()),
            }
        });
    }
    Some(rsx! {
        div { class: "df-browse-recents",
            SectionTitle { "Recent" }
            {chips.into_iter()}
        }
    })
}

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

/// Where the machine switcher sits, measured from its trigger.
#[must_use]
pub fn measure_server_anchor(trigger_id: &str) -> Option<AnchoredMenuPos> {
    let (right, bottom, viewport_width) = dom::trigger_anchor(trigger_id)?;
    Some(anchored_menu_pos(right, bottom, viewport_width))
}

/// Where the crumb overflow menu sits, measured from its trigger.
#[must_use]
pub fn measure_crumb_anchor(trigger_id: &str) -> Option<CrumbMenuPos> {
    let (left, top) = dom::trigger_corner(trigger_id)?;
    Some(CrumbMenuPos { left, top })
}
