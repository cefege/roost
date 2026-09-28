//! The select's keyboard and type-ahead rules, as data: which option a key
//! highlights or chooses, and which option a typed prefix names. Called by
//! `select.rs` (the trigger) and `select_listbox.rs` (the open listbox);
//! depends on nothing.
//!
//! v2's `apps/web/src/components/Settings/md/Select.tsx` got these from Kobalte's
//! select (`@kobalte/core` 0.13): the trigger opens on Enter, Space and the
//! arrows and steps the value with Left/Right; the listbox moves without
//! wrapping, chooses on Enter/Space, closes on Escape and Tab; and type-ahead
//! accumulates a prefix that resets after a second of quiet.

/// Where the highlight lands when the listbox opens with nothing selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenFocus {
    /// The first option (Enter, Space, ArrowDown, a click).
    First,
    /// The last option (ArrowUp).
    Last,
}

/// What a key on the closed trigger does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKeyAction {
    /// Open the listbox.
    Open(OpenFocus),
    /// Choose this option without opening.
    Choose(usize),
    /// Not a trigger key.
    Ignore,
}

/// What a key in the open listbox does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListboxKeyAction {
    /// Move the highlight here.
    Highlight(usize),
    /// Choose this option and close.
    Choose(usize),
    /// Close without choosing.
    Close,
    /// Not a listbox key.
    Ignore,
}

/// How long type-ahead waits before a new keystroke starts a new prefix.
pub const TYPEAHEAD_RESET_MS: f64 = 1000.0;

/// The highlight on open: the selected option when there is one, otherwise the
/// end the opening key names.
pub fn initial_highlight(len: usize, selected: Option<usize>, focus: OpenFocus) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if let Some(selected) = selected.filter(|selected| *selected < len) {
        return Some(selected);
    }
    Some(match focus {
        OpenFocus::First => 0,
        OpenFocus::Last => len - 1,
    })
}

/// A key on the closed trigger.
pub fn trigger_key_action(key: &str, len: usize, selected: Option<usize>) -> TriggerKeyAction {
    match key {
        "Enter" | " " | "ArrowDown" => TriggerKeyAction::Open(OpenFocus::First),
        "ArrowUp" => TriggerKeyAction::Open(OpenFocus::Last),
        "ArrowLeft" => match selected {
            Some(selected) => selected
                .checked_sub(1)
                .map_or(TriggerKeyAction::Ignore, TriggerKeyAction::Choose),
            None if len > 0 => TriggerKeyAction::Choose(0),
            None => TriggerKeyAction::Ignore,
        },
        "ArrowRight" => match selected {
            Some(selected) if selected + 1 < len => TriggerKeyAction::Choose(selected + 1),
            Some(_) => TriggerKeyAction::Ignore,
            None if len > 0 => TriggerKeyAction::Choose(0),
            None => TriggerKeyAction::Ignore,
        },
        _ => TriggerKeyAction::Ignore,
    }
}

/// A key in the open listbox. Movement stops at the ends rather than wrapping,
/// as Kobalte's select defaulted.
pub fn listbox_key_action(key: &str, len: usize, highlighted: Option<usize>) -> ListboxKeyAction {
    if len == 0 {
        return match key {
            "Escape" | "Tab" => ListboxKeyAction::Close,
            _ => ListboxKeyAction::Ignore,
        };
    }
    let last = len - 1;
    match key {
        "ArrowDown" => {
            ListboxKeyAction::Highlight(highlighted.map_or(0, |index| (index + 1).min(last)))
        }
        "ArrowUp" => {
            ListboxKeyAction::Highlight(highlighted.map_or(last, |index| index.saturating_sub(1)))
        }
        "Home" | "PageUp" => ListboxKeyAction::Highlight(0),
        "End" | "PageDown" => ListboxKeyAction::Highlight(last),
        "Enter" | " " => highlighted.map_or(ListboxKeyAction::Ignore, ListboxKeyAction::Choose),
        "Escape" | "Tab" => ListboxKeyAction::Close,
        _ => ListboxKeyAction::Ignore,
    }
}

/// The printable character a key types, or `None` for a named key or a chord.
/// Type-ahead ignores Ctrl and Meta chords, which are shortcuts, not text.
pub fn typeahead_character(key: &str, ctrl_or_meta: bool) -> Option<&str> {
    (!ctrl_or_meta && key.chars().count() == 1).then_some(key)
}

/// The type-ahead prefix being typed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Typeahead {
    query: String,
    last_key_ms: Option<f64>,
}

impl Typeahead {
    /// Whether a prefix is being typed, which makes Space part of it rather than
    /// a choice.
    pub fn is_active(&self, now_ms: f64) -> bool {
        !self.expired(now_ms) && !self.query.trim().is_empty()
    }

    /// Add a character, starting over when the previous one is too old.
    pub fn push(&mut self, character: &str, now_ms: f64) -> &str {
        if self.expired(now_ms) {
            self.query.clear();
        }
        self.query.push_str(character);
        self.last_key_ms = Some(now_ms);
        &self.query
    }

    fn expired(&self, now_ms: f64) -> bool {
        self.last_key_ms
            .is_none_or(|last| now_ms - last > TYPEAHEAD_RESET_MS)
    }
}

/// The option a prefix names: the first at or after `from` whose label starts
/// with it, case-insensitively, else the first from the top.
pub fn typeahead_match(labels: &[&str], query: &str, from: Option<usize>) -> Option<usize> {
    let query = query.to_lowercase();
    let matches = |label: &&str| label.to_lowercase().starts_with(&query);
    let start = from.unwrap_or(0);
    labels
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, label)| matches(label))
        .or_else(|| labels.iter().enumerate().find(|(_, label)| matches(label)))
        .map(|(index, _)| index)
}
