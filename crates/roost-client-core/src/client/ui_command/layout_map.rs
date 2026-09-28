//! Maps one layout-shaped legacy UI command onto a folder's pane arrangement.
//!
//! Ports `applyUiCommandToLayout` from `apps/web/src/lib/uiCommandCore.ts`.
//! Pure: the deck host resolves the folder's arrangement, calls this, and
//! commits what comes back. A bad reference or argument returns `None` with the
//! input untouched; the acknowledged apply never reaches this mapper.

use roost_protocol::layout::document::LayoutDirection;

use super::command::LegacyUiCommand;
use crate::store::layout::{
    ArrangeKind, LayoutRecords, PaneIdSource, PaneLayout, arrange_layout, find_leaf_of_tab,
    focus_pane, move_tab, select_tab, split_leaf,
};

/// What a reshape did to the folder's records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutReshape {
    /// Committed. `navigate_to_session` is where the tab must go next.
    Committed { navigate_to_session: Option<String> },
    /// The command did not map onto the resolved arrangement; nothing was
    /// written.
    Refused,
}

/// Resolve the folder's arrangement, map `command` onto it, and commit the
/// result: v2's `applyPure`, for `UiCommandAction::ReshapeLayout`.
pub fn reshape_folder_layout(
    records: &mut LayoutRecords,
    ids: &mut dyn PaneIdSource,
    folder_key: &str,
    live_session_ids: &[String],
    command: &LegacyUiCommand,
) -> LayoutReshape {
    let resolved = records.resolve(folder_key, live_session_ids, ids);
    let Some(next) = apply_ui_command_to_layout(&resolved, command, live_session_ids, ids) else {
        tracing::warn!(target: "ui_cc", kind = command.kind(), folder_key, "ui_command_unknown_session");
        return LayoutReshape::Refused;
    };
    records.commit(folder_key, next);
    // splitLeaf already focused the placed session's pane; the route follows it.
    let navigate_to_session = match command {
        LegacyUiCommand::PlaceSplit { session_id, .. } => Some(session_id.clone()),
        _ => None,
    };
    LayoutReshape::Committed {
        navigate_to_session,
    }
}

/// Apply `command` to `layout`, or `None` when it names a session the layout
/// does not hold, an invalid direction or preset, or is shell-owned
/// (navigate, close, spotlight). `live_session_ids` feeds arrange's
/// one-pane-per-live-session presets.
pub fn apply_ui_command_to_layout(
    layout: &PaneLayout,
    command: &LegacyUiCommand,
    live_session_ids: &[String],
    ids: &mut dyn PaneIdSource,
) -> Option<PaneLayout> {
    match command {
        LegacyUiCommand::PlaceSplit {
            session_id,
            anchor_session_id,
            dir,
            insert_first,
        } => {
            let anchor = find_leaf_of_tab(&layout.root, anchor_session_id)?;
            if session_id.is_empty() {
                return None;
            }
            let direction = LayoutDirection::parse("dir", dir).ok()?;
            Some(split_leaf(
                layout,
                &anchor.pane_id,
                direction,
                session_id,
                *insert_first,
                ids,
            ))
        }
        LegacyUiCommand::SelectTab { session_id } => {
            find_leaf_of_tab(&layout.root, session_id)?;
            Some(select_tab(layout, session_id))
        }
        LegacyUiCommand::FocusPane { session_id } => {
            let leaf = find_leaf_of_tab(&layout.root, session_id)?;
            Some(focus_pane(layout, &leaf.pane_id))
        }
        LegacyUiCommand::MoveTab {
            session_id,
            dest_session_id,
        } => {
            let destination = find_leaf_of_tab(&layout.root, dest_session_id)?;
            find_leaf_of_tab(&layout.root, session_id)?;
            Some(move_tab(layout, session_id, &destination.pane_id, None))
        }
        LegacyUiCommand::Arrange { preset } => {
            let kind = ArrangeKind::parse(preset)?;
            Some(arrange_layout(kind, layout, live_session_ids, ids))
        }
        LegacyUiCommand::Navigate { .. }
        | LegacyUiCommand::CloseTab { .. }
        | LegacyUiCommand::Spotlight { .. } => None,
    }
}
