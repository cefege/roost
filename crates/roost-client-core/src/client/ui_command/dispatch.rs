//! Drains the Sync-queued UI commands into the actions the browser shell
//! executes, resolving every session reference against the store first.
//!
//! Ports `handleUiCommand` from `apps/web/src/lib/uiCommandDispatch.ts`. The
//! UI bridge (roost-web SHELL) calls `drain_ui_commands` after each pump
//! revision; spotlight is store state and is applied here, everything that
//! needs the router or the deck's layout records comes back as an action.
//! Frames are fire-and-forget: a bad reference is one warn line, never an error.

use super::command::{InboundUiCommand, LegacyUiCommand, legacy_frame_accepted, read_ui_command_frame};
use super::membership::{OpenUiSession, folder_live_session_ids, open_ui_session};
use crate::client::ui_state::LayoutApplyCommand;
use crate::store::Store;
use crate::store::paths::WorkerPaths;
use crate::store::spotlight::{clear_spotlight, set_spotlight_session_id};

/// What the draining tab is, at the moment it drains.
pub struct UiCommandScope<'scope> {
    /// This tab's id, for legacy targeting.
    pub own_tab_id: &'scope str,
    /// The path codec folder keys are computed with.
    pub paths: &'scope dyn WorkerPaths,
    /// The session the current route resolves to through the host's route
    /// table, if any. Arrange targets this session's folder.
    pub active_session_id: Option<&'scope str>,
}

impl std::fmt::Debug for UiCommandScope<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiCommandScope")
            .field("own_tab_id", &self.own_tab_id)
            .field("active_session_id", &self.active_session_id)
            .finish_non_exhaustive()
    }
}

/// One thing the shell must do for a drained command.
#[derive(Debug, Clone, PartialEq)]
pub enum UiCommandAction {
    /// Route the tab to `path`.
    Navigate { path: String },
    /// Select a tab exactly as a strip click does: select and focus its pane,
    /// navigate to it, and let a floated card follow an in-pane swap.
    SelectTab {
        folder_key: String,
        session_id: String,
    },
    /// Focus the pane holding `session_id` and navigate to the tab THAT pane
    /// shows, which may be a different session.
    FocusPane {
        folder_key: String,
        session_id: String,
    },
    /// Soft-close a session exactly as the tab ✕ does: undo window, deferred
    /// kill, and the view landing on the tab the closed layout focuses.
    CloseTab {
        folder_key: String,
        session_id: String,
    },
    /// Resolve the folder's arrangement, map `command` onto it, commit, and
    /// navigate where the reshape says: `reshape_folder_layout` does all of it.
    ReshapeLayout {
        folder_key: String,
        live_session_ids: Vec<String>,
        command: LegacyUiCommand,
    },
    /// An acknowledged apply, for `execute_targeted_layout_apply` (or, with no
    /// router mounted, `reject_layout_apply_without_bridge`). Never filtered by
    /// legacy targeting: only its exact tab and socket may answer it.
    ApplyLayout(LayoutApplyCommand),
}

/// Pop every queued UI command and return the actions, oldest first.
pub fn drain_ui_commands(store: &mut Store, scope: &UiCommandScope<'_>) -> Vec<UiCommandAction> {
    let mut actions = Vec::new();
    while let Some(frame) = store.ui_commands.pop_front() {
        let Some(inbound) = read_ui_command_frame(&frame) else {
            tracing::debug!(target: "ui_cc", "ui command frame named no command");
            continue;
        };
        let action = match inbound {
            InboundUiCommand::ApplyLayout(command) => Some(UiCommandAction::ApplyLayout(command)),
            InboundUiCommand::Legacy {
                target_tab_id,
                command,
            } => {
                if !legacy_frame_accepted(&target_tab_id, scope.own_tab_id) {
                    tracing::debug!(target: "ui_cc", %target_tab_id, kind = command.kind(), "ui command addressed to another tab");
                    continue;
                }
                legacy_action(store, scope, command)
            }
        };
        if let Some(action) = action {
            tracing::info!(target: "ui_cc", action = action_kind(&action), "ui command dispatched");
            actions.push(action);
        }
    }
    actions
}

fn legacy_action(
    store: &mut Store,
    scope: &UiCommandScope<'_>,
    command: LegacyUiCommand,
) -> Option<UiCommandAction> {
    let open = |store: &Store, session_id: &str| open_ui_session(store, scope.paths, session_id);
    match command {
        LegacyUiCommand::Navigate { path } => {
            (!path.is_empty()).then_some(UiCommandAction::Navigate { path })
        }
        LegacyUiCommand::Spotlight { off: true, .. } => {
            clear_spotlight(store);
            None
        }
        LegacyUiCommand::Spotlight { session_id, .. } => {
            let session = known(open(store, &session_id), "spotlight", &session_id)?;
            set_spotlight_session_id(store, Some(session.session_id));
            None
        }
        LegacyUiCommand::CloseTab { session_id } => {
            let session = known(open(store, &session_id), "close_tab", &session_id)?;
            Some(UiCommandAction::CloseTab {
                folder_key: session.folder_key,
                session_id: session.session_id,
            })
        }
        LegacyUiCommand::SelectTab { session_id } => {
            let session = known(open(store, &session_id), "select_tab", &session_id)?;
            Some(UiCommandAction::SelectTab {
                folder_key: session.folder_key,
                session_id: session.session_id,
            })
        }
        LegacyUiCommand::FocusPane { session_id } => {
            let session = known(open(store, &session_id), "focus_pane", &session_id)?;
            Some(UiCommandAction::FocusPane {
                folder_key: session.folder_key,
                session_id: session.session_id,
            })
        }
        LegacyUiCommand::PlaceSplit {
            ref session_id,
            ref anchor_session_id,
            ..
        } => {
            // Both ends must be live: the anchor names the folder bucket, and a
            // not-yet-synced session would be pruned by the next reconcile.
            let anchor = known(open(store, anchor_session_id), "place_split", anchor_session_id)?;
            known(open(store, session_id), "place_split", session_id)?;
            Some(reshape(store, scope, anchor.folder_key, command))
        }
        LegacyUiCommand::MoveTab {
            ref session_id,
            ref dest_session_id,
        } => {
            let destination = known(open(store, dest_session_id), "move_tab", dest_session_id)?;
            known(open(store, session_id), "move_tab", session_id)?;
            Some(reshape(store, scope, destination.folder_key, command))
        }
        LegacyUiCommand::Arrange { .. } => {
            // No session in the command: arrange targets the folder being viewed.
            let active = scope.active_session_id.and_then(|session_id| open(store, session_id));
            let active = known(active, "arrange", scope.active_session_id.unwrap_or_default())?;
            Some(reshape(store, scope, active.folder_key, command))
        }
    }
}

fn reshape(
    store: &Store,
    scope: &UiCommandScope<'_>,
    folder_key: String,
    command: LegacyUiCommand,
) -> UiCommandAction {
    UiCommandAction::ReshapeLayout {
        live_session_ids: folder_live_session_ids(store, scope.paths, &folder_key),
        folder_key,
        command,
    }
}

/// One warn per dropped frame, naming the reference that did not resolve.
fn known(session: Option<OpenUiSession>, kind: &str, session_id: &str) -> Option<OpenUiSession> {
    if session.is_none() {
        tracing::warn!(target: "ui_cc", kind, session_id, "ui_command_unknown_session");
    }
    session
}

fn action_kind(action: &UiCommandAction) -> &'static str {
    match action {
        UiCommandAction::Navigate { .. } => "navigate",
        UiCommandAction::SelectTab { .. } => "select_tab",
        UiCommandAction::FocusPane { .. } => "focus_pane",
        UiCommandAction::CloseTab { .. } => "close_tab",
        UiCommandAction::ReshapeLayout { command, .. } => command.kind(),
        UiCommandAction::ApplyLayout(_) => "apply_layout",
    }
}
