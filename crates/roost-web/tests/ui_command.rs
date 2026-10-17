//! Behaviour tests for the pure rules behind `ui::command`: keyboard movement,
//! the item id scheme, and the filter.

use roost_web::components::ui::command_item::item_matches;
use roost_web::components::ui::command_keys::{
    CommandKeyAction, command_key_action, item_element_id,
};

fn values(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

#[test]
fn arrow_down_without_highlight_starts_at_first_item() {
    let visible = values(&["open", "close", "quit"]);
    assert_eq!(
        command_key_action("ArrowDown", &visible, None),
        CommandKeyAction::Highlight("open".into())
    );
}

#[test]
fn arrow_down_moves_to_next_and_stops_at_last() {
    let visible = values(&["open", "close", "quit"]);
    assert_eq!(
        command_key_action("ArrowDown", &visible, Some("open")),
        CommandKeyAction::Highlight("close".into())
    );
    assert_eq!(
        command_key_action("ArrowDown", &visible, Some("quit")),
        CommandKeyAction::Ignore
    );
}

#[test]
fn arrow_up_without_highlight_starts_at_last_item() {
    let visible = values(&["open", "close", "quit"]);
    assert_eq!(
        command_key_action("ArrowUp", &visible, None),
        CommandKeyAction::Highlight("quit".into())
    );
}

#[test]
fn arrow_up_at_first_item_scrolls_to_top() {
    let visible = values(&["open", "close"]);
    assert_eq!(
        command_key_action("ArrowUp", &visible, Some("open")),
        CommandKeyAction::ScrollToTop
    );
    assert_eq!(
        command_key_action("ArrowUp", &visible, Some("close")),
        CommandKeyAction::Highlight("open".into())
    );
}

#[test]
fn enter_chooses_highlighted_item_only() {
    let visible = values(&["open", "close"]);
    assert_eq!(
        command_key_action("Enter", &visible, Some("close")),
        CommandKeyAction::Choose("close".into())
    );
    assert_eq!(
        command_key_action("Enter", &visible, None),
        CommandKeyAction::Ignore
    );
}

#[test]
fn highlight_that_is_no_longer_visible_counts_as_none() {
    let visible = values(&["close"]);
    assert_eq!(
        command_key_action("ArrowDown", &visible, Some("open")),
        CommandKeyAction::Highlight("close".into())
    );
}

#[test]
fn no_visible_items_ignores_every_key() {
    assert_eq!(
        command_key_action("ArrowDown", &[], None),
        CommandKeyAction::Ignore
    );
    assert_eq!(
        command_key_action("Enter", &[], None),
        CommandKeyAction::Ignore
    );
}

#[test]
fn other_keys_are_ignored() {
    let visible = values(&["open"]);
    assert_eq!(
        command_key_action("a", &visible, Some("open")),
        CommandKeyAction::Ignore
    );
}

#[test]
fn item_element_id_sanitises_value_characters() {
    assert_eq!(
        item_element_id("command-3", "Open file"),
        "command-3-item-Open_file"
    );
    assert_eq!(
        item_element_id("command-3", "a-b_c9"),
        "command-3-item-a-b_c9"
    );
}

#[test]
fn item_matches_is_case_insensitive_substring() {
    assert!(item_matches("Open File", "file", true));
    assert!(item_matches("Open File", "OPEN", true));
    assert!(!item_matches("Open File", "quit", true));
}

#[test]
fn item_matches_empty_query_and_disabled_filter_match_everything() {
    assert!(item_matches("Open File", "", true));
    assert!(item_matches("Open File", "quit", false));
}
