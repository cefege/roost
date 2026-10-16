//! What each key does in the models hub, as a pure ladder over the hub's focus
//! and query state, ported from OMP `model-hub.ts` `handleInput`. The component
//! applies the returned `HubKeyEffect`; Escape clears the query before it
//! closes, and ←/→ are pane switches that never reach the search text.

/// Which pane owns the arrow keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubFocus {
    /// The sidebar: ↑/↓ move between views.
    Scope,
    /// The model rows: ↑/↓ move the selection.
    List,
}

/// The keys the hub reacts to. `Char` carries printable input for search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubKey {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Left,
    Right,
    Enter,
    Escape,
    Tab,
    ShiftTab,
    AltLeft,
    AltRight,
    F5,
    Backspace,
    Char(char),
}

/// What the component should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubKeyEffect {
    /// Move the sidebar by a step (-1 or 1), wrapping.
    MoveView(i32),
    /// Move the row selection by a step (-1 or 1), wrapping.
    MoveRow(i32),
    /// Move the row selection by a page, clamped.
    PageRow(i32),
    /// Jump the row selection to the first or last row.
    JumpRow { to_end: bool },
    /// Move the kind tab by a step (-1 or 1), wrapping.
    MoveKind(i32),
    /// Activate the selected row (Enter on the list opens its role strip).
    ActivateRow,
    /// Focus the list pane (Enter or → on the sidebar).
    FocusList,
    /// Focus the sidebar pane (←).
    FocusView,
    /// Clear the search text.
    ClearQuery,
    /// Remove the last search character.
    Backspace,
    /// Append printable text to the search.
    Search(char),
    /// Refresh the focused provider's model list.
    RefreshProvider,
    /// Close the hub.
    Close,
    /// Nothing to do.
    Ignore,
}

/// The inputs the ladder reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubKeyContext<'a> {
    pub focus: HubFocus,
    pub query: &'a str,
    pub provider_view: bool,
    /// Locked provider views take no list input; only Enter (sign in) and Escape.
    pub locked_view: bool,
}

/// Map one key to its effect. Tab toggles panes; ↑/↓ follow the focused pane;
/// Escape ladders: clear the query, then close.
pub fn hub_key_effect(context: &HubKeyContext<'_>, key: &HubKey) -> HubKeyEffect {
    if context.locked_view {
        return match key {
            HubKey::Enter => HubKeyEffect::ActivateRow,
            HubKey::Up if context.focus == HubFocus::Scope => HubKeyEffect::MoveView(-1),
            HubKey::Down if context.focus == HubFocus::Scope => HubKeyEffect::MoveView(1),
            HubKey::Left => HubKeyEffect::FocusView,
            HubKey::Escape => HubKeyEffect::Close,
            _ => HubKeyEffect::Ignore,
        };
    }
    match key {
        HubKey::Escape if !context.query.is_empty() => HubKeyEffect::ClearQuery,
        HubKey::Escape => HubKeyEffect::Close,
        HubKey::Tab | HubKey::ShiftTab => HubKeyEffect::Ignore,
        HubKey::F5 if context.provider_view => HubKeyEffect::RefreshProvider,
        HubKey::AltLeft => HubKeyEffect::MoveKind(-1),
        HubKey::AltRight => HubKeyEffect::MoveKind(1),
        HubKey::Left => HubKeyEffect::FocusView,
        HubKey::Right => HubKeyEffect::FocusList,
        HubKey::Up | HubKey::Down => {
            let step = if *key == HubKey::Up { -1 } else { 1 };
            match context.focus {
                HubFocus::Scope => HubKeyEffect::MoveView(step),
                HubFocus::List => HubKeyEffect::MoveRow(step),
            }
        }
        HubKey::PageUp => HubKeyEffect::PageRow(-1),
        HubKey::PageDown => HubKeyEffect::PageRow(1),
        HubKey::Home => HubKeyEffect::JumpRow { to_end: false },
        HubKey::End => HubKeyEffect::JumpRow { to_end: true },
        HubKey::Enter if context.focus == HubFocus::Scope => HubKeyEffect::FocusList,
        HubKey::Enter => HubKeyEffect::ActivateRow,
        HubKey::Backspace if !context.query.is_empty() => HubKeyEffect::Backspace,
        HubKey::Backspace => HubKeyEffect::Ignore,
        HubKey::Char(character) if !character.is_control() => HubKeyEffect::Search(*character),
        HubKey::Char(_) => HubKeyEffect::Ignore,
        HubKey::F5 => HubKeyEffect::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(focus: HubFocus, query: &str) -> HubKeyContext<'_> {
        HubKeyContext {
            focus,
            query,
            provider_view: true,
            locked_view: false,
        }
    }

    #[test]
    fn escape_clears_query_before_closing() {
        assert_eq!(
            hub_key_effect(&context(HubFocus::List, "son"), &HubKey::Escape),
            HubKeyEffect::ClearQuery
        );
        assert_eq!(
            hub_key_effect(&context(HubFocus::List, ""), &HubKey::Escape),
            HubKeyEffect::Close
        );
    }

    #[test]
    fn arrows_follow_the_focused_pane() {
        assert_eq!(
            hub_key_effect(&context(HubFocus::Scope, ""), &HubKey::Down),
            HubKeyEffect::MoveView(1)
        );
        assert_eq!(
            hub_key_effect(&context(HubFocus::List, ""), &HubKey::Up),
            HubKeyEffect::MoveRow(-1)
        );
        assert_eq!(
            hub_key_effect(&context(HubFocus::List, ""), &HubKey::Right),
            HubKeyEffect::FocusList
        );
    }

    #[test]
    fn enter_on_sidebar_only_moves_focus() {
        assert_eq!(
            hub_key_effect(&context(HubFocus::Scope, ""), &HubKey::Enter),
            HubKeyEffect::FocusList
        );
    }

    #[test]
    fn locked_view_takes_only_enter_and_escape() {
        let locked = HubKeyContext {
            focus: HubFocus::List,
            query: "",
            provider_view: true,
            locked_view: true,
        };
        assert_eq!(
            hub_key_effect(&locked, &HubKey::Enter),
            HubKeyEffect::ActivateRow
        );
        assert_eq!(hub_key_effect(&locked, &HubKey::Down), HubKeyEffect::Ignore);
    }
}
