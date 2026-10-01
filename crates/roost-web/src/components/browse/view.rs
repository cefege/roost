//! One render's read of the folder picker: the machine, its browser state, the
//! listing the grid paints and the terminal counts beside each row. Every value
//! the four bands paint is computed here so no band re-derives one.
//!
//! Called by `browse::picker`. Depends on the store, the path codec and the
//! clock; it writes nothing.

use std::collections::BTreeMap;

use roost_client_core::Store;
use roost_client_core::store::browse_entries::{
    BrowseEntry, folder_names, visible_files, visible_folders,
};
use roost_client_core::store::browse_machine::BrowseListing;
use roost_client_core::store::folder_activity::compute_folder_activity;
use roost_client_core::store::navigation::worker_online;
use roost_client_core::store::selectors::all_sessions;
use roost_protocol::wire::WorkerFp;

use crate::components::browse::{BrowseStatus, BrowseView, short_worker_label, workers_hydrated};
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::platform::worker_paths::palette::{child_path, parent_path, path_crumbs};

/// What the picker needs from the store, read in one borrow.
#[derive(Debug, Clone, PartialEq)]
pub struct PickerReading {
    /// Everything the four bands paint.
    pub view: BrowseView,
    /// The clock, for the entry timestamps.
    pub now_ms: i64,
    /// Every directory name in the standing listing, hidden ones included.
    pub sibling_names: Vec<String>,
    /// The machines that are online, for the switcher.
    pub online_workers: Vec<roost_protocol::wire::Worker>,
}

/// Read the picker for `worker_fp`.
///
/// `start_dir` is the directory the browser opens on: a machine's browser keeps
/// the directory the reader left it in, so this is only the FIRST answer.
#[must_use]
pub fn read_picker(store: &Store, worker_fp: &str, start_dir: &str, now_ms: i64) -> PickerReading {
    let hydrated = workers_hydrated(store);
    let worker = store.workers.get(worker_fp);
    let worker_os = worker.map(|worker| worker.os.as_str().to_owned());
    let scoped = worker.is_some();
    let server_label =
        short_worker_label(worker_fp, worker.map_or("", |worker| worker.label.as_str()));
    let server_online = worker
        .is_some_and(|worker| worker_online(worker, store.routable_worker_fps.as_ref(), now_ms));
    let machine = WorkerFp::try_from(worker_fp)
        .ok()
        .and_then(|fingerprint| store.browse.get(&fingerprint));
    let home = machine.map_or_else(|| start_dir.to_owned(), |machine| machine.home().to_owned());
    let cwd = machine.map_or_else(|| start_dir.to_owned(), |machine| machine.cwd().to_owned());
    let filter = machine.map_or_else(String::new, |machine| machine.filter().to_owned());
    let filter_open = machine.is_some_and(|machine| machine.is_filter_open());
    let can_back = machine.is_some_and(|machine| machine.history().can_go_back());
    let can_forward = machine.is_some_and(|machine| machine.history().can_go_forward());
    let recents = machine.map_or_else(Vec::new, |machine| {
        machine.recents(store.sessions.sessions())
    });
    // A LISTING NAMES THE DIRECTORY IT ANSWERED FOR. A move changes the
    // directory without disturbing the outstanding listing, so between the two
    // the machine still holds the folder the reader has LEFT — and projecting
    // its rows, its failure or its resolved path under the new breadcrumb
    // paints a listing that belongs to a directory nobody is standing in.
    // `BrowseState::apply_listing` fences the write half of that fact; this is
    // the read half, and without it a listing that has not answered reads as
    // an EMPTY FOLDER rather than as the pending region it is.
    let listing = machine.map(|machine| machine.listing());
    let for_this_directory = listing.filter(|_| answers_directory(listing, &cwd));
    let entries: Vec<BrowseEntry> = for_this_directory
        .map(|listing| listing.entries().to_vec())
        .unwrap_or_default();
    let is_loading = for_this_directory.is_none_or(BrowseListing::is_loading);
    let resolved = for_this_directory
        .and_then(BrowseListing::resolved_path)
        .unwrap_or(cwd.as_str())
        .to_owned();
    let error_message = match for_this_directory {
        Some(BrowseListing::Failed { message, .. }) => Some(message.clone()),
        _ => None,
    };
    let status = if !scoped || is_loading {
        BrowseStatus::Loading
    } else if error_message.is_some() {
        BrowseStatus::Error
    } else if !server_online && entries.is_empty() {
        BrowseStatus::Offline
    } else {
        BrowseStatus::Ready
    };
    let folders = visible_folders(&entries, &filter);
    let files = visible_files(&entries, &filter);
    let sibling_names = folder_names(&entries);
    let child_paths: Vec<String> = folders
        .iter()
        .filter_map(|entry| child_path(worker_os.as_deref(), &resolved, &entry.name))
        .collect();
    let child_refs: Vec<&str> = child_paths.iter().map(String::as_str).collect();
    let terminal_counts: BTreeMap<String, usize> = compute_folder_activity(
        &all_sessions(store),
        &BrowserWorkerPaths,
        worker_os.as_deref(),
        worker_fp,
        &child_refs,
    )
    .into_iter()
    .map(|(path, activity)| (path, activity.terminals))
    .collect();
    let can_up = parent_path(worker_os.as_deref(), &resolved) != resolved;
    let crumbs = path_crumbs(worker_os.as_deref(), &resolved);
    let mut online_workers: Vec<&roost_protocol::wire::Worker> = store
        .workers
        .values()
        .filter(|worker| worker_online(worker, store.routable_worker_fps.as_ref(), now_ms))
        .collect();
    online_workers.sort_by(|left, right| left.label.cmp(&right.label));
    PickerReading {
        view: BrowseView {
            worker_fp: worker_fp.to_owned(),
            worker_os,
            scoped,
            hydrated,
            server_label,
            server_online,
            home,
            cwd,
            resolved,
            crumbs,
            status,
            error_message,
            filter,
            filter_open,
            folders,
            files,
            recents,
            terminal_counts,
            can_up,
            can_back,
            can_forward,
            show_files: store.ui.home_folder_show_files,
        },
        now_ms,
        sibling_names,
        online_workers: online_workers.into_iter().cloned().collect(),
    }
}

/// Whether `listing` is an answer about `directory`.
///
/// Every arm a listing holds in carries the directory it was asked for, and a
/// browser that has never asked carries none at all. Both are the same answer
/// for a reader: nothing has come back for the folder they are standing in.
fn answers_directory(listing: Option<&BrowseListing>, directory: &str) -> bool {
    match listing {
        Some(BrowseListing::Ready { path, .. })
        | Some(BrowseListing::Loading { path, .. })
        | Some(BrowseListing::Failed { path, .. }) => path.as_str() == directory,
        Some(BrowseListing::Idle) | None => false,
    }
}

/// The four states the picker's content region can be in, and the one rule that
/// decides between them.
///
/// These build a real `ClientCore` and read the picker the way the page does, so
/// the observable is the surface each state selects and not a restatement of
/// the fold.
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use roost_client_core::ClientCore;
    use roost_client_core::store::browse_entries::BrowseEntry;
    use roost_client_core::store::browse_machine::{BrowseListing, BrowseListingRequest};
    use roost_client_core::store::browse_state::intent::{BrowseIntent, apply_browse_intent};
    use roost_protocol::wire::{Worker, WorkerFp, WorkerOs};

    use super::read_picker;
    use crate::components::browse::BrowseStatus;

    /// One machine's fingerprint, which is 64 lowercase hex characters.
    const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
    /// The directory this machine's browser opens on.
    const HOME: &str = "/home/someone";
    /// A directory under it.
    const CHILD: &str = "/home/someone/src";
    /// A clock far enough from zero that freshness is not in question.
    const NOW: i64 = 60_000;

    /// The machine under test.
    fn machine() -> WorkerFp {
        WorkerFp::try_from(MACHINE.to_owned()).expect("a fingerprint")
    }

    /// A registry row for that machine, reachable as of `NOW`.
    fn worker() -> Worker {
        Worker {
            fp: machine(),
            label: "workstation".to_owned(),
            os: WorkerOs::Linux,
            host_identity: None,
            git_sha: None,
            host_metrics: None,
            registered_at_ms: 1,
            last_seen_ms: NOW,
            reachable_addr: None,
            keeper_runtime: None,
            terminal_core_capacity: None,
        }
    }

    /// A core whose registry holds one machine, with its browser open on `HOME`.
    fn core() -> ClientCore {
        let mut client = ClientCore::in_memory("browse-states");
        let opened = BrowseIntent::Open {
            worker_fp: MACHINE.to_owned(),
            home: Some(HOME.to_owned()),
        };
        let store = client.store_mut();
        store.workers.insert(MACHINE.to_owned(), worker());
        store.routable_worker_fps = Some(BTreeSet::from([MACHINE.to_owned()]));
        apply_browse_intent(store, &opened);
        client
    }

    /// Ask the open machine for `HOME`, which is the outstanding request.
    fn ask(core: &mut ClientCore) -> BrowseListingRequest {
        let refresh = BrowseIntent::Refresh {
            worker_fp: MACHINE.to_owned(),
        };
        let (_, request) = apply_browse_intent(core.store_mut(), &refresh);
        request.expect("an open machine has a request to issue")
    }

    /// Move the machine's browser to `CHILD` without letting the answer land.
    fn drill(core: &mut ClientCore) {
        let navigate = BrowseIntent::Navigate {
            worker_fp: MACHINE.to_owned(),
            path: CHILD.to_owned(),
        };
        apply_browse_intent(core.store_mut(), &navigate);
    }

    /// A directory that HAS answered shows its rows.
    #[test]
    fn a_directory_that_answered_shows_its_rows() {
        let mut core = core();
        let request = ask(&mut core);
        let rows = vec![BrowseEntry::dir("src", 7), BrowseEntry::file("note.txt", 9)];
        let stood = core
            .store_mut()
            .browse
            .apply_listing(&request, HOME.to_owned(), rows);
        assert!(stood, "the listing stands for the directory it answered");

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Ready);
        assert_eq!(
            reading.view.folders.len(),
            1,
            "the folder rows the grid draws"
        );
        assert_eq!(reading.view.folders[0].name, "src");
        assert_eq!(reading.view.resolved, HOME, "the breadcrumb's directory");
        assert_eq!(reading.view.error_message, None);
    }

    /// AN OUTSTANDING LISTING IS PENDING, not an empty folder. "Empty folder"
    /// is an answer; a machine that has not answered has said nothing, and the
    /// reader has to be able to tell those apart.
    #[test]
    fn a_listing_that_has_not_answered_is_pending_and_not_an_empty_folder() {
        let mut core = core();
        let outstanding = ask(&mut core);

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(outstanding.path, HOME);
        assert_eq!(reading.view.status, BrowseStatus::Loading);
        assert!(reading.view.folders.is_empty());
    }

    /// The same holds before anything has been asked: a browser that has just
    /// opened has no rows because it has no ANSWER, not because the folder is
    /// empty.
    #[test]
    fn a_browser_that_has_not_been_asked_is_pending() {
        let core = core();
        let browsing = core.store().browse.get(&machine()).expect("open");
        assert!(
            matches!(browsing.listing(), BrowseListing::Idle),
            "never asked"
        );

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Loading);
    }

    /// A FAILED LISTING NAMES ITSELF, and the failure belongs to the directory
    /// that failed.
    #[test]
    fn a_failed_listing_is_the_named_error_surface() {
        let mut core = core();
        let request = ask(&mut core);
        let named = "That folder isn't there any more.";
        let store = core.store_mut();
        let failed = store.browse.fail_listing(&request, named.to_owned());
        assert!(failed, "the failure belongs to the directory that failed");

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Error);
        assert_eq!(reading.view.error_message.as_deref(), Some(named));
        assert!(reading.view.folders.is_empty());
    }

    /// A MACHINE THAT ANSWERED AND THEN WENT AWAY is offline, which is not the
    /// same answer as a machine that never answered and not the same as a
    /// folder that failed.
    #[test]
    fn a_machine_that_went_away_is_the_offline_surface() {
        let mut core = core();
        let request = ask(&mut core);
        let stood = core
            .store_mut()
            .browse
            .apply_listing(&request, HOME.to_owned(), vec![]);
        assert!(stood, "the machine answered before it went away");
        core.store_mut().routable_worker_fps = Some(BTreeSet::new());

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Offline);
    }

    /// A LISTING FOR THE DIRECTORY THE READER LEFT CANNOT REPAINT THIS ONE: its
    /// rows belong to a folder the breadcrumb no longer names.
    #[test]
    fn a_listing_for_the_directory_the_reader_left_is_pending_here() {
        let mut core = core();
        let request = ask(&mut core);
        let rows = vec![BrowseEntry::dir("src", 7)];
        let stood = core
            .store_mut()
            .browse
            .apply_listing(&request, HOME.to_owned(), rows);
        assert!(stood, "the listing stands for HOME");
        drill(&mut core);

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Loading);
        assert_eq!(reading.view.resolved, CHILD, "the child's own breadcrumb");
        assert!(
            reading.view.folders.is_empty(),
            "the abandoned rows are gone"
        );
    }

    /// THE SAME FENCE COVERS THE REPLY: a listing that arrives for the directory
    /// the reader has left is dropped, so the rows never arrive to be projected.
    #[test]
    fn a_reply_for_the_directory_the_reader_left_is_dropped() {
        let mut core = core();
        let request = ask(&mut core);
        drill(&mut core);
        let rows = vec![BrowseEntry::dir("src", 7)];
        let stood = core
            .store_mut()
            .browse
            .apply_listing(&request, HOME.to_owned(), rows);
        assert!(
            !stood,
            "a reply for an abandoned directory must not publish"
        );

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Loading);
    }

    /// A MACHINE THIS COORDINATOR NEVER CLAIMED has no rows because it is not in
    /// the registry at all, which the page routes to its own surface.
    #[test]
    fn a_machine_outside_the_registry_is_pending_here() {
        let mut core = core();
        core.store_mut().workers.clear();

        let reading = read_picker(core.store(), MACHINE, HOME, NOW);
        assert_eq!(reading.view.status, BrowseStatus::Loading);
        assert!(!reading.view.scoped, "the page reads this as unavailable");
    }
}
