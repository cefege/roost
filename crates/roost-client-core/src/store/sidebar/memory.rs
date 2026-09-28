//! Browser-local "where was I" memory the sidebar writes: the MRU session
//! stack, the last terminal viewed plus the last session per folder, and the
//! last workspace per machine. Ports `apps/web/src/lib/sidebarRecent.ts`,
//! `apps/web/src/lib/lastVisited.ts` and `apps/web/src/lib/lastWorkspace.ts`.
//! Row clicks write it; FolderList, MainPane and the boot redirect read it.

use std::collections::BTreeMap;

use crate::platform::KeyValueStore;

/// v2's MRU key.
pub const RECENT_KEY: &str = "roost.sidebar.recent";
/// How many recent sessions the stack keeps.
pub const RECENT_MAX: usize = 5;
/// The boot-restore path key.
pub const LAST_TERMINAL_PATH_KEY: &str = "roost.lastTerminalPath";
/// The per-folder last-session map key.
pub const LAST_SESSION_BY_FOLDER_KEY: &str = "roost.lastSessionByFolder";
/// The per-machine last-workspace key prefix.
pub const LAST_WORKSPACE_KEY_PREFIX: &str = "roost.lastWorkspaceId.";

/// The sidebar's persisted memory, loaded once per document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarMemory {
    recent: Vec<String>,
    session_by_folder: BTreeMap<String, String>,
    last_path_written: Option<String>,
}

impl SidebarMemory {
    /// Read the stored stack and folder map. A value some other build wrote
    /// that does not parse is treated as absent, never trusted.
    pub fn load(storage: &dyn KeyValueStore) -> Self {
        let recent = storage
            .get(RECENT_KEY)
            .and_then(|raw| serde_json::from_str::<Vec<serde_json::Value>>(&raw).ok())
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let session_by_folder = storage
            .get(LAST_SESSION_BY_FOLDER_KEY)
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|value| value.as_object().cloned())
            .map(|object| {
                object
                    .into_iter()
                    .filter_map(|(key, value)| value.as_str().map(|id| (key, id.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            recent,
            session_by_folder,
            last_path_written: None,
        }
    }

    /// The MRU stack, newest first.
    pub fn recent(&self) -> &[String] {
        &self.recent
    }

    /// Move `session_id` to the top of the stack, deduped and capped, and
    /// persist it.
    pub fn push_recent(&mut self, storage: &dyn KeyValueStore, session_id: &str) {
        self.recent.retain(|id| id != session_id);
        self.recent.insert(0, session_id.to_owned());
        self.recent.truncate(RECENT_MAX);
        if let Ok(encoded) = serde_json::to_string(&self.recent) {
            storage.set(RECENT_KEY, &encoded);
        }
    }

    /// Record the terminal on screen: its stable href for boot restore, and
    /// it as the last session of its spawn folder. Both writes are skipped when
    /// the stored value is already this one. Returns whether the folder map
    /// changed.
    pub fn remember_visit(
        &mut self,
        storage: &dyn KeyValueStore,
        session_id: &str,
        worker_fp: &str,
        folder: &str,
        href: &str,
    ) -> bool {
        if self.last_path_written.as_deref() != Some(href) {
            storage.set(LAST_TERMINAL_PATH_KEY, href);
            self.last_path_written = Some(href.to_owned());
        }
        let key = folder_memory_key(worker_fp, folder);
        if self.session_by_folder.get(&key).map(String::as_str) == Some(session_id) {
            return false;
        }
        self.session_by_folder.insert(key, session_id.to_owned());
        if let Ok(encoded) = serde_json::to_string(&self.session_by_folder) {
            storage.set(LAST_SESSION_BY_FOLDER_KEY, &encoded);
        }
        true
    }

    /// The session last viewed in a folder, if one was recorded.
    pub fn last_session_for_folder(&self, worker_fp: &str, folder: &str) -> Option<&str> {
        self.session_by_folder
            .get(&folder_memory_key(worker_fp, folder))
            .map(String::as_str)
    }
}

/// The last terminal path this browser viewed; `/` reads as none.
pub fn last_terminal_path(storage: &dyn KeyValueStore) -> Option<String> {
    storage
        .get(LAST_TERMINAL_PATH_KEY)
        .filter(|path| !path.is_empty() && path != "/")
}

/// Remember the workspace last used on a machine.
pub fn remember_last_workspace(storage: &dyn KeyValueStore, worker_fp: &str, workspace_id: &str) {
    storage.set(&format!("{LAST_WORKSPACE_KEY_PREFIX}{worker_fp}"), workspace_id);
}

/// `worker_fp NUL folder`: NUL cannot appear in either half, so two pairs
/// never share a key.
fn folder_memory_key(worker_fp: &str, folder: &str) -> String {
    format!("{worker_fp}\u{0}{folder}")
}
