//! The folder picker's page: one machine's directory, its history, and the
//! terminal-launch flow, with the toolbar, path band, entry grid, launch bar and
//! new-folder dialog composed around them. Desktop gets the centred `Sheet`; a
//! phone paints the same content with no overlay, so the entries own the screen.
//!
//! The state it moves is the store's `BrowseState`, reached only through
//! `BrowseIntent`; what lives here is the keyboard cursor, the two floating
//! menus and the new-folder dialog's own fields.
//!
//! Ports `apps/web/src/components/browse/WorkerBrowsePage.tsx`. Called by
//! `components::browse::BrowseSurface`; the composition is `picker::page`.

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
use crate::components::browse::path_bar::CrumbMenuPos;
use crate::components::browse::view::{PickerReading, read_picker};
use crate::components::browse::{BrowseStatus, newest_session_cwd};
use crate::components::context_menu::AnchoredMenuPos;
use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{Sheet, SheetSide};
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
    let server_anchor = use_signal(|| None::<AnchoredMenuPos>);
    let crumb_menu_open = use_signal(|| false);
    let crumb_anchor = use_signal(|| None::<CrumbMenuPos>);
    let new_folder = use_signal(NewFolderForm::default);

    let start_dir = newest_session_cwd(&pump, &worker_fp);
    // Reactive on the machine, not once per component: a `browse-server` switch
    // to a machine this picker has never browsed leaves that machine's browser
    // closed, and the region then sits pending at `~` forever. `use_hook` ran
    // only for the machine the picker first mounted with.
    let open_pump = pump.clone();
    use_effect(use_reactive((&worker_fp,), move |(machine,)| {
        open_machine_browser(&open_pump, machine.as_str(), &start_dir);
    }));
    let reading = browse_reading(&pump, &worker_fp);

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

    // Navigating drops the highlight: a selected row in a directory the reader
    // has left is a drill into the wrong place.
    use_effect(use_reactive((&reading.view.cwd,), move |_| {
        if *cursor.peek() != -1 {
            cursor.set(-1);
        }
    }));
    dom::scroll_entry_into_view(cursor());

    use_picker_keys(
        {
            let pump = pump.clone();
            let worker_fp = worker_fp.clone();
            let cursor = cursor;
            let dialog_open = new_folder().open;
            move || {
                let reading = browse_reading(&pump, &worker_fp);
                PickerKeyContext {
                    default_prevented: false,
                    dialog_open,
                    inside_results: false,
                    scoped: reading.view.scoped,
                    compact,
                    folder_count: reading.view.folders.len(),
                    columns: dom::grid_columns(),
                    has_active: *cursor.peek() >= 0,
                }
            }
        },
        {
            let pump = pump.clone();
            let worker_fp = worker_fp.clone();
            move |action| {
                let reading = browse_reading(&pump, &worker_fp);
                run_key(action, &pump, &reading, cursor, ticket, &navigate);
            }
        },
    );

    let content = rsx! {
        page::PickerPage {
            pump: pump.clone(),
            worker_fp: worker_fp.clone(),
            reading: reading.clone(),
            compact,
            cursor,
            new_folder,
            ticket,
            retry,
            hide_middle,
            server_menu_open,
            server_anchor,
            crumb_menu_open,
            crumb_anchor,
            navigate,
        }
    };
    if compact {
        return content;
    }
    let close = EventHandler::new(move |()| navigate.call("/".to_owned()));
    rsx! {
        Sheet {
            open: true,
            on_close: close,
            headline: "Browse folders".to_owned(),
            side: SheetSide::Center,
            class: Some("roost-dialog--wide roost-dialog--browse".to_owned()),
            show_close_button: false,
            on_open_auto_focus: Some(EventHandler::new(control_set::keep_sheet_focus)),
            {content}
        }
    }
}

/// One read of everything the picker paints, in a single borrow of the core.
#[must_use]
pub fn browse_reading(pump: &Pump, worker_fp: &str) -> PickerReading {
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
    read_picker(store, worker_fp, &start, now_ms)
}

/// Open `worker_fp`'s browser on `home`, once, before the first paint.
fn open_machine_browser(pump: &Pump, worker_fp: &str, home: &str) {
    let core = pump.core();
    let mut core = core.borrow_mut();
    let _ = apply_browse_intent(
        core.store_mut(),
        &BrowseIntent::Open {
            worker_fp: worker_fp.to_owned(),
            home: Some(home.to_owned()),
        },
    );
}

/// Move the cursor, drill, go to the parent, open a terminal here, or leave.
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
        PickerKey::Ignore | PickerKey::Leave => {}
        PickerKey::Move(delta) => {
            let next = moved_cursor(*cursor.peek(), delta, view.folders.len());
            cursor.set(next);
            dom::scroll_entry_into_view(next);
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
                &view.cwd,
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
        PickerKey::OpenHere => {
            listing::launch_terminal(
                pump.clone(),
                view.worker_fp.clone(),
                view.resolved.clone(),
                *navigate,
            );
        }
    }
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
