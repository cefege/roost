//! Folder-list keyboard decisions: movement, drill, parent and terminal launch.
//! Called by the picker's scoped DOM listener; the picker owns cursor state.
//! Pure decisions remain testable without a browser.

// Native builds do not mount the listener that constructs these variants.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
/// What a key press in the picker asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerKey {
    /// Not the picker's.
    Ignore,
    /// Move the cursor by this many entries.
    Move(i64),
    /// Select the first folder matching a typed prefix.
    Typeahead(String),
    /// Edit an absolute or home-relative path.
    EditPath(String),
    /// Descend into the entry under the cursor.
    Drill,
    /// Go to the parent directory.
    Parent,
    /// Open a terminal in the directory being browsed.
    OpenHere,
    /// Leave the picker.
    Leave,
}

/// The facts the decision reads besides the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PickerKeyContext {
    /// A handler already consumed the event.
    pub default_prevented: bool,
    /// The new-folder dialog is showing, and owns every key while it is.
    pub dialog_open: bool,
    /// The press landed inside the entry region.
    pub inside_results: bool,
    /// The machine is in scope, so there is something to move through.
    pub scoped: bool,
    /// The surface is a phone, where Escape leaves rather than doing nothing.
    pub compact: bool,
    /// Whether the cursor has an entry under it.
    pub has_active: bool,
}

/// Decide what `key` does in the picker.
///
/// The cursor rests at "nothing selected", so the FIRST arrow press lands on
/// entry 0 rather than jumping a row: a cursor that starts somewhere the reader
/// did not put it is a cursor that opens the wrong folder.
#[must_use]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn picker_key(key: &str, alt: bool, context: &PickerKeyContext) -> PickerKey {
    use PickerKey as K;
    if context.default_prevented || context.dialog_open {
        return K::Ignore;
    }
    if key == "Escape" {
        return if context.compact { K::Leave } else { K::Ignore };
    }
    if !context.scoped || !context.inside_results {
        return K::Ignore;
    }
    match key {
        "ArrowRight" => K::Move(1),
        "ArrowLeft" if alt => K::Parent,
        "ArrowLeft" => K::Move(-1),
        "ArrowDown" => K::Move(1),
        "ArrowUp" if alt => K::Parent,
        "ArrowUp" => K::Move(-1),
        "Home" => K::Move(-i64::MAX),
        "End" => K::Move(i64::MAX),
        "Backspace" => K::Parent,
        "Enter" if context.has_active => K::Drill,
        "Enter" => K::OpenHere,
        _ => K::Ignore,
    }
}

/// The cursor after a move, from a resting `-1`.
///
/// `-1` is "no keyboard cursor yet", so the FIRST arrow press lands on entry 0
/// whatever direction it went: a cursor that starts somewhere the reader did
/// not put it is a cursor that opens the wrong folder. Only a cursor already
/// resting on an entry moves by `delta`.
#[must_use]
pub fn moved_cursor(current: i64, delta: i64, count: usize) -> i64 {
    if count == 0 {
        return -1;
    }
    let last = count as i64 - 1;
    if current < 0 {
        return if delta == i64::MAX { last } else { 0 };
    }
    current.saturating_add(delta).clamp(0, last)
}

#[cfg(test)]
mod tests {
    use super::{PickerKey as K, PickerKeyContext, moved_cursor, picker_key};

    /// The context of a picker mid-browse on a desktop, which every case below
    /// varies one fact of rather than restating.
    fn browsing() -> PickerKeyContext {
        PickerKeyContext {
            default_prevented: false,
            dialog_open: false,
            inside_results: true,
            scoped: true,
            compact: false,
            has_active: true,
        }
    }

    #[test]
    fn arrows_move_by_one_folder() {
        assert_eq!(picker_key("ArrowRight", false, &browsing()), K::Move(1));
        assert_eq!(picker_key("ArrowLeft", false, &browsing()), K::Move(-1));
        assert_eq!(picker_key("ArrowDown", false, &browsing()), K::Move(1));
        assert_eq!(picker_key("ArrowUp", false, &browsing()), K::Move(-1));
    }

    #[test]
    fn alt_left_and_backspace_both_go_to_the_parent() {
        assert_eq!(picker_key("ArrowLeft", true, &browsing()), K::Parent);
        assert_eq!(picker_key("Backspace", false, &browsing()), K::Parent);
    }

    #[test]
    fn enter_drills_only_when_the_cursor_has_an_entry() {
        assert_eq!(picker_key("Enter", false, &browsing()), K::Drill);
        let resting = PickerKeyContext {
            has_active: false,
            ..browsing()
        };
        assert_eq!(picker_key("Enter", false, &resting), K::OpenHere);
    }

    #[test]
    fn list_boundaries_and_alt_up_are_available() {
        assert_eq!(picker_key("Home", false, &browsing()), K::Move(-i64::MAX));
        assert_eq!(picker_key("End", false, &browsing()), K::Move(i64::MAX));
        assert_eq!(picker_key("ArrowUp", true, &browsing()), K::Parent);
        assert_eq!(moved_cursor(-1, i64::MAX, 4), 3);
        assert_eq!(moved_cursor(2, i64::MAX, 4), 3);
    }

    #[test]
    fn escape_leaves_only_on_a_phone() {
        assert_eq!(picker_key("Escape", false, &browsing()), K::Ignore);
        let phone = PickerKeyContext {
            compact: true,
            ..browsing()
        };
        assert_eq!(picker_key("Escape", false, &phone), K::Leave);
    }

    #[test]
    fn a_consumed_or_owned_event_is_never_the_pickers() {
        let consumed = PickerKeyContext {
            default_prevented: true,
            ..browsing()
        };
        assert_eq!(picker_key("Enter", false, &consumed), K::Ignore);
        // The dialog owns the keyboard while it is open, including Escape on a
        // phone — a second handler closing both is one Escape, two surfaces
        // gone.
        let owned = PickerKeyContext {
            dialog_open: true,
            compact: true,
            ..browsing()
        };
        assert_eq!(picker_key("Escape", false, &owned), K::Ignore);
    }

    #[test]
    fn keys_press_landing_outside_the_region_or_an_absent_machine_are_ignored() {
        let outside = PickerKeyContext {
            inside_results: false,
            ..browsing()
        };
        assert_eq!(picker_key("ArrowDown", false, &outside), K::Ignore);
        let unscoped = PickerKeyContext {
            scoped: false,
            ..browsing()
        };
        assert_eq!(picker_key("Enter", false, &unscoped), K::Ignore);
    }

    #[test]
    fn an_unbound_key_is_ignored() {
        assert_eq!(picker_key("KeyQ", false, &browsing()), K::Ignore);
    }

    #[test]
    fn the_first_arrow_lands_on_entry_zero_and_moves_clamp_at_both_ends() {
        assert_eq!(moved_cursor(-1, 1, 4), 0);
        assert_eq!(moved_cursor(-1, -1, 4), 0);
        assert_eq!(moved_cursor(0, -1, 4), 0);
        assert_eq!(moved_cursor(3, 1, 4), 3);
        assert_eq!(moved_cursor(1, 3, 4), 3);
    }

    #[test]
    fn an_empty_region_leaves_the_cursor_resting() {
        assert_eq!(moved_cursor(0, 1, 0), -1);
        assert_eq!(moved_cursor(-1, 1, 0), -1);
    }
}
