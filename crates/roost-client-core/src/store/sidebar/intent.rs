//! The sidebar's user actions, as one event the pump dispatches: the view
//! selector, the drawer and rail, the Cmd-F reveal, a row's navigation
//! bookkeeping, the keyboard cursor, and the visit memory. The actions of
//! `apps/web/src/store/uiStore.ts`, `SidebarRoot.tsx`'s shortcut, and the row
//! click handlers of `FolderList.tsx`, `SessionRow.tsx` and `SidebarAgents.tsx`.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::sidebar::memory::remember_last_workspace;
use crate::store::ui::{
    SidebarView, close_sidebar, open_sidebar, set_sidebar_view, toggle_sidebar_collapsed,
};

/// A sidebar action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarIntent {
    /// Show the Folders or Agents projection (the selector buttons).
    SetView(SidebarView),
    /// Open the compact drawer.
    OpenDrawer,
    /// Close the compact drawer.
    CloseDrawer,
    /// Flip the desktop rail.
    ToggleCollapsed,
    /// The Cmd-F shortcut: make the search box reachable.
    RevealForSearch {
        /// Whether the window is in the compact size class.
        compact: bool,
    },
    /// A row click is about to navigate to `session_id`.
    RecordNavigation {
        /// The destination.
        session_id: String,
        /// `(worker_fp, workspace_id)` to remember, for a session row that has one.
        last_workspace: Option<(String, String)>,
        /// Close the compact drawer on the tap (a same-route navigate fires no
        /// route change that would close it).
        close_drawer: bool,
    },
    /// The Folders panel's rendered row order; empty while it is inactive.
    PublishCursorTargets(Vec<String>),
    /// ↑/↓ by `delta` rows.
    MoveCursor(i32),
    /// A live terminal is on screen.
    RememberVisit {
        /// The session.
        session_id: String,
        /// Its machine.
        worker_fp: String,
        /// Its spawn folder (its cwd when it has none).
        folder: String,
        /// Its stable href, for boot restore.
        href: String,
    },
}

impl SidebarIntent {
    /// A stable name for the transition log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::SetView(_) => "set_view",
            Self::OpenDrawer => "open_drawer",
            Self::CloseDrawer => "close_drawer",
            Self::ToggleCollapsed => "toggle_collapsed",
            Self::RevealForSearch { .. } => "reveal_for_search",
            Self::RecordNavigation { .. } => "record_navigation",
            Self::PublishCursorTargets(_) => "publish_cursor_targets",
            Self::MoveCursor(_) => "move_cursor",
            Self::RememberVisit { .. } => "remember_visit",
        }
    }
}

/// Apply one sidebar action to the store, persisting what persists.
pub fn apply_sidebar_intent(
    store: &mut Store,
    storage: &dyn KeyValueStore,
    intent: &SidebarIntent,
) {
    let changed = match intent {
        SidebarIntent::SetView(view) => set_sidebar_view(store, storage, *view),
        SidebarIntent::OpenDrawer => open_sidebar(store),
        SidebarIntent::CloseDrawer => close_sidebar(store),
        SidebarIntent::ToggleCollapsed => toggle_sidebar_collapsed(store, storage),
        SidebarIntent::RevealForSearch { compact } => reveal_for_search(store, storage, *compact),
        SidebarIntent::RecordNavigation {
            session_id,
            last_workspace,
            close_drawer,
        } => {
            if let Some((worker_fp, workspace_id)) = last_workspace {
                remember_last_workspace(storage, worker_fp, workspace_id);
            }
            store.sidebar.memory.push_recent(storage, session_id);
            *close_drawer && close_sidebar(store)
        }
        SidebarIntent::PublishCursorTargets(ids) => {
            store.sidebar.cursor.set_ordered_session_ids(ids.clone())
        }
        SidebarIntent::MoveCursor(delta) => store.sidebar.cursor.move_cursor(*delta),
        SidebarIntent::RememberVisit {
            session_id,
            worker_fp,
            folder,
            href,
        } => store
            .sidebar
            .memory
            .remember_visit(storage, session_id, worker_fp, folder, href),
    };
    let cursor_or_memory = matches!(
        intent,
        SidebarIntent::PublishCursorTargets(_)
            | SidebarIntent::MoveCursor(_)
            | SidebarIntent::RememberVisit { .. }
    );
    if changed && cursor_or_memory {
        store.note_change();
    }
    tracing::debug!(target: "sidebar", action = intent.kind_name(), changed, "sidebar intent");
}

/// Cmd-F: a compact window opens its closed drawer; a desktop window expands
/// its collapsed rail. Anything already visible is left alone.
fn reveal_for_search(store: &mut Store, storage: &dyn KeyValueStore, compact: bool) -> bool {
    if compact {
        !store.ui.sidebar_open && open_sidebar(store)
    } else {
        store.ui.sidebar_collapsed && toggle_sidebar_collapsed(store, storage)
    }
}
