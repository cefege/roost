//! Where the controller's next-folder press lands. A pad has no pointer to
//! click a sidebar folder row, so "switch folders" has to resolve to one
//! session id. Pure over a folder list, so the cycling contract has exactly one
//! implementation. Called by `pad_router`; the list comes from the shell's
//! folder groups through `PadSurfaces::folder_cycle`.
//! Ported from `apps/web/src/lib/padFolders.ts`.

/// One sidebar folder, as far as the cycle cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderLead {
    /// The folder key.
    pub key: String,
    /// The folder's most-recently-active session — the same target a sidebar
    /// folder click opens.
    pub lead_id: String,
}

/// The session to open for the folder AFTER `current_folder_key` in `folders`
/// (already ordered by latest activity, newest first), cycling at the end.
/// `None` when there is nowhere to go.
pub fn next_folder_session_id<'folders>(
    folders: &'folders [FolderLead],
    current_folder_key: Option<&str>,
) -> Option<&'folders str> {
    // One folder is not a cycle, whatever the pad is currently showing: the
    // button exists to move BETWEEN folders, and re-navigating to the session
    // already open would rebuild the pane deck for no visible change.
    if folders.len() < 2 {
        return None;
    }
    let current =
        current_folder_key.and_then(|key| folders.iter().position(|folder| folder.key == key));
    // An unknown current folder — a non-session route, or a folder whose
    // sessions have all closed — still has somewhere to go: the newest one.
    let next = current.map_or(0, |index| (index + 1) % folders.len());
    folders.get(next).map(|folder| folder.lead_id.as_str())
}
