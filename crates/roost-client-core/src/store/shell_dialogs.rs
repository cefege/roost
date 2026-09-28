//! The two authenticated dialogs the shell hosts: "Rename…" (one active request,
//! opened from a session or folder row's menu) and "Queue a task" (open flag and
//! its prefill). Ports `apps/web/src/store/renameDialog.ts` and
//! `apps/web/src/store/queueTaskDialog.ts`; mutated through
//! `shell_intent::ShellIntent`, read by roost-web's `RenameDialogHost` and the
//! queue-task dialog, and emptied at every credential boundary because both can
//! hold a path, a name or an action from the retired account.

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

/// The "Queue a task" dialog's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueTaskDialog {
    /// Whether it is open.
    pub open: bool,
    /// The folder to prefill.
    pub prefill_cwd: Option<String>,
    /// The task body to prefill.
    pub prefill_body: Option<String>,
    /// The machine to pin.
    pub prefill_worker_fp: Option<String>,
}

/// Both dialogs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellDialogs {
    /// The active rename request.
    pub rename: Option<RenameDialogRequest>,
    /// The queue-task dialog.
    pub queue_task: QueueTaskDialog,
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

    /// Open the queue-task dialog with its prefill (each absent field cleared).
    pub fn open_queue_task(
        &mut self,
        cwd: Option<String>,
        body: Option<String>,
        worker_fp: Option<String>,
    ) -> bool {
        let next = QueueTaskDialog {
            open: true,
            prefill_cwd: cwd,
            prefill_body: body,
            prefill_worker_fp: worker_fp,
        };
        let changed = self.queue_task != next;
        self.queue_task = next;
        changed
    }

    /// Close the queue-task dialog; the prefill stays for the TaskEditor's
    /// unmount, as v2 left it.
    pub fn close_queue_task(&mut self) -> bool {
        std::mem::replace(&mut self.queue_task.open, false)
    }

    /// The credential boundary: close both and forget every captured value.
    pub fn clear_all(&mut self) -> bool {
        let had_any = *self != Self::default();
        *self = Self::default();
        had_any
    }
}
