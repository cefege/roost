//! The `/design` gallery draws only tokens the stylesheet declares, and the
//! theme engine writes only roles the stylesheet has a fallback for. Exercises
//! `roost_web::components::design::catalog` (the port of the catalogs in
//! `apps/web/src/components/design/DesignGallery.tsx`) and
//! `roost_web::theme::tokens` against `assets/styles/theme-vars.css`.
//!
//! A swatch or ramp row naming an undeclared property renders transparent or at
//! the inherited size, which reads as "that token is fine" when it does not
//! exist — the gallery would be lying about the palette it documents.

use std::collections::BTreeSet;

use roost_web::components::design::catalog::{
    COLOR_GROUPS, ELEV_STEPS, RAMP_STEPS, SHAPE_STEPS, SPACE_STEPS,
};
use roost_web::theme::tokens::CanonicalToken;

const THEME_VARS: &str = include_str!("../assets/styles/theme-vars.css");

/// Every custom property the stylesheet DECLARES: a `--name` that starts a
/// declaration (after whitespace, `{` or `;`) and is followed by `:`.
fn declared_properties(css: &str) -> BTreeSet<String> {
    let bytes = css.as_bytes();
    let mut declared = BTreeSet::new();
    let mut index = 0;
    while let Some(offset) = css[index..].find("--") {
        let start = index + offset;
        let starts_declaration = start == 0
            || matches!(bytes[start - 1], b' ' | b'\t' | b'\n' | b'\r' | b'{' | b';');
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

#[test]
fn every_swatched_colour_token_is_declared() {
    let declared = declared_properties(THEME_VARS);
    for group in COLOR_GROUPS {
        for token in group.tokens {
            assert!(declared.contains(*token), "{} swatches undeclared {token}", group.title);
        }
    }
}

#[test]
fn every_type_ramp_step_declares_size_line_and_weight() {
    let declared = declared_properties(THEME_VARS);
    for step in RAMP_STEPS {
        for part in ["size", "line", "weight"] {
            let property = format!("--md-{step}-{part}");
            assert!(declared.contains(&property), "{property} is not declared");
        }
    }
}

#[test]
fn every_space_shape_and_elevation_step_is_declared() {
    let declared = declared_properties(THEME_VARS);
    let properties = SPACE_STEPS
        .iter()
        .map(|step| format!("--md-space-{step}"))
        .chain(SHAPE_STEPS.iter().map(|step| format!("--md-shape-{step}")))
        .chain(ELEV_STEPS.iter().map(|step| format!("--md-elev-{step}")));
    for property in properties {
        assert!(declared.contains(&property), "{property} is not declared");
    }
}

#[test]
fn every_canonical_theme_role_has_a_stylesheet_fallback() {
    // The engine writes these inline; before it runs (and wherever it cannot)
    // the `:root` declaration is what paints, and every alias reads through it.
    let declared = declared_properties(THEME_VARS);
    for token in CanonicalToken::ALL {
        let property = token.custom_property();
        assert!(declared.contains(&property), "{property} has no fallback in theme-vars.css");
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
