//! Named sets of all-folders model roles, saved and switched as a whole, and
//! the starter presets every install offers beside the saved ones.

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

/// The starter presets: one provider's models in every chat role, the
/// strongest on Default, Planning and Deep review, the fastest on Quick and
/// Side tasks. Roles left out inherit as usual (Advisor follows Deep review,
/// Decision checks pick automatically). A saved preset of the same name
/// replaces a starter; deleting that saved preset brings the starter back.
pub const BUILTIN_PRESETS: &[(&str, &[(&str, &str)])] = &[
    (
        "Anthropic",
        &[
            ("default", "anthropic/claude-opus-5-5"),
            ("plan", "anthropic/claude-opus-5-5"),
            ("slow", "anthropic/claude-fable-5-1"),
            ("task", "anthropic/claude-sonnet-5-5"),
            ("smol", "anthropic/claude-haiku-5-5"),
            ("tiny", "anthropic/claude-haiku-5-5"),
        ],
    ),
    (
        "OpenAI",
        &[
            ("default", "openai-codex/gpt-6.1-sol"),
            ("plan", "openai-codex/gpt-6.1-sol"),
            ("slow", "openai-codex/gpt-6-astra"),
            ("task", "openai-codex/gpt-6.1-sol"),
            ("smol", "openai-codex/gpt-6-luna"),
            ("tiny", "openai-codex/gpt-6-luna"),
        ],
    ),
];

/// Whether `name` is a starter preset.
pub fn is_builtin_preset(name: &str) -> bool {
    BUILTIN_PRESETS.iter().any(|(builtin, _)| *builtin == name)
}

/// Every preset a user can apply: the starters, each replaced by a saved
/// preset of the same name, then the other saved presets.
pub fn presets_with_builtins(saved: &ModelPresets) -> ModelPresets {
    let mut presets: ModelPresets = BUILTIN_PRESETS
        .iter()
        .map(|(name, roles)| {
            let roles = roles
                .iter()
                .map(|(role, chain)| ((*role).to_owned(), (*chain).to_owned()))
                .collect();
            ((*name).to_owned(), roles)
        })
        .collect();
    presets.extend(
        saved
            .iter()
            .map(|(name, roles)| (name.clone(), roles.clone())),
    );
    presets
}

/// The roles after applying `preset`: each role it names takes its chain, and
/// every role it leaves out keeps its current chain, so a starter that does
/// not mention Decision checks or Web search never clears them.
pub fn apply_preset(
    current: &BTreeMap<String, String>,
    preset: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut roles = current.clone();
    for (role, chain) in set_roles(preset) {
        roles.insert(role.to_owned(), chain.to_owned());
    }
    roles
}

/// First preset (map order) whose every set role matches `roles`, so a preset
/// reads as in use after it is applied whatever else is set.
pub fn active_preset<'a>(
    presets: &'a ModelPresets,
    roles: &BTreeMap<String, String>,
) -> Option<&'a str> {
    let current = set_roles(roles);
    presets
        .iter()
        .find(|(_, preset)| {
            let wanted = set_roles(preset);
            !wanted.is_empty()
                && wanted
                    .iter()
                    .all(|(role, chain)| current.get(role) == Some(chain))
        })
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

    #[test]
    fn starters_are_always_offered_and_a_saved_preset_of_the_same_name_replaces_one() {
        let mut saved = ModelPresets::new();
        saved.insert(
            "OpenAI".to_owned(),
            roles(&[("default", "openai-codex/gpt-6-astra")]),
        );
        saved.insert("fast".to_owned(), roles(&[("default", "a/x")]));
        let presets = presets_with_builtins(&saved);
        assert_eq!(
            presets["Anthropic"].get("default").map(String::as_str),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(
            presets["OpenAI"],
            roles(&[("default", "openai-codex/gpt-6-astra")])
        );
        assert!(presets.contains_key("fast"));
        assert!(
            BUILTIN_PRESETS
                .iter()
                .all(|(name, _)| valid_preset_name(name))
        );
    }

    #[test]
    fn applying_a_preset_keeps_the_roles_it_does_not_name_and_reads_as_active() {
        let current = roles(&[
            ("default", "a/x"),
            ("judge", "typesafe/jev"),
            ("web", "web/exa"),
        ]);
        let presets = presets_with_builtins(&ModelPresets::new());
        let applied = apply_preset(&current, &presets["Anthropic"]);
        assert_eq!(
            applied.get("judge").map(String::as_str),
            Some("typesafe/jev")
        );
        assert_eq!(applied.get("web").map(String::as_str), Some("web/exa"));
        assert_eq!(
            applied.get("default").map(String::as_str),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(active_preset(&presets, &applied), Some("Anthropic"));
    }
}
