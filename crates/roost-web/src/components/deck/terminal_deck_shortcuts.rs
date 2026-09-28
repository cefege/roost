//! The deck-wide keyboard chords: which press means which deck operation, the
//! painted-geometry walk to the adjacent pane, and the tab a digit selects.
//! Read by `terminal_deck` through `terminal_deck_dom`'s document listener.
//! Pure; ports `apps/web/src/components/deck/terminal-deck-shortcuts.ts`.
//! Every chord not listed here stays with the terminal.

use roost_client_core::store::layout::{ArrangeKind, PaneView, PresetKind};
use roost_protocol::layout::document::LayoutDirection;

use crate::platform::browser_platform::{
    BrowserPlatform, PlatformShortcut, ShortcutKey, matches_platform_shortcut,
};

/// A direction to walk from the focused pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMoveDirection {
    /// Toward smaller x.
    Left,
    /// Toward larger x.
    Right,
    /// Toward smaller y.
    Up,
    /// Toward larger y.
    Down,
}

/// What a deck chord asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum DeckShortcut {
    /// Escape while a pane is floated.
    ClearSpotlight,
    /// Give the keyboard to the nearest pane that way.
    FocusAdjacent(PaneMoveDirection),
    /// A new terminal in the focused pane.
    NewTerminal,
    /// The focused pane's tab N (1–8), or its last tab (9).
    TerminalTab(u8),
    /// Split the focused pane with a fresh session.
    Split(LayoutDirection),
    /// Float or un-float the focused pane.
    Spotlight,
    /// Re-arrange the folder.
    Arrange(ArrangeKind),
}

const ARRANGE_SHORTCUTS: [(PlatformShortcut, ArrangeKind); 5] = [
    (PlatformShortcut::ArrangeBalance, ArrangeKind::Balance),
    (
        PlatformShortcut::ArrangeColumns,
        ArrangeKind::Preset(PresetKind::Even),
    ),
    (
        PlatformShortcut::ArrangeRows,
        ArrangeKind::Preset(PresetKind::Rows),
    ),
    (
        PlatformShortcut::ArrangeGrid,
        ArrangeKind::Preset(PresetKind::Tiled),
    ),
    (
        PlatformShortcut::ArrangeMain,
        ArrangeKind::Preset(PresetKind::MainVertical),
    ),
];

/// The deck chord `key` is, if any. Escape clears a spotlight anywhere; every
/// other chord needs the deck to be the visible surface on a live folder.
pub fn deck_shortcut_for(
    key: &ShortcutKey,
    platform: BrowserPlatform,
    spotlight_active: bool,
    deck_live: bool,
) -> Option<DeckShortcut> {
    if key.key == "Escape" && spotlight_active {
        return Some(DeckShortcut::ClearSpotlight);
    }
    if !deck_live {
        return None;
    }
    let matches = |shortcut| matches_platform_shortcut(key, shortcut, platform);
    let direction = match key.key.as_str() {
        "ArrowLeft" => Some(PaneMoveDirection::Left),
        "ArrowRight" => Some(PaneMoveDirection::Right),
        "ArrowUp" => Some(PaneMoveDirection::Up),
        "ArrowDown" => Some(PaneMoveDirection::Down),
        _ => None,
    };
    if let Some(direction) = direction
        && matches(PlatformShortcut::PaneFocus)
    {
        return Some(DeckShortcut::FocusAdjacent(direction));
    }
    if matches(PlatformShortcut::NewTerminal) {
        return Some(DeckShortcut::NewTerminal);
    }
    if matches(PlatformShortcut::TerminalTab) {
        return key.key.parse::<u8>().ok().map(DeckShortcut::TerminalTab);
    }
    if matches(PlatformShortcut::SplitRight) {
        return Some(DeckShortcut::Split(LayoutDirection::Row));
    }
    if matches(PlatformShortcut::SplitDown) {
        return Some(DeckShortcut::Split(LayoutDirection::Col));
    }
    if matches(PlatformShortcut::Spotlight) {
        return Some(DeckShortcut::Spotlight);
    }
    ARRANGE_SHORTCUTS
        .iter()
        .find(|(shortcut, _)| matches(*shortcut))
        .map(|(_, kind)| DeckShortcut::Arrange(*kind))
}

/// The pane that owns the keyboard: the layout's focused pane when painted,
/// else whichever painted pane is marked focused.
pub fn focused_pane_view<'view>(
    panes: &'view [PaneView],
    focused_pane_id: Option<&str>,
) -> Option<&'view PaneView> {
    panes
        .iter()
        .find(|pane| Some(pane.pane_id.as_str()) == focused_pane_id)
        .or_else(|| panes.iter().find(|pane| pane.focused))
}

/// The tab a digit selects in `pane`: 1–8 by position, 9 the last.
pub fn tab_for_digit(pane: &PaneView, digit: u8) -> Option<&str> {
    let index = if digit == 9 {
        pane.tab_ids.len().checked_sub(1)?
    } else {
        usize::from(digit).checked_sub(1)?
    };
    pane.tab_ids.get(index).map(String::as_str)
}

/// The nearest painted pane whose centre lies `direction` of `source`'s.
pub fn adjacent_pane<'view>(
    panes: &'view [PaneView],
    source: &PaneView,
    direction: PaneMoveDirection,
) -> Option<&'view PaneView> {
    let center_x = source.rect.x + source.rect.w / 2.0;
    let center_y = source.rect.y + source.rect.h / 2.0;
    let mut best: Option<(&PaneView, f64)> = None;
    for pane in panes {
        if pane.pane_id == source.pane_id {
            continue;
        }
        let delta_x = pane.rect.x + pane.rect.w / 2.0 - center_x;
        let delta_y = pane.rect.y + pane.rect.h / 2.0 - center_y;
        let ahead = match direction {
            PaneMoveDirection::Left => delta_x < 0.0,
            PaneMoveDirection::Right => delta_x > 0.0,
            PaneMoveDirection::Up => delta_y < 0.0,
            PaneMoveDirection::Down => delta_y > 0.0,
        };
        let distance = delta_x * delta_x + delta_y * delta_y;
        if ahead && best.is_none_or(|(_, nearest)| distance < nearest) {
            best = Some((pane, distance));
        }
    }
    best.map(|(pane, _)| pane)
}
