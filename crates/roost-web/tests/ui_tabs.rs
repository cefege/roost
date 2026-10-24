//! Tab keyboard movement: arrow keys follow the orientation, Home and End jump to
//! the ends, and the index wraps past either edge.

use dioxus::prelude::{Key, NamedKey};
use roost_web::components::ui::tabs::{TabStep, next_tab_index, tab_step_for_key};

#[test]
fn arrow_keys_follow_the_tab_orientation() {
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::ArrowRight), false),
        Some(TabStep::Next)
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::ArrowLeft), false),
        Some(TabStep::Previous)
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::ArrowDown), true),
        Some(TabStep::Next)
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::ArrowUp), true),
        Some(TabStep::Previous)
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::ArrowDown), false),
        None
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::Home), true),
        Some(TabStep::First)
    );
    assert_eq!(
        tab_step_for_key(&Key::Named(NamedKey::End), false),
        Some(TabStep::Last)
    );
}

#[test]
fn the_index_wraps_past_either_edge() {
    assert_eq!(next_tab_index(2, 3, TabStep::Next), 0);
    assert_eq!(next_tab_index(0, 3, TabStep::Previous), 2);
    assert_eq!(next_tab_index(1, 3, TabStep::Next), 2);
    assert_eq!(next_tab_index(1, 3, TabStep::First), 0);
    assert_eq!(next_tab_index(1, 3, TabStep::Last), 2);
    assert_eq!(next_tab_index(0, 0, TabStep::Next), 0);
}
