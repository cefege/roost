//! The `Button` and `IconButton` rules that have no markup of their own: the
//! default `type` yielding to a caller's, the icon button's ARIA, and the class
//! merge where a caller's utility replaces the size's. Exercises
//! `roost_web::components::ui::button`.

use dioxus::prelude::Attribute;
use roost_web::components::ui::button::{
    button_class, icon_button_aria, names_button_type, ButtonSize, ButtonVariant,
};

#[test]
fn a_caller_type_override_suppresses_the_button_default() {
    // A second `type` attribute beside a caller's `type="submit"` would leave
    // the browser to pick one; the default must step aside.
    let submit = vec![Attribute::new("type", "submit", None, false)];
    let labelled = vec![Attribute::new("aria-label", "Close", None, false)];
    assert!(names_button_type(&submit));
    assert!(!names_button_type(&labelled));
    assert!(!names_button_type(&[]));
}

#[test]
fn an_icon_button_names_itself_and_claims_a_popup_only_when_it_has_one() {
    assert_eq!(
        icon_button_aria("More", None, None, None),
        vec![("aria-label", "More".to_string())]
    );
    assert_eq!(
        icon_button_aria(
            "Open context menu",
            Some("menu"),
            Some("design-context-menu"),
            Some(false)
        ),
        vec![
            ("aria-label", "Open context menu".to_string()),
            ("aria-haspopup", "menu".to_string()),
            ("aria-controls", "design-context-menu".to_string()),
            ("aria-expanded", "false".to_string()),
        ]
    );
}

#[test]
fn a_caller_utility_replaces_the_size_utility_it_conflicts_with() {
    let classes = button_class(ButtonVariant::Default, ButtonSize::Default, Some("px-8"));
    let tokens: Vec<&str> = classes.split_whitespace().collect();
    assert!(tokens.contains(&"px-8"), "{classes}");
    assert!(!tokens.contains(&"px-4"), "{classes}");
    assert!(tokens.contains(&"py-2"), "{classes}");
}
