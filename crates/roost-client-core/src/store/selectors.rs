//! Derived read-only selectors over the store: the live session lists, the
//! folder buckets, and the "which session does this route mean" resolvers.
//!
//! Read-only, and deliberately not cached. A memo is a cache whose invalidation is
//! a second thing to get right, and every caller here already has the store's
//! `revision` — the number a renderer repaints on — so a selector that recomputes
//! on demand is both correct and free at the sizes a browser client holds.
//!
//! THREE OF THE SIX ARE FOLDER-BUCKETED, so they take the path codec rather than
//! guessing at path equality: two terminals in one folder are one tab strip, and
//! a comparison that folded less would show a user two folders that the shell
//! says are one.
//!
//! NOT PORTED: `activeSessionForPath` (`selectors.ts:81-92`). It resolves a
//! session by matching `/s/:id`, `/t/:workerFp/*folderPath`, `/w/:id` and a legacy
//! `/w/:id/t/:channel` against a pathname string. That is the ROUTE TABLE, which
//! `roost-web/src/routes.rs` owns; a second path parser in this crate is a second
//! route table, which is the fork this port pays for most. The host matches its own
//! route and calls the resolver that route names — `session_by_id`,
//! `session_by_folder`, or `session_by_workspace`.

use roost_protocol::wire::{Session, SessionStatus};

use crate::store::Store;
use crate::store::paths::{WorkerPaths, folder_key_of};
use crate::store::pending_close::is_pending_close;

/// Every session the store projects, in projection order.
pub fn all_sessions(store: &Store) -> Vec<&Session> {
    store.sessions.sessions().values().collect()
}

/// The canonical live membership and order for one folder bucket.
///
/// OPEN, in this folder, and not waiting out a close — a row the user has already
/// clicked ✕ on is hidden immediately while its kill waits, so including it would
/// put a row back that the user's finger just removed. Oldest first, and the id
/// breaks a tie, so the order is total: two sessions created in the same
/// millisecond must still have one order or the list reshuffles under the reader.
pub fn live_session_ids_for_folder(
    store: &Store,
    paths: &dyn WorkerPaths,
    folder_key: &str,
) -> Vec<String> {
    let mut live: Vec<(i64, String)> = store
        .sessions
        .sessions()
        .values()
        .filter(|session| {
            session.status == SessionStatus::Open
                && !is_pending_close(store, session.id.as_str())
                && session_folder_key(store, paths, session) == folder_key
        })
        .map(|session| (session.created_at, session.id.to_string()))
        .collect();
    live.sort();
    live.into_iter().map(|(_, session_id)| session_id).collect()
}

/// One session by id.
pub fn session_by_id<'session>(
    store: &'session Store,
    session_id: &str,
) -> Option<&'session Session> {
    store
        .sessions
        .session(&roost_protocol::wire::SessionId::try_from(session_id.to_owned()).ok()?)
}

/// The live session behind a `/t/:workerFp/*folderPath` route.
///
/// The OPEN session on `worker_fp` spawned in `folder_path`. Two terminals in one
/// folder is normal — every spawn mints a new id — so a collision ties to the
/// newest `created_at`. `None` when nothing live matches, which is the caller's
/// signal to go home.
pub fn session_by_folder<'session>(
    store: &'session Store,
    paths: &dyn WorkerPaths,
    worker_fp: &str,
    folder_path: &str,
) -> Option<&'session Session> {
    let worker_os = store
        .workers
        .get(worker_fp)
        .map(|worker| worker.os.as_str());
    newest_open(store, |session| {
        session.worker_fp.as_str() == worker_fp
            && paths.same_folder(
                worker_os,
                session.spawn_cwd.as_deref().unwrap_or(session.cwd.as_str()),
                folder_path,
            )
    })
}

/// The newest OPEN session belonging to a workspace.
///
/// `None` for an empty workspace, which is a real state the caller must handle
/// rather than an error.
pub fn session_by_workspace<'session>(
    store: &'session Store,
    workspace_id: &str,
) -> Option<&'session Session> {
    newest_open(store, |session| {
        session
            .workspace_id
            .as_ref()
            .is_some_and(|workspace| workspace.as_str() == workspace_id)
    })
}

/// The newest OPEN session in a folder bucket, other than `except_id`.
///
/// Backs the pane's safety net: when the terminal being viewed ends, land on a
/// sibling in the SAME folder rather than at home.
pub fn newest_open_session_in_folder<'session>(
    store: &'session Store,
    paths: &dyn WorkerPaths,
    folder_key: &str,
    except_id: Option<&str>,
) -> Option<&'session Session> {
    newest_open(store, |session| {
        Some(session.id.as_str()) != except_id
            && !is_pending_close(store, session.id.as_str())
            && session_folder_key(store, paths, session) == folder_key
    })
}

/// The bucket a session's row lives in: its machine plus its LIVE folder, which
/// follows a `cd`.
pub fn session_folder_key(store: &Store, paths: &dyn WorkerPaths, session: &Session) -> String {
    let worker_os = store
        .workers
        .get(session.worker_fp.as_str())
        .map(|worker| worker.os.as_str());
    folder_key_of(
        paths,
        worker_os,
        session.worker_fp.as_str(),
        session.cwd.as_str(),
    )
}

/// The newest OPEN session that matches, by `created_at`.
///
/// The comparison is `>` and never `>=`, so a tie resolves to the first session
/// in projection order rather than to whichever row the map happened to yield
/// last; a selector whose answer changes between two calls with the same inputs
/// is a selector nobody can cache a route against.
fn newest_open(store: &Store, predicate: impl Fn(&Session) -> bool) -> Option<&Session> {
    let mut best: Option<&Session> = None;
    for session in store.sessions.sessions().values() {
        if session.status != SessionStatus::Open || !predicate(session) {
            continue;
        }
        if best.is_none_or(|current| session.created_at > current.created_at) {
            best = Some(session);
        }
    }
    best
}
