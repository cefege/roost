//! The dialog's modal rules as data: when its header close button shows, which
//! key dismisses it, which way a focus-trap sentinel wraps, what counts as
//! tabbable, and that a caller can take over auto-focus. Exercises
//! `roost_web::components::md::{dialog, focus_scope}`, the port of
//! `apps/web/src/components/Settings/md/Dialog.tsx` and the Kobalte focus scope
//! it wrapped.

use roost_web::components::md::dialog::{dialog_shows_close_button, is_dismiss_key};
use roost_web::components::md::focus_scope::{
    AutoFocusRequest, FocusEdge, is_in_tab_order, sentinel_focus_edge,
};

#[test]
fn the_close_button_shows_only_without_an_action_band_unless_told() {
    assert!(dialog_shows_close_button(None, false));
    assert!(!dialog_shows_close_button(None, true));
    assert!(dialog_shows_close_button(Some(true), true));
    assert!(!dialog_shows_close_button(Some(false), false));
}

#[test]
fn escape_dismisses_and_nothing_else_does() {
    assert!(is_dismiss_key("Escape"));
    assert!(!is_dismiss_key("Enter"));
    assert!(!is_dismiss_key("Esc "));
}

#[test]
fn a_sentinel_wraps_backwards_only_when_focus_left_the_first_element() {
    // Shift+Tab off the first element lands on the start sentinel and must go
    // to the LAST element; every other arrival wraps to the first.
    assert_eq!(sentinel_focus_edge(true), FocusEdge::Last);
    assert_eq!(sentinel_focus_edge(false), FocusEdge::First);
}

#[test]
fn negative_tabindex_leaves_the_tab_order() {
    assert!(is_in_tab_order(None));
    assert!(is_in_tab_order(Some("0")));
    assert!(is_in_tab_order(Some("3")));
    assert!(!is_in_tab_order(Some("-1")));
    assert!(is_in_tab_order(Some("not-a-number")));
}

#[test]
fn a_prevented_auto_focus_is_seen_by_every_clone() {
    // The dialog hands the caller a clone and reads the original afterwards,
    // so the prevention must be shared, not copied.
    let request = AutoFocusRequest::new();
    let handed_to_caller = request.clone();
    assert!(!request.default_prevented());
    handed_to_caller.prevent_default();
    assert!(request.default_prevented());
}
