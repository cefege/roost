//! Fuzzy model search shared by the web pickers and the harness's `/model`:
//! every whitespace-separated query token must appear, and items rank by how
//! tightly each token matched (whole word, word prefix, substring, scattered).

/// `"{selector} {name}"`, lowercased: what a model is searched by.
pub fn model_search_text(selector: &str, name: &str) -> String {
    format!("{selector} {name}").to_lowercase()
}

/// The subsequence rule's score: a token whose letters only appear scattered.
const SCATTERED: u32 = 3;

/// Lower is better; `None` when any whitespace-separated query token is absent.
pub fn fuzzy_score(haystack: &str, query: &str) -> Option<u32> {
    token_scores(haystack, query).map(|scores| scores.iter().sum())
}

/// Each token's score, or `None` when any token is absent.
fn token_scores(haystack: &str, query: &str) -> Option<Vec<u32>> {
    let haystack = haystack.to_lowercase();
    let query = query.to_lowercase();
    let words: Vec<&str> = haystack
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let parts: Vec<&str> = haystack.split_whitespace().collect();
    query
        .split_whitespace()
        .map(|token| token_score(&haystack, &words, &parts, token))
        .collect()
}

fn token_score(haystack: &str, words: &[&str], parts: &[&str], token: &str) -> Option<u32> {
    let mut pieces = words.iter().chain(parts);
    if pieces.clone().any(|piece| *piece == token) {
        Some(0)
    } else if pieces.any(|piece| piece.starts_with(token)) {
        Some(1)
    } else if haystack.contains(token) {
        Some(2)
    } else if is_subsequence(haystack, token) {
        Some(SCATTERED)
    } else {
        None
    }
}

fn is_subsequence(haystack: &str, token: &str) -> bool {
    let mut remaining = haystack.chars();
    token
        .chars()
        .all(|wanted| remaining.any(|character| character == wanted))
}

/// Indices of `haystacks` matching `query`, best first; ties keep input order
/// (stable sort). A blank query returns every index in order. Scattered
/// letters are a fallback: once some item contains every token as a word,
/// prefix or substring, items that needed scattered letters are dropped, so
/// `opus` does not keep every model whose text happens to spell o…p…u…s.
pub fn rank_matches(haystacks: &[String], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..haystacks.len()).collect();
    }
    let mut scored: Vec<(u32, bool, usize)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(index, haystack)| {
            token_scores(haystack, query).map(|scores| {
                let scattered = scores.contains(&SCATTERED);
                (scores.iter().sum(), scattered, index)
            })
        })
        .collect();
    if scored.iter().any(|(_, scattered, _)| !scattered) {
        scored.retain(|(_, scattered, _)| !scattered);
    }
    scored.sort_by_key(|(score, _, _)| *score);
    scored.into_iter().map(|(_, _, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn haystacks(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn tighter_matches_score_lower() {
        let text = "anthropic/claude-sonnet-5 claude sonnet 5";
        let exact = fuzzy_score(text, "sonnet").unwrap();
        let prefix = fuzzy_score(text, "son").unwrap();
        let substring = fuzzy_score(text, "onne").unwrap();
        let subsequence = fuzzy_score(text, "cs5").unwrap();
        assert!(exact < prefix && prefix < substring && substring < subsequence);
    }

    #[test]
    fn every_token_must_match() {
        assert!(fuzzy_score("claude sonnet 5", "son 5").is_some());
        assert!(fuzzy_score("claude sonnet 4", "son 5").is_none());
        assert_eq!(fuzzy_score("claude sonnet 5", "zzz"), None);
    }

    #[test]
    fn a_blank_query_keeps_input_order() {
        let items = haystacks(&["b", "a", "c"]);
        assert_eq!(rank_matches(&items, "  "), [0, 1, 2]);
    }

    #[test]
    fn ranking_is_best_first_and_stable_on_ties() {
        let items = haystacks(&["opus 5", "sonnet 4", "sonnet 5", "sonnetish"]);
        assert_eq!(rank_matches(&items, "sonnet"), [1, 2, 3]);
        assert_eq!(rank_matches(&items, "sonnet 5"), [2]);
    }

    #[test]
    fn scattered_letters_count_only_when_nothing_matches_closer() {
        let items = haystacks(&[
            "anthropic/claude-sonnet-5 claude sonnet 5",
            "anthropic/claude-opus-5 claude opus 5",
        ]);
        assert_eq!(rank_matches(&items, "opus"), [1]);
        // Nothing contains `cs5` outright, so the scattered matches stay.
        assert_eq!(rank_matches(&items, "cs5"), [0, 1]);
    }
}
