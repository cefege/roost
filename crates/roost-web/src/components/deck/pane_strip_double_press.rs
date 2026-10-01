//! The pane strip's filler double-press, as a decision rather than an event.
//!
//! The strip's empty bar opens a new terminal on a second press, and that used
//! to be an `ondoubleclick` attribute. Dioxus matches a handler by comparing
//! the attribute's own tail against `event.type_()`, and `ondoubleclick` strips
//! to `doubleclick` while the browser sends `dblclick` — so the handler
//! registered and never fired, with no error and no stack trace. This is the
//! same class as `docs/FAILURE-INDEX.md`'s entry on that matcher; reading the
//! press stream fixes it without depending on a name mapping at all.
//!
//! Owned by `pane_strip`, which calls it from the filler's press. No DOM, no
//! dioxus: the decision is a function of two instants so it can be tested on a
//! target that has no browser.

/// How far apart two presses may be and still be one double press, ms.
pub const DOUBLE_PRESS_WINDOW_MS: f64 = 500.0;

/// Whether two presses at `now_ms` are one double press.
///
/// A press that pairs is SPENT: the memory is cleared so a third press starts a
/// new pair rather than firing again off the same second press, which is the
/// shape a user gets when they click a little too eagerly.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DoublePress {
    last: Option<f64>,
}

impl DoublePress {
    /// Record a press and answer whether it completed a pair.
    pub fn press(&mut self, now_ms: f64) -> bool {
        let paired = self
            .last
            .is_some_and(|last| (now_ms - last).abs() <= DOUBLE_PRESS_WINDOW_MS);
        self.last = if paired { None } else { Some(now_ms) };
        paired
    }
}

#[cfg(test)]
mod tests {
    use super::{DOUBLE_PRESS_WINDOW_MS, DoublePress};

    #[test]
    fn two_presses_inside_the_window_are_one_double_press() {
        let mut press = DoublePress::default();
        assert!(!press.press(0.0));
        assert!(press.press(DOUBLE_PRESS_WINDOW_MS / 2.0));
    }

    #[test]
    fn a_press_outside_the_window_is_its_own() {
        let mut press = DoublePress::default();
        assert!(!press.press(0.0));
        assert!(
            !press.press(DOUBLE_PRESS_WINDOW_MS * 2.0),
            "a slow second press opens a new terminal once, not twice"
        );
    }

    #[test]
    fn a_spent_pair_does_not_fire_a_third_press() {
        let mut press = DoublePress::default();
        press.press(0.0);
        assert!(press.press(100.0));
        assert!(
            !press.press(150.0),
            "the pairing is spent; a third press begins a new one"
        );
    }

    #[test]
    fn the_window_is_inclusive_at_its_edge() {
        let mut press = DoublePress::default();
        press.press(10.0);
        assert!(press.press(10.0 + DOUBLE_PRESS_WINDOW_MS));
    }
}
