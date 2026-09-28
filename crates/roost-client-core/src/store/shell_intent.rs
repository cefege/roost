//! The shell's user actions as one event the pump dispatches: the sidebar
//! resizer's width, terminal zoom, and the rename / queue-task dialogs. The
//! actions of `apps/web/src/components/layout/SidebarResizer.tsx`,
//! `apps/web/src/lib/keyboardShortcuts.ts` (zoom), and
//! `apps/web/src/store/{renameDialog,queueTaskDialog}.ts`. Handled by
//! `handle_event` for `ClientEvent::Shell`; dispatched by roost-web's layout,
//! keyboard router and dialog hosts. The drawer and rail are
//! `sidebar::SidebarIntent`'s.

use crate::platform::KeyValueStore;
use crate::store::Store;
use crate::store::prefs::terminal_font::reset_term_font_px;
use crate::store::prefs::{set_term_font_px, step_term_font_px};
use crate::store::shell_dialogs::RenameDialogRequest;
use crate::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};
use crate::store::ui::set_sidebar_width;

/// The host-fact key a shell failure card is raised under.
pub const SHELL_FAILURE_TOAST: &str = "shell.action_failed";

/// A shell action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellIntent {
    /// Resize the desktop sidebar (clamped by the store).
    SetSidebarWidth {
        /// Pixels.
        px: u32,
    },
    /// Zoom the terminal font by `delta` pixels (clamped).
    StepTermFont {
        /// Signed step.
        delta: i32,
    },
    /// Put the terminal font back to this device's default.
    ResetTermFont {
        /// 14px, or 20px on a TV.
        default_px: u32,
    },
    /// Set the terminal font exactly (settings slider).
    SetTermFont {
        /// Pixels.
        px: u32,
    },
    /// Open the rename dialog.
    OpenRenameDialog(RenameDialogRequest),
    /// Close the rename dialog.
    CloseRenameDialog,
    /// Open "Queue a task" with its prefill.
    OpenQueueTaskDialog {
        /// Folder to prefill.
        cwd: Option<String>,
        /// Body to prefill.
        body: Option<String>,
        /// Machine to pin.
        worker_fp: Option<String>,
    },
    /// Close "Queue a task".
    CloseQueueTaskDialog,
    /// A shell action the coordinator refused; raised as an error card.
    ActionFailed {
        /// What the card says.
        message: String,
    },
}

impl ShellIntent {
    /// A stable name for the transition log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::SetSidebarWidth { .. } => "set_sidebar_width",
            Self::StepTermFont { .. } => "step_term_font",
            Self::ResetTermFont { .. } => "reset_term_font",
            Self::SetTermFont { .. } => "set_term_font",
            Self::OpenRenameDialog(_) => "open_rename_dialog",
            Self::CloseRenameDialog => "close_rename_dialog",
            Self::OpenQueueTaskDialog { .. } => "open_queue_task_dialog",
            Self::CloseQueueTaskDialog => "close_queue_task_dialog",
            Self::ActionFailed { .. } => "action_failed",
        }
    }
}

/// Apply one shell action, persisting what persists; one revision when
/// anything moved.
pub fn apply_shell_intent(store: &mut Store, storage: &dyn KeyValueStore, intent: &ShellIntent, now_ms: u64) {
    let changed = match intent {
        ShellIntent::SetSidebarWidth { px } => set_sidebar_width(store, storage, *px),
        ShellIntent::StepTermFont { delta } => step_term_font_px(store, storage, *delta),
        ShellIntent::ResetTermFont { default_px } => reset_term_font_px(store, storage, *default_px),
        ShellIntent::SetTermFont { px } => set_term_font_px(store, storage, *px),
        ShellIntent::OpenRenameDialog(request) => note(store, |dialogs| dialogs.open_rename(request.clone())),
        ShellIntent::CloseRenameDialog => note(store, |dialogs| dialogs.close_rename()),
        ShellIntent::OpenQueueTaskDialog { cwd, body, worker_fp } => note(store, |dialogs| {
            dialogs.open_queue_task(cwd.clone(), body.clone(), worker_fp.clone())
        }),
        ShellIntent::CloseQueueTaskDialog => note(store, |dialogs| dialogs.close_queue_task()),
        ShellIntent::ActionFailed { message } => {
            add_toast(
                store,
                ToastId::new(ToastSource::Host { name: SHELL_FAILURE_TOAST }, message.clone()),
                message.clone(),
                ToastKind::Err,
                ToastOptions::plain(),
                now_ms,
            );
            true
        }
    };
    tracing::info!(target: "shell", intent = intent.kind_name(), changed, "shell intent");
}

fn note(store: &mut Store, change: impl FnOnce(&mut crate::store::shell_dialogs::ShellDialogs) -> bool) -> bool {
    let changed = change(&mut store.shell_dialogs);
    if changed {
        store.note_change();
    }
    changed
}
