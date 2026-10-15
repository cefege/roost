//! Named sets of all-folders model roles, saved and switched as a whole.

use std::collections::BTreeMap;

/// Preset name → role name → selector chain (same strings as `model_roles`).
pub type ModelPresets = BTreeMap<String, BTreeMap<String, String>>;

pub const PRESET_NAME_HINT: &str =
    "use a letter, then letters, digits, - or _ (at most 64 characters)";

const MAX_PRESET_NAME: usize = 64;

/// `^[A-Za-z][A-Za-z0-9_-]*$`, 1..=64 chars.
pub fn valid_preset_name(name: &str) -> bool {
    let mut characters = name.chars();
    name.len() <= MAX_PRESET_NAME
        && characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

/// First preset (map order) whose roles equal `roles`, ignoring entries whose
/// value is blank on either side.
pub fn active_preset<'a>(
    presets: &'a ModelPresets,
    roles: &BTreeMap<String, String>,
) -> Option<&'a str> {
    let wanted = set_roles(roles);
    presets
        .iter()
        .find(|(_, preset)| set_roles(preset) == wanted)
        .map(|(name, _)| name.as_str())
}

fn set_roles(roles: &BTreeMap<String, String>) -> BTreeMap<&str, &str> {
    roles
        .iter()
        .map(|(role, chain)| (role.as_str(), chain.trim()))
        .filter(|(_, chain)| !chain.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(role, chain)| ((*role).to_owned(), (*chain).to_owned()))
            .collect()
    }

    #[test]
    fn preset_names_start_with_a_letter() {
        for name in ["a", "fast-1", "A_b"] {
            assert!(valid_preset_name(name), "{name}");
        }
        let long = "a".repeat(65);
        for name in ["", "1a", "a b", long.as_str()] {
            assert!(!valid_preset_name(name), "{name}");
        }
        assert!(valid_preset_name(&"a".repeat(64)));
    }

    #[test]
    fn the_active_preset_ignores_blank_roles() {
        let mut presets = ModelPresets::new();
        presets.insert("fast".to_owned(), roles(&[("default", "a/x")]));
        let current = roles(&[("default", "a/x"), ("smol", " ")]);
        assert_eq!(active_preset(&presets, &current), Some("fast"));
        let changed = roles(&[("default", "a/y")]);
        assert_eq!(active_preset(&presets, &changed), None);
    }
}
