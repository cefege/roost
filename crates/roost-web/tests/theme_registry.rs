//! The theme registry's invariants: every registered theme is complete, valid
//! and uniquely named, and the ids the engine falls back to exist. Ported from
//! `apps/web/tests/themes.test.ts`; exercises `roost_web::theme::{themes, tokens}`.
//!
//! "Defines every canonical token" and "no unknown tokens" are compile-time
//! facts here (a palette is an exhaustive match over `CanonicalToken`), so the
//! runtime checks left are the ones a type cannot make: a malformed or empty
//! colour, a duplicate id, and a fallback id that names nothing.

use std::collections::BTreeSet;

use roost_web::theme::themes::{
    DEFAULT_THEME_ID, SYSTEM_DARK_ID, SYSTEM_LIGHT_ID, THEMES, default_theme, theme_by_id,
};
use roost_web::theme::tokens::{CanonicalToken, ThemeAppearance};

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, or an `rgb(…)`/`rgba(…)` function — the
/// forms v2's registry test admitted.
fn is_css_colour(value: &str) -> bool {
    if let Some(digits) = value.strip_prefix('#') {
        return matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit());
    }
    let function = value
        .strip_prefix("rgba")
        .or_else(|| value.strip_prefix("rgb"));
    function.is_some_and(|rest| rest.starts_with('(') && rest.ends_with(')') && rest.len() > 2)
}

#[test]
fn at_least_dark_and_light_are_registered() {
    assert!(THEMES.len() >= 2);
    let appearances: BTreeSet<&str> = THEMES
        .iter()
        .map(|theme| theme.appearance.color_scheme())
        .collect();
    assert_eq!(appearances, BTreeSet::from(["dark", "light"]));
}

#[test]
fn theme_ids_are_unique() {
    let ids: BTreeSet<&str> = THEMES.iter().map(|theme| theme.id).collect();
    assert_eq!(ids.len(), THEMES.len());
}

#[test]
fn every_fallback_id_resolves_to_a_registered_theme() {
    for id in [DEFAULT_THEME_ID, SYSTEM_DARK_ID, SYSTEM_LIGHT_ID] {
        assert!(theme_by_id(id).is_some(), "{id} is not registered");
    }
    assert_eq!(default_theme().id, DEFAULT_THEME_ID);
}

#[test]
fn the_system_ids_have_the_appearance_they_stand_for() {
    // `auto` on a dark OS must land on a dark theme; swapping the two ids would
    // paint a light UI for a reader who asked the OS for dark.
    assert_eq!(theme_by_id(SYSTEM_DARK_ID).map(|t| t.appearance), Some(ThemeAppearance::Dark));
    assert_eq!(theme_by_id(SYSTEM_LIGHT_ID).map(|t| t.appearance), Some(ThemeAppearance::Light));
}

#[test]
fn every_value_is_a_valid_colour() {
    for theme in &THEMES {
        for token in CanonicalToken::ALL {
            let value = theme.token(token);
            assert!(!value.is_empty(), "{} {} is empty", theme.id, token.name());
            assert!(is_css_colour(value), "{} {} = {value:?}", theme.id, token.name());
        }
    }
}

#[test]
fn every_theme_has_a_label() {
    for theme in &THEMES {
        assert!(!theme.label.is_empty(), "{} has no label", theme.id);
    }
}

#[test]
fn canonical_token_names_are_unique_custom_properties() {
    // Two roles sharing a name would make the engine write one property twice
    // and leave the other at its `:root` fallback.
    let names: BTreeSet<&str> = CanonicalToken::ALL.iter().map(|token| token.name()).collect();
    assert_eq!(names.len(), CanonicalToken::ALL.len());
    assert_eq!(CanonicalToken::StatusOk.custom_property(), "--status-ok");
}
