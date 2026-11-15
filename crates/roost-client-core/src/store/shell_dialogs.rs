//! The authenticated dialog the shell hosts: "Rename…" (one active request,
//! opened from a session or folder row's menu). Mutated through
//! `shell_intent::ShellIntent`, read by roost-web's `RenameDialogHost`, and
//! emptied at every credential boundary because it can hold a name from the
//! retired account.

/// What a rename commits to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameTarget {
    /// A session's custom title (`SessionsRename`; empty clears it).
    Session {
        /// The session.
        session_id: String,
    },
    /// The workspace a folder names: update it when one exists, else create it.
    FolderWorkspace {
        /// The folder's machine.
        worker_fp: String,
        /// The folder.
        folder_path: String,
        /// The folder's sessions, attached when the workspace is created.
        session_ids: Vec<String>,
    },
}

/// One open rename request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameDialogRequest {
    /// The field's pre-fill: the custom name, else the automatic one.
    pub current_title: String,
    /// Offer "Reset to auto".
    pub has_custom: bool,
    /// The dialog's title; `None` reads "Rename terminal".
    pub headline: Option<String>,
    /// What the commit changes.
    pub target: RenameTarget,
}

/// The shell's dialogs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellDialogs {
    /// The active rename request.
    pub rename: Option<RenameDialogRequest>,
}

impl ShellDialogs {
    /// Open the rename dialog for `request`, replacing any other.
    pub fn open_rename(&mut self, request: RenameDialogRequest) -> bool {
        let changed = self.rename.as_ref() != Some(&request);
        self.rename = Some(request);
        changed
    }

    /// Close the rename dialog.
    pub fn close_rename(&mut self) -> bool {
        self.rename.take().is_some()
    }

    /// The credential boundary: close it and forget every captured value.
    pub fn clear_all(&mut self) -> bool {
        let had_any = *self != Self::default();
        *self = Self::default();
        had_any
    }
}
