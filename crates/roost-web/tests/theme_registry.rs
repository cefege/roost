//! The theme catalog's invariants: every base and accent palette is complete,
//! uniquely named and carries a value in both appearances, and the ids the
//! engine falls back to exist. Exercises `roost_web::theme::catalog`.
//!
//! "Every palette has the same keys" is checked at runtime because the palette
//! tables are plain data; a missing role would otherwise paint nothing.

use std::collections::BTreeSet;

use roost_web::theme::catalog::{
    DEFAULT_ACCENT_ID, DEFAULT_BASE_ID, ThemeSpec, accent_theme, accent_themes, base_theme,
    base_themes, default_accent, default_base,
};

/// A colour value the stylesheet can parse: `oklch(…)`, `rgb(…)`, `rgba(…)`,
/// `hsl(…)`, or a hex literal.
fn is_css_colour(value: &str) -> bool {
    if let Some(digits) = value.strip_prefix('#') {
        return matches!(digits.len(), 3 | 6 | 8) && digits.chars().all(|c| c.is_ascii_hexdigit());
    }
    ["oklch(", "rgb(", "rgba(", "hsl("]
        .iter()
        .any(|prefix| value.starts_with(prefix) && value.ends_with(')'))
}

fn keys(pairs: &[(&str, &str)]) -> BTreeSet<String> {
    pairs.iter().map(|(key, _)| (*key).to_string()).collect()
}

fn every_palette() -> Vec<&'static ThemeSpec> {
    base_themes().iter().chain(accent_themes()).collect()
}

#[test]
fn the_picker_lists_seven_bases_and_eighteen_accents() {
    assert_eq!(base_themes().len(), 7);
    assert_eq!(accent_themes().count(), 18);
}

#[test]
fn palette_ids_are_unique_within_each_list() {
    let bases: BTreeSet<&str> = base_themes().iter().map(|theme| theme.id).collect();
    assert_eq!(bases.len(), base_themes().len());
    let accents: BTreeSet<&str> = accent_themes().map(|theme| theme.id).collect();
    assert_eq!(accents.len(), accent_themes().count());
}

#[test]
fn every_palette_defines_values_in_both_appearances() {
    for theme in every_palette() {
        assert!(
            !theme.light.is_empty() || theme.id == DEFAULT_ACCENT_ID,
            "{} light",
            theme.id
        );
        assert!(
            !theme.dark.is_empty() || theme.id == DEFAULT_ACCENT_ID,
            "{} dark",
            theme.id
        );
        for (key, value) in theme.light.iter().chain(theme.dark.iter()) {
            assert!(
                key.starts_with("--"),
                "{} key {key} is not a custom property",
                theme.id
            );
            assert!(
                is_css_colour(value),
                "{} {key} = {value:?} is not a colour",
                theme.id
            );
        }
    }
}

#[test]
fn a_base_defines_the_same_keys_in_both_appearances() {
    for theme in base_themes() {
        assert_eq!(
            keys(theme.light),
            keys(theme.dark),
            "{} light and dark define different roles",
            theme.id
        );
    }
}

#[test]
fn every_base_defines_the_core_surface_and_text_roles() {
    let core = [
        "--background",
        "--foreground",
        "--card",
        "--primary",
        "--border",
        "--ring",
    ];
    for theme in base_themes() {
        for role in core {
            assert!(
                keys(theme.light).contains(role),
                "{} light lacks {role}",
                theme.id
            );
            assert!(
                keys(theme.dark).contains(role),
                "{} dark lacks {role}",
                theme.id
            );
        }
    }
}

#[test]
fn the_fallback_ids_resolve() {
    assert_eq!(default_base().id, DEFAULT_BASE_ID);
    assert_eq!(default_accent().id, DEFAULT_ACCENT_ID);
    assert!(base_theme(DEFAULT_BASE_ID).is_some());
    assert!(accent_theme(DEFAULT_ACCENT_ID).is_some());
}

#[test]
fn the_default_base_is_neutral_with_its_dark_background() {
    let neutral = base_theme("neutral").expect("neutral is registered");
    assert!(neutral.dark.contains(&("--background", "oklch(0.145 0 0)")));
}
