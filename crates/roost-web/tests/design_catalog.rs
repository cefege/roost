//! The `/design` gallery specimens name only Tailwind theme roles the stylesheet
//! defines, and the theme engine writes only roles the stylesheet has a fallback
//! for. Exercises `roost_web::components::design::catalog` against the
//! `@theme inline` block in `tailwind.css` and `theme::tokens` against
//! `assets/styles/theme-vars.css`.
//!
//! A swatch or radius step naming a role the theme does not define paints
//! nothing, which reads as "that token is fine" when it does not exist — the
//! gallery would be lying about the palette it documents.

use std::collections::BTreeSet;

use roost_web::components::design::catalog::{COLOR_GROUPS, SHAPE_STEPS};
use roost_web::theme::tokens::CanonicalToken;

const THEME_VARS: &str = include_str!("../assets/styles/theme-vars.css");
const TAILWIND_CSS: &str = include_str!("../tailwind.css");

/// Every custom property the stylesheet DECLARES: a `--name` that starts a
/// declaration (after whitespace, `{` or `;`) and is followed by `:`.
fn declared_properties(css: &str) -> BTreeSet<String> {
    let bytes = css.as_bytes();
    let mut declared = BTreeSet::new();
    let mut index = 0;
    while let Some(offset) = css[index..].find("--") {
        let start = index + offset;
        let starts_declaration =
            start == 0 || matches!(bytes[start - 1], b' ' | b'\t' | b'\n' | b'\r' | b'{' | b';');
        let name_end = css[start + 2..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .map_or(css.len(), |end| start + 2 + end);
        if starts_declaration && css[name_end..].trim_start().starts_with(':') {
            declared.insert(css[start..name_end].to_string());
        }
        index = name_end.max(start + 2);
    }
    declared
}

/// The body of the first `@theme inline` block. It holds declarations only, so
/// the first closing brace ends it.
fn theme_block(css: &str) -> &str {
    css.split_once("@theme inline").map_or("", |(_, rest)| {
        rest.split_once('}').map_or(rest, |(block, _)| block)
    })
}

/// The names after `prefix` of every theme property that starts with it.
fn theme_names(prefix: &str) -> BTreeSet<String> {
    declared_properties(theme_block(TAILWIND_CSS))
        .into_iter()
        .filter_map(|property| property.strip_prefix(prefix).map(str::to_string))
        .collect()
}

#[test]
fn every_colour_role_in_the_theme_is_swatched_once() {
    let theme_roles = theme_names("--color-");
    let swatched_roles: Vec<&str> = COLOR_GROUPS
        .iter()
        .flat_map(|group| group.roles.iter().map(|role| role.name))
        .collect();
    assert_eq!(
        swatched_roles.iter().copied().collect::<BTreeSet<_>>(),
        theme_roles
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        "the colour swatches must show exactly the roles the theme defines"
    );
    assert_eq!(
        swatched_roles.len(),
        theme_roles.len(),
        "a colour role is swatched more than once"
    );
}

#[test]
fn every_colour_swatch_names_its_background_utility() {
    for group in COLOR_GROUPS {
        for role in group.roles {
            assert_eq!(
                role.class,
                format!("bg-{}", role.name).as_str(),
                "{} swatch does not paint its own role",
                role.name
            );
        }
    }
}

#[test]
fn every_radius_step_is_defined_by_the_theme() {
    let theme_radii = theme_names("--radius-");
    let shown_radii: BTreeSet<String> = SHAPE_STEPS
        .iter()
        .filter_map(|step| step.label.strip_prefix("rounded-").map(str::to_string))
        .collect();
    assert_eq!(
        shown_radii, theme_radii,
        "the shape scale must show exactly the radii the theme defines"
    );
}

#[test]
fn every_canonical_theme_role_has_a_stylesheet_fallback() {
    // The engine writes these inline; before it runs (and wherever it cannot)
    // the `:root` declaration is what paints, and every alias reads through it.
    let declared = declared_properties(THEME_VARS);
    for token in CanonicalToken::ALL {
        let property = token.custom_property();
        assert!(
            declared.contains(&property),
            "{property} has no fallback in theme-vars.css"
        );
    }
}

#[test]
fn the_declaration_scanner_ignores_references() {
    let css = ":root { --declared: 1px; --other:2px }\n.x { color: var(--referenced); }";
    let declared = declared_properties(css);
    assert_eq!(
        declared,
        BTreeSet::from(["--declared".to_string(), "--other".to_string()])
    );
}
