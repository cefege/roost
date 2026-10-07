//! The palette's body: the catalog, the filter, the result rows and the list
//! keys. Ports `apps/web/src/components/palette/CommandPaletteBody.tsx`; the
//! rows and the legend it renders live in `palette::pieces`.
//!
//! The body exists only while the palette is open, which is what makes "clear
//! the query and focus the field on open" a mount-time fact instead of a reset
//! that can be forgotten: there is no closed state to reset from, and a WebSocket
//! tick cannot rebuild the list out from under a reader who is arrowing through
//! it, because the cursor is reset by the QUERY and by nothing else.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::navigation::query::navigation_search_terms;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::palette::{
    CommandPaletteContext, PaletteItem, PaletteTarget, build_default_items, matches_query,
};
use roost_client_core::store::paths::folder_key_of;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::{SessionStatus, Store};

use super::dom;
use super::list_keys;
use super::outcome;
use super::pieces::{PaletteFooter, PaletteRow};
use crate::components::help::shortcuts::reader_platform;
use crate::components::md::TextField;
use crate::keyboard_shortcuts::ShortcutOverlays;
use crate::platform::browser_platform::{PlatformShortcut, platform_shortcut_label};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::route_session::active_session_for_path;

/// The test ids the field and the rows are addressed by.
const INPUT_TEST_ID: &str = "command-palette-input";
const ITEM_TEST_ID: &str = "command-palette-item";

/// The field's own look: the search row draws the border and the spacing, so the
/// control inside it is a bare line of text.
const SEARCH_CONTROL_STYLE: &str = "background: transparent; border: 0; outline: none; color: var(--text-hi); \
     flex: 1; min-inline-size: 0; font: var(--md-body-m-weight) var(--md-body-m-size) / var(--md-body-m-line) var(--md-font);";
const SEARCH_CHORD_STYLE: &str = "color: var(--text-lo); \
     font: var(--md-body-m-weight) var(--md-body-m-size) / var(--md-body-m-line) var(--md-font);";
const EMPTY_STYLE: &str = "padding: var(--md-space-5) var(--md-space-4); text-align: center; color: var(--text-lo); \
     font: var(--md-body-s-weight) var(--md-body-s-size) / var(--md-body-s-line) var(--md-font);";

/// The search, the results, and the legend. Mounted only while open.
#[component]
pub fn PaletteBody(pump: Pump, overlays: ShortcutOverlays) -> Element {
    let mut query = use_signal(String::new);
    let mut active_index = use_signal(|| 0_usize);
    let navigate = crate::router_state::use_navigate();
    let path = crate::router_state::use_location();

    let items = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let context = palette_context(store, &path(), now_ms);
        let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
        let workspaces: Vec<_> = store.workspaces.values().cloned().collect();
        build_default_items(&context, &documents, &workspaces)
    };
    let terms = navigation_search_terms(&query());
    let filtered: Vec<PaletteItem> = items
        .into_iter()
        .filter(|item| {
            let haystack = format!(
                "{} {} {}",
                item.label,
                item.hint.as_deref().unwrap_or_default(),
                item.search.as_deref().unwrap_or_default()
            );
            matches_query(&haystack, &terms)
        })
        .collect();
    let has_results = !filtered.is_empty();

    // The list keys answer from the rows as this render produced them; a
    // listener that rebuilt the catalog would do the store's work on every
    // arrow press.
    let rows: Rc<RefCell<Vec<PaletteItem>>> = use_hook(|| Rc::new(RefCell::new(Vec::new())));
    *rows.borrow_mut() = filtered.clone();
    let on_select = select_handler(pump.clone(), overlays, navigate, query);

    use_drop({
        let keys = list_keys::install(Rc::clone(&rows), on_select, active_index);
        move || drop(keys)
    });
    use_effect({
        let mut query = query;
        move || {
            if *overlays.palette.peek() {
                query.set(String::new());
                active_index.set(0);
                dom::focus_by_test_id_next_frame(INPUT_TEST_ID);
            }
        }
    });
    use_effect({
        let query = query;
        move || {
            let _ = query();
            active_index.set(0);
        }
    });
    use_effect({
        let active_index = active_index;
        move || dom::scroll_row_into_view(ITEM_TEST_ID, active_index())
    });

    let on_input = move |value: String| query.set(value);
    let chord = platform_shortcut_label(PlatformShortcut::CommandPalette, "⌘K", reader_platform())
        .to_owned();
    let highlighted = active_index();
    rsx! {
        div { class: "roost-command-palette",
            div { class: "roost-command-palette__search",
                span { style: SEARCH_CHORD_STYLE, {chord} }
                TextField {
                    value: query(),
                    on_input,
                    placeholder: "Jump to session, workspace, or action…",
                    aria_label: "Jump to session, workspace, or action",
                    test_id: INPUT_TEST_ID.to_owned(),
                    style: "flex: 1 1 auto; min-inline-size: 0;",
                    control_style: Some(SEARCH_CONTROL_STYLE.to_owned()),
                }
            }
            div { class: "roost-command-palette__results", "data-testid": "command-palette-results",
                if !has_results {
                    div { style: EMPTY_STYLE, "No matches" }
                }
                for (index, item) in filtered.into_iter().enumerate() {
                    PaletteRow {
                        item,
                        index,
                        active: highlighted == index,
                        on_select,
                    }
                }
            }
            PaletteFooter { has_results }
        }
    }
}

/// Close, clear, then act: the palette must be gone before a navigation, or the
/// dialog the reader is being taken away from stays mounted over the new route.
fn select_handler(
    pump: Pump,
    overlays: ShortcutOverlays,
    navigate: EventHandler<String>,
    mut query: Signal<String>,
) -> EventHandler<PaletteItem> {
    EventHandler::new(move |item: PaletteItem| {
        let mut palette = overlays.palette;
        palette.set(false);
        query.set(String::new());
        outcome::perform(&pump, &navigate, &item, overlays);
    })
}

/// The catalog's context, compiled from the route the reader is on.
///
/// A route that resolves to a CLOSED session has no target: the folder it names
/// is a row in the store, not a place a task can be queued, and offering the
/// command is how it ends up failing on press.
fn palette_context(store: &Store, path: &str, now_ms: i64) -> CommandPaletteContext {
    let open_session = active_session_for_path(store, &BrowserWorkerPaths, path)
        .filter(|session| session.status == SessionStatus::Open);
    let Some(session) = open_session else {
        return CommandPaletteContext {
            auth_generation: store.auth_generation,
            ..CommandPaletteContext::default()
        };
    };
    let worker_fp = session.worker_fp.as_str();
    CommandPaletteContext {
        auth_generation: store.auth_generation,
        active_session: Some(PaletteTarget {
            id: session.id.as_str().to_owned(),
            worker_fp: worker_fp.to_owned(),
            cwd: session.cwd.clone(),
        }),
        active_folder: Some(PaletteTarget {
            id: folder_key_of(
                &BrowserWorkerPaths,
                crate::terminal_href::worker_os(store, worker_fp),
                worker_fp,
                &session.cwd,
            ),
            worker_fp: worker_fp.to_owned(),
            cwd: session.cwd.clone(),
        }),
        worker_routable: store.workers.get(worker_fp).is_some_and(|worker| {
            worker_online(worker, store.routable_worker_fps.as_ref(), now_ms)
        }),
    }
}
