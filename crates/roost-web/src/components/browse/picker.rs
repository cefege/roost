//! The folder picker composes places, a directory listing and terminal launch.
//! BrowseState owns navigation; the picker owns selection and form state.
//! The interaction is informed by Tanner Middleton's MIT-licensed Pi Pocket:
//! <https://github.com/TannerMidd/pi-pocket/blob/5d009fa4d1e2b69db54f05e04f750a6f2e591bb1/web/sheets/folder.js>.

mod control_set;
mod controls;
mod page;
mod parts;
mod regions;

use dioxus::prelude::*;
use roost_client_core::store::browse_paths::BROWSE_HOME;
use roost_client_core::store::browse_state::intent::{BrowseIntent, apply_browse_intent};
use roost_protocol::wire::WorkerFp;

use crate::components::browse::dom;
use crate::components::browse::key_listener::use_picker_keys;
use crate::components::browse::keys::{PickerKey, PickerKeyContext, moved_cursor};
use crate::components::browse::listing;
use crate::components::browse::view::{PickerReading, read_picker};
use crate::components::browse::{BrowseStatus, newest_session_cwd};
use crate::components::layout::window_size::use_is_compact;
use crate::components::ui::dom::focus_by_id;
use crate::components::ui::{Dialog, DialogContent, DialogTitle};
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;

/// The new-folder dialog's own fields. Not store state: they are a form, and a
/// half-typed name has no business outliving the machine it was typed for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewFolderForm {
    /// Whether the dialog is showing.
    pub open: bool,
    /// What the reader has typed.
    pub name: String,
    /// Whether a create is in flight.
    pub busy: bool,
    /// The validation or machine failure for this attempt, shown in place.
    pub error: Option<String>,
}

/// One machine's folder browser.
#[component]
pub fn BrowsePicker(worker_fp: String) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let compact = use_is_compact();
    let ticket = use_signal(|| 0_u64);
    let mut cursor = use_signal(|| -1_i64);
    let retry = use_signal(|| 0_u32);
    let hide_middle = use_signal(|| 0_usize);
    let server_menu_open = use_signal(|| false);
    let crumb_menu_open = use_signal(|| false);
    let new_folder = use_signal(NewFolderForm::default);
    let show_hidden = use_signal(|| false);

    // Opened during render, not in an effect: an effect runs after the first
    // paint, which would paint and list the `~` placeholder before the real
    // directory. Opening is idempotent per machine, and checking on every
    // render is what follows a `browse-server` switch to a machine this picker
    // has never browsed.
    ensure_machine_browser(&pump, &worker_fp);
    // The store writes the picker makes (open, intents, a landed listing) go
    // around the core's revision counter; reading the ticket is what repaints
    // on them.
    let _ = ticket();
    let reading = browse_reading(&pump, &worker_fp, show_hidden());
    let mut path_input = use_signal(|| reading.view.resolved.clone());

    // The listing is asked for whenever the directory, the machine's presence or
    // the retry nonce moves — and NOT when the answer lands, which is what stops
    // one listing from asking for the next.
    let listing_pump = pump.clone();
    let listing_fp = worker_fp.clone();
    let watched = (reading.view.cwd.clone(), reading.view.scoped, retry());
    use_effect(use_reactive(
        (&watched.0, &watched.1, &watched.2),
        move |_| {
            listing::refresh(listing_pump.clone(), listing_fp.clone(), ticket);
        },
    ));

    // A row index must never carry over to another listing.
    let scope = (
        worker_fp.clone(),
        reading.view.cwd.clone(),
        reading.view.filter.clone(),
        reading.view.folders.clone(),
    );
    let mut last_scope = use_signal(|| scope.clone());
    if *last_scope.peek() != scope {
        last_scope.set(scope);
        cursor.set(-1);
    }
    use_effect(move || dom::scroll_entry_into_view(cursor()));
    let mut key_worker = use_signal(|| worker_fp.clone());
    if *key_worker.peek() != worker_fp {
        key_worker.set(worker_fp.clone());
    }

    use_picker_keys(
        {
            let pump = pump.clone();
            let dialog = new_folder;
            move || {
                let reading = browse_reading(&pump, &key_worker.peek(), *show_hidden.peek());
                PickerKeyContext {
                    default_prevented: false,
                    dialog_open: dialog.peek().open,
                    inside_results: false,
                    scoped: reading.view.scoped,
                    compact,
                    has_active: *cursor.peek() >= 0,
                }
            }
        },
        {
            let pump = pump.clone();
            move |action| {
                if let PickerKey::EditPath(path) = action {
                    path_input.set(path);
                    focus_by_id("browse-path-input");
                } else {
                    let reading = browse_reading(&pump, &key_worker.peek(), *show_hidden.peek());
                    run_key(action, &pump, &reading, cursor, ticket, &navigate);
                }
            }
        },
    );

    let content = rsx! {
        page::PickerPage {
            pump: pump.clone(),
            worker_fp: worker_fp.clone(),
            reading,
            compact,
            cursor,
            show_hidden,
            path_input,
            new_folder,
            ticket,
            retry,
            hide_middle,
            server_menu_open,
            crumb_menu_open,
            navigate,
        }
    };
    if compact {
        return content;
    }
    let close = EventHandler::new(move |()| navigate.call("/".to_owned()));
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |still_open: bool| if !still_open { close.call(()) },
            DialogContent {
                class: Some("flex h-[min(85vh,48rem)] w-[min(64rem,calc(100%-2rem))] max-w-none flex-col gap-0 overflow-hidden p-0".to_owned()),
                show_close_button: Some(false),
                on_open_auto_focus: Some(EventHandler::new(control_set::keep_sheet_focus)),
                DialogTitle { class: Some("sr-only".to_owned()), "Browse folders" }
                {content}
            }
        }
    }
}

/// One read of everything the picker paints, in a single borrow of the core.
#[must_use]
pub fn browse_reading(pump: &Pump, worker_fp: &str, show_hidden: bool) -> PickerReading {
    let core = pump.core();
    let core = core.borrow();
    let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
    let store = core.store();
    let start = WorkerFp::try_from(worker_fp)
        .ok()
        .and_then(|fingerprint| store.browse.get(&fingerprint))
        .map_or_else(
            || BROWSE_HOME.to_owned(),
            |machine| machine.cwd().to_owned(),
        );
    read_picker(store, worker_fp, &start, now_ms, show_hidden)
}

/// Open `worker_fp`'s browser on its newest session directory unless it is
/// already open; an open machine keeps the directory the reader left it in.
fn ensure_machine_browser(pump: &Pump, worker_fp: &str) {
    let Ok(fingerprint) = WorkerFp::try_from(worker_fp) else {
        return;
    };
    let core = pump.core();
    if core.borrow().store().browse.get(&fingerprint).is_some() {
        return;
    }
    let home = newest_session_cwd(pump, worker_fp);
    let mut core = core.borrow_mut();
    let _ = apply_browse_intent(
        core.store_mut(),
        &BrowseIntent::Open {
            worker_fp: worker_fp.to_owned(),
            home: Some(home),
        },
    );
}

/// Move the cursor, drill, go to the parent, launch here, or leave.
fn run_key(
    action: PickerKey,
    pump: &Pump,
    reading: &PickerReading,
    cursor: Signal<i64>,
    ticket: Signal<u64>,
    navigate: &EventHandler<String>,
) {
    let mut cursor = cursor;
    let mut ticket = ticket;
    let view = &reading.view;
    match action {
        PickerKey::Ignore | PickerKey::EditPath(_) => {}
        PickerKey::Leave => navigate.call("/".to_owned()),
        PickerKey::Move(delta) => {
            let next = moved_cursor(*cursor.peek(), delta, view.folders.len());
            cursor.set(next);
        }
        PickerKey::Typeahead(prefix) => {
            if let Some(index) = view
                .folders
                .iter()
                .position(|entry| entry.name.to_lowercase().starts_with(&prefix))
            {
                cursor.set(index as i64);
            }
        }
        PickerKey::Drill => {
            let Some(name) = view
                .folders
                .get(usize::try_from(*cursor.peek()).unwrap_or_default())
                .map(|entry| entry.name.clone())
            else {
                return;
            };
            let Some(path) = crate::platform::worker_paths::palette::child_path(
                view.worker_os.as_deref(),
                &view.resolved,
                &name,
            ) else {
                return;
            };
            navigate_to(pump, &mut ticket, &view.worker_fp, path);
            cursor.set(-1);
        }
        PickerKey::Parent => {
            let path = crate::platform::worker_paths::palette::parent_path(
                view.worker_os.as_deref(),
                &view.resolved,
            );
            navigate_to(pump, &mut ticket, &view.worker_fp, path);
            cursor.set(-1);
        }
        PickerKey::OpenHere => listing::launch_terminal(
            pump.clone(),
            view.worker_fp.clone(),
            selected_target(view, *cursor.peek()),
            *navigate,
        ),
    }
}

fn selected_target(view: &crate::components::browse::BrowseView, cursor: i64) -> String {
    usize::try_from(cursor)
        .ok()
        .and_then(|index| view.folders.get(index))
        .and_then(|entry| {
            crate::platform::worker_paths::palette::child_path(
                view.worker_os.as_deref(),
                &view.resolved,
                &entry.name,
            )
        })
        .unwrap_or_else(|| view.resolved.clone())
}

/// Move one machine's browser and let the listing effect follow it.
fn navigate_to(pump: &Pump, ticket: &mut Signal<u64>, worker_fp: &str, path: String) {
    listing::apply(
        pump,
        ticket,
        BrowseIntent::Navigate {
            worker_fp: worker_fp.to_owned(),
            path,
        },
    );
}

/// The caption a loading region shows, which names what is being waited for.
#[must_use]
pub fn loading_caption(status: BrowseStatus, hydrated: bool) -> &'static str {
    match (status, hydrated) {
        (BrowseStatus::Loading, false) => "Loading machine\u{2026}",
        _ => "Loading folders\u{2026}",
    }
}
