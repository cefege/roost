//! The deck decisions that may persist a route's selection or a rendered
//! pane's focus into the stored arrangement. Called by `deck::intent` for the
//! route-sync, pane-focus and tab-select intents. Pure; ports
//! `apps/web/src/lib/deckRouteSelection.ts`.
//!
//! A compact host projects ONE pane; when the stored focus is on an empty pane
//! that projection is view-only, so a phone never overwrites desktop focus a
//! layout import or another browser put there.

use crate::store::layout::{PaneLayout, find_leaf, find_leaf_of_tab, select_tab};

/// How a tab click lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSelection {
    /// Persist the selection into the arrangement, then navigate.
    Persist,
    /// Navigate only; the arrangement stays exactly as stored.
    NavigateOnly,
}

fn projects_compact_from_empty_focus(current: Option<&PaneLayout>, compact: bool) -> bool {
    let Some(current) = current else {
        return false;
    };
    compact
        && find_leaf(&current.root, &current.focused_pane_id)
            .is_some_and(|leaf| leaf.tabs.is_empty())
}

/// The arrangement that makes the route's session the selected tab of the
/// focused pane, or `None` when nothing needs committing.
pub fn route_selection_commit(
    current: &PaneLayout,
    active_session_id: &str,
    compact: bool,
) -> Option<PaneLayout> {
    if projects_compact_from_empty_focus(Some(current), compact) {
        return None;
    }
    let leaf = find_leaf_of_tab(&current.root, active_session_id)?;
    if current.focused_pane_id == leaf.pane_id && leaf.selected_tab == active_session_id {
        return None;
    }
    Some(select_tab(current, active_session_id))
}

/// Whether focusing the rendered pane may persist into the arrangement.
pub fn pane_focus_persists(
    current: Option<&PaneLayout>,
    rendered_pane_id: &str,
    compact: bool,
) -> bool {
    let Some(layout) = current else {
        return false;
    };
    if projects_compact_from_empty_focus(current, compact) {
        return false;
    }
    !(compact && layout.focused_pane_id != rendered_pane_id)
}

/// How a tab selection lands for this arrangement.
pub fn session_selection(current: Option<&PaneLayout>, compact: bool) -> SessionSelection {
    if projects_compact_from_empty_focus(current, compact) {
        SessionSelection::NavigateOnly
    } else {
        SessionSelection::Persist
    }
}
