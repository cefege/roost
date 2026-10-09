//! The folder picker: `/browse` and `/browse/:workerFp`, the one surface that
//! picks the machine a terminal opens on and the folder it opens in. Mounted by
//! `app::RouteContent`.
//!
//! The STATE is the store's, not this file's: `roost_client_core::store::browse`
//! owns each machine's directory, history, filter and listing, and every move
//! arrives as a `BrowseIntent`. What lives in `browse::picker` is what the store
//! has no opinion about — the keyboard cursor, the two floating menus, the
//! new-folder dialog's own fields — exactly as v2 held them in
//! `WorkerBrowsePage`'s signals.
//!
//! Ports `apps/web/src/components/browse/`: the page, the toolbar, the path
//! band, the entry grid, the new-folder dialog, the listing request and the
//! keyboard cursor. The row filter, the history, the path arithmetic and the
//! folder-name rules are the store's and `platform::worker_paths::palette`'s.

mod dom;
mod entry_list;
mod key_listener;
mod keys;
pub(crate) mod listing;
mod new_folder;
mod path_bar;
mod picker;
mod toolbar;
mod unavailable;
mod view;

pub mod folder_glyph;

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_client_core::Store;
use roost_client_core::store::browse_entries::BrowseEntry;
use roost_client_core::store::browse_paths::BROWSE_HOME;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::all_sessions;
use roost_client_core::sync::SyncDomain;

use crate::platform::worker_paths::WorkerPathCrumb;
use crate::pump::{Pump, use_store};
use crate::router_state::use_navigate;
use crate::routes::Route;
use crate::routes::browse_href;

/// How much of a fingerprint names a machine that has no label yet.
const FP_LABEL_CHARS: usize = 8;

/// The route's surface. `/browse` with no machine resolves one first, because
/// the picker is always about one machine's filesystem.
#[component]
pub fn BrowseSurface(route: Route) -> Element {
    match route {
        Route::Browse {
            worker_fp: Some(worker_fp),
        } => rsx! { picker::BrowsePicker { worker_fp } },
        _ => rsx! { BrowseRedirect {} },
    }
}

/// `/browse` before a machine is chosen: the newest session's online machine,
/// else any online machine, else home. One navigation, taken once the store can
/// answer, so the reader never sees a picker with nothing to browse.
#[component]
fn BrowseRedirect() -> Element {
    let navigate = use_navigate();
    let pump = use_store();
    let target = {
        let core = pump.core();
        let core = core.borrow();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        browse_redirect_target(core.store(), now_ms)
    };
    let navigated = use_hook(|| std::rc::Rc::new(std::cell::Cell::new(false)));
    use_effect(use_reactive((&target,), move |(target,)| {
        let Some(href) = target else { return };
        if !navigated.replace(true) {
            navigate.call(href);
        }
    }));
    rsx! {}
}

/// Where a bare `/browse` goes, or `None` while a cold store cannot answer yet:
/// before the first snapshot and the worker registry land, "no machine online"
/// is indistinguishable from "not heard yet", and sending the reader home then
/// would be wrong.
fn browse_redirect_target(store: &Store, now_ms: i64) -> Option<String> {
    if !store.hydrated || !workers_hydrated(store) {
        return None;
    }
    Some(default_browse_worker(store, now_ms).map_or_else(|| "/".to_owned(), |fp| browse_href(&fp)))
}

/// The machine `/browse` opens on: the newest session's machine while it is
/// online, else the first online machine by label.
fn default_browse_worker(store: &Store, now_ms: i64) -> Option<String> {
    let reachable = |worker: &roost_protocol::wire::Worker| {
        worker_online(worker, store.routable_worker_fps.as_ref(), now_ms)
    };
    let mut sessions = all_sessions(store);
    sessions.sort_by_key(|session| std::cmp::Reverse(session.created_at));
    if let Some(recent) = sessions
        .iter()
        .map(|session| session.worker_fp.as_str())
        .find(|worker_fp| store.workers.get(*worker_fp).is_some_and(&reachable))
    {
        return Some(recent.to_owned());
    }
    let mut machines: Vec<&roost_protocol::wire::Worker> = store
        .workers
        .values()
        .filter(|worker| reachable(worker))
        .collect();
    machines.sort_by(|left, right| left.label.cmp(&right.label));
    machines.first().map(|worker| worker.fp.as_str().to_owned())
}

/// What the picker's content region is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseStatus {
    /// The machine is unknown so far, or its directory is on the way.
    Loading,
    /// The directory stands.
    Ready,
    /// The last listing failed and named why.
    Error,
    /// The machine answered before and is not reachable now.
    Offline,
}

/// One render's read of everything the picker paints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseView {
    /// The machine being browsed.
    pub worker_fp: String,
    /// The platform its path rules follow.
    pub worker_os: Option<String>,
    /// Whether the machine is in the registry.
    pub scoped: bool,
    /// Whether the worker registry has hydrated, so an absent machine is
    /// "unavailable" rather than "not yet known".
    pub hydrated: bool,
    /// What the machine is called.
    pub server_label: String,
    /// Whether an operator could reach it right now.
    pub server_online: bool,
    /// The directory the browser opened on.
    pub home: String,
    /// Where it is.
    pub cwd: String,
    /// What the machine resolved that to.
    pub resolved: String,
    /// The breadcrumb trail for `resolved`.
    pub crumbs: Vec<WorkerPathCrumb>,
    /// What the content region is showing.
    pub status: BrowseStatus,
    /// The reader-facing failure, when the last listing failed.
    pub error_message: Option<String>,
    /// The in-list filter.
    pub filter: String,
    /// Whether the filter box is showing.
    pub filter_open: bool,
    /// The directories the filter admits.
    pub folders: Vec<BrowseEntry>,
    /// The files the filter admits.
    pub files: Vec<BrowseEntry>,
    /// This machine's recent session directories, most recent first.
    pub recents: Vec<String>,
    /// Open shell terminals per listed folder path.
    pub terminal_counts: BTreeMap<String, usize>,
    /// Whether Up has somewhere to go.
    pub can_up: bool,
    /// Whether Back is available.
    pub can_back: bool,
    /// Whether Forward is available.
    pub can_forward: bool,
    /// Whether files are listed beside the folders.
    pub show_files: bool,
}

/// The failure copy a listing shows, mapped to a sentence.
///
/// The machine's own words stay in the log line; this is what the reader reads,
/// and a coordinator status code is not a sentence.
#[must_use]
pub fn browse_error_message(error: &str) -> String {
    let lowered = error.to_lowercase();
    if lowered.contains("not found") || lowered.contains("no such file") {
        "That folder isn't there any more.".to_owned()
    } else if lowered.contains("permission") || lowered.contains("denied") {
        "This account can't read that folder.".to_owned()
    } else if lowered.contains("unavailable") || lowered.contains("unreachable") {
        "This machine isn't reachable right now.".to_owned()
    } else {
        "Couldn't read this folder.".to_owned()
    }
}

/// What a fingerprint reads as when the machine has no label yet.
#[must_use]
pub fn short_worker_label(worker_fp: &str, label: &str) -> String {
    if !label.is_empty() {
        return label.to_owned();
    }
    worker_fp.chars().take(FP_LABEL_CHARS).collect::<String>()
}

/// Whether the worker registry has published, which is what separates "this
/// machine is unavailable" from "this browser has not heard yet".
#[must_use]
pub fn workers_hydrated(store: &Store) -> bool {
    store.sync.domain_is_ready(SyncDomain::Workers)
}

/// The newest session directory on `worker_fp`, or the browse home sentinel.
///
/// The picker opens where the reader last worked on that machine, which is the
/// only directory a machine has that means anything to them.
#[must_use]
pub fn newest_session_cwd(pump: &Pump, worker_fp: &str) -> String {
    let core = pump.core();
    let core = core.borrow();
    all_sessions(core.store())
        .into_iter()
        .filter(|session| session.worker_fp.as_str() == worker_fp)
        .max_by_key(|session| session.created_at)
        .map(|session| session.cwd.clone())
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or_else(|| BROWSE_HOME.to_owned())
}
