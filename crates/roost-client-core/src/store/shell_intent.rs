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
use crate::store::prefs::notify::NotifyPref;
use crate::store::prefs::terminal_font::reset_term_font_px;
use crate::store::prefs::{
    set_copy_on_select, set_keyboard_resize, set_keyterm_biasing, set_mouse_forward,
    set_notify_pref, set_predict_mode, set_term_font_px, set_terminal_bell, step_term_font_px,
};
use crate::store::shell_dialogs::RenameDialogRequest;
use crate::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};
use crate::store::ui::set_sidebar_width;

/// The host-fact key a shell failure card is raised under.
pub const SHELL_FAILURE_TOAST: &str = "shell.action_failed";
/// The toast source name for a completed action, beside [`SHELL_FAILURE_TOAST`].
pub const SHELL_SUCCESS_TOAST: &str = "shell.action_succeeded";
/// The toast source name a raised warning card carries.
pub const SHELL_WARNING_TOAST: &str = "shell.warning";

/// A shell action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellIntent {
    /// Resize the desktop sidebar (clamped by the store).
    SetSidebarWidth {
        /// Pixels.
        px: u32,
    },
    /// Put a terminal selection on the clipboard when the drag ends.
    SetCopyOnSelect {
        /// The new value.
        on: bool,
    },
    /// Shrink the shell for the soft keyboard instead of pushing content up.
    SetKeyboardResize {
        /// The new value.
        on: bool,
    },
    /// Let pointer and touch gestures reach the fullscreen application.
    SetMouseForward {
        /// The new value.
        on: bool,
    },
    /// Bias dictation toward the terminal's on-screen jargon.
    SetKeytermBiasing {
        /// The new value.
        on: bool,
    },
    /// Choose how loudly a blocked or finished agent interrupts.
    SetNotifyPref {
        /// Which switch.
        pref: NotifyPref,
        /// The new value.
        value: bool,
    },
    /// Choose how much speculative echo the terminal paints.
    SetPredictMode {
        /// The raw stored spelling a control produced; normalised on the way in.
        value: String,
    },
    /// Choose what a terminal BEL does on this device.
    SetTerminalBell {
        /// The stored spelling a control produced; unknown values read as Visual.
        value: String,
    },
    /// The operator has seen a session: drop its unseen-bell mark.
    ClearTerminalBell {
        /// The session now on screen.
        session_id: String,
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
    /// A completed action worth one line of confirmation.
    ActionSucceeded {
        /// What the card says.
        message: String,
    },
    /// Take one answered pair request off the approver's list.
    ///
    /// The row is the approver's own decision to act on, and the Sync pair
    /// domain confirms it a round trip later. Without this the request an
    /// approver has already bound a code to sits in their list until the next
    /// snapshot, which reads as "did my click do anything".
    DismissPairRequest {
        /// The coordinator's one-shot id for this request.
        ephemeral_id: String,
    },
    /// The touch/controller terminal key sheet, toggled.
    ///
    /// The sheet's own button and the controller's `keypad` / `activate`
    /// intents are the same action, so both land here rather than in a second
    /// store path.
    ToggleNavPad,
    /// The terminal key sheet, closed. The ONE close path, so the mounted
    /// sheet's latched Ctrl and Alt drop with it.
    CloseNavPad,
    /// Raise a warning card.
    ShowWarning {
        /// What the card says.
        message: String,
    },
}

impl ShellIntent {
    /// A stable name for the transition log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::SetCopyOnSelect { .. } => "set_copy_on_select",
            Self::SetKeyboardResize { .. } => "set_keyboard_resize",
            Self::SetMouseForward { .. } => "set_mouse_forward",
            Self::SetKeytermBiasing { .. } => "set_keyterm_biasing",
            Self::SetNotifyPref { .. } => "set_notify_pref",
            Self::SetPredictMode { .. } => "set_predict_mode",
            Self::SetTerminalBell { .. } => "set_terminal_bell",
            Self::ClearTerminalBell { .. } => "clear_terminal_bell",
            Self::SetSidebarWidth { .. } => "set_sidebar_width",
            Self::StepTermFont { .. } => "step_term_font",
            Self::ResetTermFont { .. } => "reset_term_font",
            Self::SetTermFont { .. } => "set_term_font",
            Self::OpenRenameDialog(_) => "open_rename_dialog",
            Self::CloseRenameDialog => "close_rename_dialog",
            Self::OpenQueueTaskDialog { .. } => "open_queue_task_dialog",
            Self::CloseQueueTaskDialog => "close_queue_task_dialog",
            Self::ActionFailed { .. } => "action_failed",
            Self::ActionSucceeded { .. } => "action_succeeded",
            Self::DismissPairRequest { .. } => "dismiss_pair_request",
            Self::ToggleNavPad => "toggle_nav_pad",
            Self::CloseNavPad => "close_nav_pad",
            Self::ShowWarning { .. } => "show_warning",
        }
    }
}

/// Apply one shell action, persisting what persists; one revision when
/// anything moved.
pub fn apply_shell_intent(
    store: &mut Store,
    storage: &dyn KeyValueStore,
    intent: &ShellIntent,
    now_ms: u64,
) {
    let changed = match intent {
        ShellIntent::SetSidebarWidth { px } => set_sidebar_width(store, storage, *px),
        ShellIntent::SetCopyOnSelect { on } => set_copy_on_select(store, storage, *on),
        ShellIntent::SetKeyboardResize { on } => set_keyboard_resize(store, storage, *on),
        ShellIntent::SetMouseForward { on } => set_mouse_forward(store, storage, *on),
        ShellIntent::SetKeytermBiasing { on } => set_keyterm_biasing(store, storage, *on),
        ShellIntent::SetNotifyPref { pref, value } => {
            set_notify_pref(store, storage, *pref, *value)
        }
        ShellIntent::SetPredictMode { value } => set_predict_mode(store, storage, value),
        ShellIntent::SetTerminalBell { value } => set_terminal_bell(
            store,
            storage,
            crate::store::prefs::TerminalBell::parse(Some(value)),
        ),
        ShellIntent::ClearTerminalBell { session_id } => {
            let cleared = store.terminal_bells.clear(session_id);
            if cleared {
                store.note_change();
            }
            cleared
        }
        ShellIntent::StepTermFont { delta } => step_term_font_px(store, storage, *delta),
        ShellIntent::ResetTermFont { default_px } => {
            reset_term_font_px(store, storage, *default_px)
        }
        ShellIntent::SetTermFont { px } => set_term_font_px(store, storage, *px),
        ShellIntent::OpenRenameDialog(request) => {
            note(store, |dialogs| dialogs.open_rename(request.clone()))
        }
        ShellIntent::CloseRenameDialog => note(store, |dialogs| dialogs.close_rename()),
        ShellIntent::OpenQueueTaskDialog {
            cwd,
            body,
            worker_fp,
        } => note(store, |dialogs| {
            dialogs.open_queue_task(cwd.clone(), body.clone(), worker_fp.clone())
        }),
        ShellIntent::CloseQueueTaskDialog => note(store, |dialogs| dialogs.close_queue_task()),
        ShellIntent::ToggleNavPad => {
            crate::store::terminal_nav_pad::toggle_terminal_nav_pad(store, storage)
        }
        ShellIntent::CloseNavPad => {
            crate::store::terminal_nav_pad::close_terminal_nav_pad(store, storage)
        }
        ShellIntent::ShowWarning { message } => {
            add_toast(
                store,
                ToastId::new(
                    ToastSource::Host {
                        name: SHELL_WARNING_TOAST,
                    },
                    message.clone(),
                ),
                message.clone(),
                ToastKind::Warn,
                ToastOptions::plain(),
                now_ms,
            );
            true
        }
        ShellIntent::ActionFailed { message } => {
            add_toast(
                store,
                ToastId::new(
                    ToastSource::Host {
                        name: SHELL_FAILURE_TOAST,
                    },
                    message.clone(),
                ),
                message.clone(),
                ToastKind::Err,
                ToastOptions::plain(),
                now_ms,
            );
            true
        }
        ShellIntent::ActionSucceeded { message } => {
            add_toast(
                store,
                ToastId::new(
                    ToastSource::Host {
                        name: SHELL_SUCCESS_TOAST,
                    },
                    message.clone(),
                ),
                message.clone(),
                ToastKind::Ok,
                ToastOptions::plain(),
                now_ms,
            );
            true
        }
        ShellIntent::DismissPairRequest { ephemeral_id } => {
            let removed = crate::store::mutations::delete_pair_request(store, ephemeral_id);
            if removed {
                tracing::info!(
                    target: "shell",
                    ephemeral_id,
                    "an answered pair request left the approver's list"
                );
            }
            removed
        }
    };
    tracing::info!(target: "shell", intent = intent.kind_name(), changed, "shell intent");
}

fn note(
    store: &mut Store,
    change: impl FnOnce(&mut crate::store::shell_dialogs::ShellDialogs) -> bool,
) -> bool {
    let changed = change(&mut store.shell_dialogs);
    if changed {
        store.note_change();
    }
    changed
}
