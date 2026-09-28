//! Which session a terminal route addresses: `/s/:id` by id, `/t/:fp/*folder` by
//! (machine, spawn folder, newest wins), `/w/:workspaceId` by the workspace's
//! newest open session, and the legacy `/w/:workspaceId/t/:channel` by channel.
//! Ports `activeSessionForPath` (`apps/web/src/store/selectors.ts:81-92`) and
//! `MainPane.tsx`'s `activeSession` memo; read by `MainPane`, the status and
//! title bars, the sidebar and the deck, so every surface scopes to exactly the
//! session the pane renders.

use roost_client_core::Store;
use roost_client_core::store::selectors::{
    newest_open_session_in_folder, session_by_folder, session_by_id, session_by_workspace,
    session_folder_key,
};
use roost_client_core::store::{Session, SessionStatus, WorkerPaths};

use crate::routes::{Route, session_href};
use crate::terminal_href::{decode_folder_path, worker_os};

/// Whether a route is one of the terminal routes (`/s`, `/t`, `/w`), whether
/// or not it resolves to a session right now.
pub fn is_terminal_route(route: &Route) -> bool {
    matches!(
        route,
        Route::Session { .. } | Route::Terminal { .. } | Route::Workspace { .. }
    )
}

/// The session a route addresses, open or not; `None` off a terminal route.
///
/// A legacy channel that is not a number resolves to nothing, as MainPane's
/// `parseInt` guard did: an unreadable bookmark must not open a different
/// session of the same workspace.
pub fn active_session_for_route<'store>(
    store: &'store Store,
    paths: &dyn WorkerPaths,
    route: &Route,
) -> Option<&'store Session> {
    match route {
        Route::Session { session_id } => session_by_id(store, session_id),
        Route::Terminal {
            worker_fp,
            folder_path,
        } => {
            let folder = decode_folder_path(worker_os(store, worker_fp), folder_path)?;
            session_by_folder(store, paths, worker_fp, &folder)
        }
        Route::Workspace {
            workspace_id,
            channel_id: None,
        } => session_by_workspace(store, workspace_id),
        Route::Workspace {
            channel_id: Some(channel),
            ..
        } => {
            let channel: u32 = channel.parse().ok()?;
            store
                .sessions
                .sessions()
                .values()
                .find(|session| session.channel.as_u32() == channel)
        }
        _ => None,
    }
}

/// The session a pathname addresses (v2 `activeSessionForPath`).
pub fn active_session_for_path<'store>(
    store: &'store Store,
    paths: &dyn WorkerPaths,
    path: &str,
) -> Option<&'store Session> {
    active_session_for_route(store, paths, &Route::parse(path))
}

/// The session the route addresses, only while it is OPEN — the one the deck
/// can render. A closed row can linger in the store; routing to it would leave
/// a blank pane beside a tab bar that still lists the rest.
pub fn active_open_session_for_route<'store>(
    store: &'store Store,
    paths: &dyn WorkerPaths,
    route: &Route,
) -> Option<&'store Session> {
    active_session_for_route(store, paths, route).filter(|session| session.status == SessionStatus::Open)
}

/// Where the view lands when `session` goes away: the newest still-open sibling
/// in its folder (never itself, never one waiting out a close), else home. The
/// ONE policy behind v2's `closeSession.siblingOrHomeHref` and MainPane's
/// safety-net `bounceTarget`, which v2 kept "in lockstep" by hand.
pub fn sibling_or_home_href(store: &Store, paths: &dyn WorkerPaths, session: &Session) -> String {
    let folder_key = session_folder_key(store, paths, session);
    newest_open_session_in_folder(store, paths, &folder_key, Some(session.id.as_str()))
        .map_or_else(|| "/".to_owned(), |sibling| session_href(sibling.id.as_str()))
}
