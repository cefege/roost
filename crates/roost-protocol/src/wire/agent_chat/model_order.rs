//! The order every model list shows: the newest generation first, read from
//! the version number in the model's display name ("Claude Sonnet 4.5",
//! "GPT-5.1"), then alphabetical. The coordinator sorts `ModelsCatalog.models`
//! with it once, so each picker that iterates the catalog agrees.

use std::cmp::Ordering;

use super::catalog::ModelEntry;

/// A first version component at or above this is a date stamp
/// (`20250929`) or a parameter count, not a model generation.
const MAX_GENERATION: u32 = 1_000;

/// Newest generation first; unversioned names after every versioned one;
/// ties alphabetical by name, then by id.
pub fn compare_models_newest_first(left: &ModelEntry, right: &ModelEntry) -> Ordering {
    compare_newest_first(&left.name, &left.model_id, &right.name, &right.model_id)
}

/// [`compare_models_newest_first`] over a bare display name and model id, for
/// callers that hold some other model type.
pub fn compare_newest_first(
    left_name: &str,
    left_id: &str,
    right_name: &str,
    right_id: &str,
) -> Ordering {
    let left_version = model_version(left_name).or_else(|| model_version(left_id));
    let right_version = model_version(right_name).or_else(|| model_version(right_id));
    match (left_version, right_version) {
        (Some(left_version), Some(right_version)) => right_version.cmp(&left_version),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| left_name.to_lowercase().cmp(&right_name.to_lowercase()))
    .then_with(|| left_id.cmp(right_id))
}

/// The first dotted number in a model name: `"Claude Opus 4.1"` → `[4, 1]`.
pub fn model_version(name: &str) -> Option<Vec<u32>> {
    let characters: Vec<char> = name.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        if !characters[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < characters.len()
            && (characters[index].is_ascii_digit()
                || (characters[index] == '.'
                    && characters.get(index + 1).is_some_and(char::is_ascii_digit)))
        {
            index += 1;
        }
        let run: String = characters[start..index].iter().collect();
        let components: Option<Vec<u32>> = run.split('.').map(|part| part.parse().ok()).collect();
        if let Some(components) = components
            && components
                .first()
                .is_some_and(|generation| *generation < MAX_GENERATION)
        {
            return Some(components);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ModelEntry {
        ModelEntry {
            provider: "anthropic".to_owned(),
            model_id: name.to_lowercase().replace(' ', "-"),
            name: name.to_owned(),
            ..ModelEntry::default()
        }
    }

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut models: Vec<ModelEntry> = names.iter().map(|name| model(name)).collect();
        models.sort_by(compare_models_newest_first);
        models.into_iter().map(|model| model.name).collect()
    }

    #[test]
    fn the_newest_generation_leads_then_names_sort() {
        assert_eq!(
            sorted(&[
                "Claude Haiku 3.5",
                "Claude Opus 4.1",
                "Claude Opus 5",
                "Claude Sonnet 4.5",
                "Claude Sonnet 5.5",
                "Auto",
            ]),
            [
                "Claude Sonnet 5.5",
                "Claude Opus 5",
                "Claude Sonnet 4.5",
                "Claude Opus 4.1",
                "Claude Haiku 3.5",
                "Auto",
            ]
        );
    }

    #[test]
    fn a_minor_version_compares_numerically() {
        assert_eq!(sorted(&["GPT-5.2", "GPT-5.10"]), ["GPT-5.10", "GPT-5.2"]);
    }

    #[test]
    fn a_date_stamp_is_not_a_generation() {
        assert_eq!(model_version("claude-sonnet-20250929"), None);
        assert_eq!(model_version("Llama 3.1 405B"), Some(vec![3, 1]));
        assert_eq!(model_version("Model 2025 v2"), Some(vec![2]));
    }

    #[test]
    fn the_same_generation_falls_back_to_alphabetical() {
        assert_eq!(
            sorted(&["Claude Sonnet 4.5", "Claude Haiku 4.5"]),
            ["Claude Haiku 4.5", "Claude Sonnet 4.5"]
        );
    }
}
