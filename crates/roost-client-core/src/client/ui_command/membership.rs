//! Which sessions a UI command or a UI state report may name: open rows,
//! this browser's optimistic placeholders, and the split between the two.
//!
//! Ports v2's `openSession` (`apps/web/src/lib/uiCommandDispatch.ts`), the
//! `activeFolder` projection of `apps/web/src/lib/uiLayoutApply.ts`, and
//! `projectOptimisticSpawnMembership` / `liveSessionIdsForFolder` as they meet
//! (`apps/web/src/store/{optimisticSpawn,selectors}.ts`). v2 kept placeholders
//! as open rows in the root store; here they live in `Store::spawns`, so this
//! file is where the two sets are merged back into v2's one list.

use crate::client::ui_state::LayoutApplyFolder;
use crate::store::Store;
use crate::store::optimistic_spawn::ClientOnlySession;
use crate::store::paths::{WorkerPaths, folder_key_of};
use crate::store::pending_close::is_pending_close;
use crate::store::selectors::{live_session_ids_for_folder, session_by_id, session_folder_key};
use roost_protocol::wire::SessionStatus;

/// One session a UI command may act on, and the folder bucket it lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenUiSession {
    /// The session id.
    pub session_id: String,
    /// Its folder bucket, from its live cwd.
    pub folder_key: String,
    /// Whether the coordinator has admitted it. A placeholder, or an admitted
    /// spawn this browser has not yet reconciled, is not: its id must not
    /// cross a wire as if the fleet knew it.
    pub authoritative: bool,
}

/// A folder's live membership, split by whether the fleet knows each id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderMembership {
    /// The ids the coordinator admitted, oldest first.
    pub authoritative_session_ids: Vec<String>,
    /// Whether any member exists only in this browser.
    pub has_client_only_session: bool,
}

/// The open session `session_id` names: an open row, or a placeholder this
/// browser is holding. `None` for an unknown or closed id.
pub fn open_ui_session(
    store: &Store,
    paths: &dyn WorkerPaths,
    session_id: &str,
) -> Option<OpenUiSession> {
    if let Some(session) = session_by_id(store, session_id) {
        return (session.status == SessionStatus::Open).then(|| OpenUiSession {
            session_id: session_id.to_owned(),
            folder_key: session_folder_key(store, paths, session),
            authoritative: !store.spawns.is_client_only(session_id),
        });
    }
    store
        .spawns
        .client_only_sessions()
        .into_iter()
        .find(|placeholder| placeholder.id == session_id)
        .map(|placeholder| OpenUiSession {
            folder_key: placeholder_folder_key(store, paths, &placeholder),
            session_id: placeholder.id,
            authoritative: false,
        })
}

/// Every live member of a folder, placeholders included, oldest first with
/// the id breaking a tie -- the list v2's deck and arrange presets read.
pub fn folder_live_session_ids(
    store: &Store,
    paths: &dyn WorkerPaths,
    folder_key: &str,
) -> Vec<String> {
    let mut members: Vec<(i64, String)> = live_session_ids_for_folder(store, paths, folder_key)
        .into_iter()
        .map(|session_id| {
            let created_at = session_by_id(store, &session_id).map_or(0, |row| row.created_at);
            (created_at, session_id)
        })
        .collect();
    for placeholder in store.spawns.client_only_sessions() {
        let already_listed = members.iter().any(|(_, id)| *id == placeholder.id);
        if already_listed
            || is_pending_close(store, &placeholder.id)
            || placeholder_folder_key(store, paths, &placeholder) != folder_key
        {
            continue;
        }
        members.push((placeholder.created_at_ms, placeholder.id));
    }
    members.sort();
    members.into_iter().map(|(_, session_id)| session_id).collect()
}

/// A folder's membership with this browser's own ids set apart.
pub fn project_folder_membership(
    store: &Store,
    paths: &dyn WorkerPaths,
    folder_key: &str,
) -> FolderMembership {
    let mut membership = FolderMembership::default();
    for session_id in folder_live_session_ids(store, paths, folder_key) {
        if store.spawns.is_client_only(&session_id) {
            membership.has_client_only_session = true;
        } else {
            membership.authoritative_session_ids.push(session_id);
        }
    }
    membership
}

/// The folder an acknowledged apply may arrange: the one the viewed session
/// lives in, with its membership projected. `None` when the route shows no
/// open session.
pub fn layout_apply_folder(
    store: &Store,
    paths: &dyn WorkerPaths,
    active_session_id: Option<&str>,
) -> Option<LayoutApplyFolder> {
    let active = open_ui_session(store, paths, active_session_id?)?;
    let membership = project_folder_membership(store, paths, &active.folder_key);
    Some(LayoutApplyFolder {
        folder_key: active.folder_key,
        active_session_id: active.session_id,
        live_session_ids: membership.authoritative_session_ids,
        has_client_only_session: membership.has_client_only_session,
    })
}

fn placeholder_folder_key(
    store: &Store,
    paths: &dyn WorkerPaths,
    placeholder: &ClientOnlySession,
) -> String {
    let worker_os = store
        .workers
        .get(placeholder.worker_fp.as_str())
        .map(|worker| worker.os.as_str());
    folder_key_of(paths, worker_os, &placeholder.worker_fp, &placeholder.cwd)
}
