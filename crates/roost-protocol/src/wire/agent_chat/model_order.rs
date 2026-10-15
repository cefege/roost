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
    version_span(&characters).map(|(_, _, components)| components)
}

/// What stays of a model's name once its version is gone, lowercased and
/// reduced to words: "Claude Sonnet 4.5 (latest)" and "Claude Sonnet 5.5" are
/// both `claude sonnet`, "GPT-5.1 Codex" is `gpt codex`, "Llama 3.1 70B" is
/// `llama 70b`. Versions of one model share a family; the id stands in only
/// for a blank name (ids carry dates like `2025-07-28` that read as versions).
/// Lists keep the newest model of each family.
pub fn model_family(name: &str, id: &str) -> String {
    let source = if name.trim().is_empty() { id } else { name };
    let mut characters: Vec<char> = source.to_lowercase().chars().collect();
    if let Some((start, end, _)) = version_span(&characters) {
        characters.splice(start..end, [' ']);
    }
    let mut family = String::new();
    let mut word = String::new();
    let mut depth = 0_usize;
    for character in characters {
        match character {
            '(' | '[' => {
                push_word(&mut word, &mut family);
                depth += 1;
            }
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ if character.is_alphanumeric() => word.push(character),
            _ => push_word(&mut word, &mut family),
        }
    }
    push_word(&mut word, &mut family);
    family
}

/// Appends a finished word to a family, skipping a bare date stamp
/// (`20250929`, `0728`): that is not part of the model's name.
fn push_word(word: &mut String, family: &mut String) {
    let digits = word.chars().all(|character| character.is_ascii_digit());
    let date = digits && (word.len() >= 6 || (word.len() >= 2 && word.starts_with('0')));
    if !word.is_empty() && !date {
        if !family.is_empty() {
            family.push(' ');
        }
        family.push_str(word);
    }
    word.clear();
}

/// The first dotted number that is a generation, as character offsets and
/// components. A number fused to a size suffix (`70B`, `8x7b`) is a parameter
/// count. A date is never one: a four-digit year takes its dashed month and
/// day along (`2025-07-28`), a number at or above `MAX_GENERATION` is a
/// stamp, and so is a zero-led one (`0728`).
fn version_span(characters: &[char]) -> Option<(usize, usize, Vec<u32>)> {
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
        let size = characters
            .get(index)
            .is_some_and(|next| matches!(next, 'b' | 'B' | 'x' | 'X'));
        let run: String = characters[start..index].iter().collect();
        if run.len() == 4 && run.chars().all(|character| character.is_ascii_digit()) {
            // A year: the `-07-28` after it is part of the same date.
            while characters.get(index) == Some(&'-')
                && characters.get(index + 1).is_some_and(char::is_ascii_digit)
            {
                index += 1;
                while characters.get(index).is_some_and(char::is_ascii_digit) {
                    index += 1;
                }
            }
            continue;
        }
        let stamp =
            run.starts_with('0') && run.chars().nth(1).is_some_and(|next| next.is_ascii_digit());
        let components: Option<Vec<u32>> = run.split('.').map(|part| part.parse().ok()).collect();
        if !size
            && !stamp
            && let Some(components) = components
            && components
                .first()
                .is_some_and(|generation| *generation < MAX_GENERATION)
        {
            return Some((start, index, components));
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
        assert_eq!(model_version("Qwen Plus 0728"), None);
        assert_eq!(model_version("qwen/qwen-plus-2025-07-28"), None);
    }

    #[test]
    fn the_same_generation_falls_back_to_alphabetical() {
        assert_eq!(
            sorted(&["Claude Sonnet 4.5", "Claude Haiku 4.5"]),
            ["Claude Haiku 4.5", "Claude Sonnet 4.5"]
        );
    }

    #[test]
    fn versions_of_one_model_share_a_family() {
        let family = |name: &str| model_family(name, "");
        assert_eq!(family("Claude Sonnet 5.5"), "claude sonnet");
        assert_eq!(family("Claude Sonnet 4.5 (latest)"), "claude sonnet");
        assert_eq!(family("Claude 3.5 Sonnet"), "claude sonnet");
        assert_eq!(family("GPT-5.1 Codex"), "gpt codex");
        assert_ne!(family("GPT-5.1 Codex"), family("GPT-5.5"));
        assert_eq!(
            family("Meta: Llama 3.1 70B Instruct"),
            "meta llama 70b instruct"
        );
        assert_ne!(family("Llama 3.1 70B"), family("Llama 3.1 8B"));
        assert_eq!(family("Gemini 2.5 Flash"), "gemini flash");
        assert_eq!(family("Qwen Plus 0728"), family("Qwen3.7 Plus"));
    }

    #[test]
    fn the_name_decides_the_family_and_a_blank_one_uses_the_id() {
        assert_eq!(
            model_family("Qwen Plus 0728", "qwen/qwen-plus-2025-07-28"),
            "qwen plus"
        );
        assert_eq!(model_family("Sonnet", "claude-sonnet-20250929"), "sonnet");
        assert_eq!(model_family("", "claude-sonnet-5"), "claude sonnet");
    }
}
