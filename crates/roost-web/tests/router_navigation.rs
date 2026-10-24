//! A destination row hands only plain primary clicks to the in-app router; any
//! modifier or other button is the reader asking the browser for something else.

use dioxus::prelude::Modifiers;
use roost_web::router_state::is_in_app_navigation_click;

#[test]
fn a_destination_row_hands_only_plain_primary_clicks_to_the_router() {
    assert!(is_in_app_navigation_click(true, Modifiers::empty()));
    assert!(!is_in_app_navigation_click(true, Modifiers::CONTROL));
    assert!(!is_in_app_navigation_click(true, Modifiers::META));
    assert!(!is_in_app_navigation_click(true, Modifiers::SHIFT));
    assert!(!is_in_app_navigation_click(false, Modifiers::empty()));
}
