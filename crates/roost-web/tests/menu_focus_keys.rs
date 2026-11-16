//! A key pressed before a menu's opening focus lands: Home and End still pick
//! their edge with no item focused, and a cancelled focus request is cancelled
//! for every retry holding a clone of it, so a stale attempt cannot pull focus
//! back to the edge the opening key asked for. The microtask-then-frame
//! attempt order needs a document and lives in `ui/menu_focus/dom.rs`.
//!
//! Exercises `roost_web::components::ui::menu_focus`.

use roost_web::components::ui::menu_focus::{
    MenuFocusEdge, MenuFocusRequest, MenuKeyAction, menu_key_action,
};

#[test]
fn home_and_end_pick_their_edge_before_any_item_has_focus() {
    assert_eq!(menu_key_action("Home", None, 4), MenuKeyAction::Focus(0));
    assert_eq!(
        menu_key_action("End", None, 4),
        MenuKeyAction::Focus(3),
        "End pressed right after the opening ArrowDown must reach the last item, not wait for \
         the first item's focus"
    );
}

#[test]
fn home_and_end_jump_from_any_focused_item() {
    assert_eq!(menu_key_action("End", Some(0), 4), MenuKeyAction::Focus(3));
    assert_eq!(menu_key_action("Home", Some(3), 4), MenuKeyAction::Focus(0));
}

#[test]
fn an_empty_menu_ignores_the_edge_keys() {
    assert_eq!(menu_key_action("Home", None, 0), MenuKeyAction::Ignore);
    assert_eq!(menu_key_action("End", None, 0), MenuKeyAction::Ignore);
}

#[test]
fn cancelling_a_request_stops_every_retry_holding_a_clone() {
    let request = MenuFocusRequest::new("machine-menu", MenuFocusEdge::First);
    let retry = request.clone();
    assert!(retry.is_pending());

    request.cancel();

    assert!(
        !retry.is_pending(),
        "a retry still pending after the menu cancelled its request would move focus off the \
         item the reader's next key chose"
    );
}
