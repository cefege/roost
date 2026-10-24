//! Builds Libero Spotlight actions from Roost's single command catalog.
//! Called by the persistent palette host; query filtering and action outcomes
//! remain owned by the existing store catalog and outcome module.

use dioxus::prelude::*;
use libero::components::SpotlightAction;
use roost_client_core::store::navigation::query::navigation_search_terms;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::palette::{
    CommandPaletteContext, PaletteTarget, build_default_items, matches_query,
};
use roost_client_core::store::paths::folder_key_of;
use roost_client_core::store::sidebar::documents::store_navigation_documents;
use roost_client_core::store::{SessionStatus, Store};

use super::outcome;
use crate::keyboard_shortcuts::ShortcutOverlays;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::route_session::active_session_for_path;

/// Projects the existing catalog into Spotlight without delegating its search
/// semantics or retaining a second catalog.
pub(super) fn spotlight_actions(
    pump: Pump,
    mut overlays: ShortcutOverlays,
    navigate: EventHandler<String>,
    path: String,
    query: String,
) -> Vec<SpotlightAction> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
    let context = palette_context(store, &path, now_ms);
    let documents = store_navigation_documents(store, &BrowserWorkerPaths, now_ms);
    let workspaces: Vec<_> = store.workspaces.values().cloned().collect();
    let items = build_default_items(&context, &documents, &workspaces);
    let terms = navigation_search_terms(&query);
    items
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
        .map(|item| {
            let mut action = SpotlightAction::new(item.label.clone());
            if let Some(hint) = &item.hint {
                action = action.description(hint.clone());
            }
            if let Some(search) = &item.search {
                action = action.keywords(search.split_whitespace().map(str::to_owned));
            }
            let pump = pump.clone();
            action.onclick(move |_| {
                overlays.palette.set(false);
                outcome::perform(&pump, &navigate, &item, overlays);
            })
        })
        .collect()
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
