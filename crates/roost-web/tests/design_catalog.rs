//! The `/design` gallery's specimens name only Tailwind theme roles the
//! stylesheet defines, and every Roost alias in `theme-vars.css` resolves to a
//! property the stylesheet declares or the engine writes. Exercises
//! `roost_web::components::design::catalog` against the `@theme inline` block in
//! `tailwind.css`, and `assets/styles/theme-vars.css` for alias targets.
//!
//! A swatch or radius step naming a role the theme does not define paints
//! nothing, which reads as "that token is fine" when it does not exist — the
//! gallery would be lying about the palette it documents.

use std::collections::BTreeSet;

use roost_web::components::design::catalog::SHAPE_STEPS;
use roost_web::components::design::color_roles::COLOR_GROUPS;
use roost_web::theme::choice::managed_properties;

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

/// Every custom property the stylesheet REFERENCES through `var(--name)`.
fn referenced_properties(css: &str) -> BTreeSet<String> {
    css.split("var(")
        .skip(1)
        .filter_map(|rest| {
            let name = rest.trim_start().strip_prefix("--")?;
            let end = name
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(name.len());
            Some(format!("--{}", &name[..end]))
        })
        .collect()
}

/// The bodies of every `@theme inline` block, joined: rust-ui's own block and
/// Roost's additions after it. Each holds declarations only, so its first
/// closing brace ends it.
fn theme_blocks(css: &str) -> String {
    css.split("@theme inline")
        .skip(1)
        .map(|rest| rest.split_once('}').map_or(rest, |(block, _)| block))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The names after `prefix` of every theme property that starts with it.
fn theme_names(prefix: &str) -> BTreeSet<String> {
    declared_properties(&theme_blocks(TAILWIND_CSS))
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
fn every_alias_in_theme_vars_resolves_to_a_declared_property() {
    // Roost's aliases read rust-ui roles; the engine writes the rest inline.
    let engine_written = managed_properties()
        .into_iter()
        .map(|name| name.to_string());
    let declared: BTreeSet<String> = declared_properties(TAILWIND_CSS)
        .union(&declared_properties(THEME_VARS))
        .cloned()
        .chain(engine_written)
        .collect();
    for property in referenced_properties(THEME_VARS) {
        assert!(
            declared.contains(&property),
            "{property} is referenced by theme-vars.css but never declared"
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
