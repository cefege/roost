//! Navigation documents projected from the live terminal store.
//! Search and sidebar consumers share this detached index.

use roost_protocol::wire::Workspace;

use crate::store::Store;
use crate::store::navigation::{
    NavigationSearchDocument, NavigationSources, project_navigation_search_documents,
};
use crate::store::paths::WorkerPaths;

/// Every row the client can see, most recently active first.
pub fn store_navigation_documents(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
) -> Vec<NavigationSearchDocument> {
    let workspaces: Vec<Workspace> = store.workspaces.values().cloned().collect();
    let client_only = store.spawns.client_only_sessions();
    project_navigation_search_documents(&NavigationSources {
        sessions: store.sessions.sessions(),
        workers: &store.workers,
        workspaces: &workspaces,
        terminal_titles: &store.terminal_titles,
        last_activity_ms: &store.last_activity_ms,
        routable_worker_fps: store.routable_worker_fps.as_ref(),
        now_ms,
        paths,
        client_only: &client_only,
    })
}
