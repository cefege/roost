//! Fuzzy model search shared by the web pickers and the harness's `/model`:
//! every whitespace-separated query token must appear, and items rank by how
//! tightly each token matched (whole word, word prefix, substring, scattered).

/// `"{selector} {name}"`, lowercased: what a model is searched by.
pub fn model_search_text(selector: &str, name: &str) -> String {
    format!("{selector} {name}").to_lowercase()
}

/// Lower is better; `None` when any whitespace-separated query token is absent.
pub fn fuzzy_score(haystack: &str, query: &str) -> Option<u32> {
    let haystack = haystack.to_lowercase();
    let query = query.to_lowercase();
    let words: Vec<&str> = haystack
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let parts: Vec<&str> = haystack.split_whitespace().collect();
    query.split_whitespace().try_fold(0, |total, token| {
        token_score(&haystack, &words, &parts, token).map(|score| total + score)
    })
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
        Some(3)
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
/// (stable sort). A blank query returns every index in order.
pub fn rank_matches(haystacks: &[String], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..haystacks.len()).collect();
    }
    let mut scored: Vec<(u32, usize)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(index, haystack)| fuzzy_score(haystack, query).map(|score| (score, index)))
        .collect();
    scored.sort_by_key(|(score, _)| *score);
    scored.into_iter().map(|(_, index)| index).collect()
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
}
