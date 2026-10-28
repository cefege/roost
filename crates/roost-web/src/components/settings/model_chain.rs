//! The ordered fallback chain a role stores: one comma-separated selector
//! string, first candidate first. Entries are kept verbatim, so an `@role`
//! reference or a `:thinking` suffix survives an edit the UI does not
//! understand. Pure; the editor component and the scope rules call these.

/// Splits a saved chain into its candidates, dropping blanks.
pub fn parse_chain(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The saved form of a chain. Empty means the role is unset and inherits.
pub fn join_chain(entries: &[String]) -> String {
    entries.join(",")
}

/// Appends `entry` unless the chain already names it. Returns whether it was added.
pub fn push_entry(chain: &mut Vec<String>, entry: &str) -> bool {
    if chain.iter().any(|existing| existing == entry) {
        return false;
    }
    chain.push(entry.to_owned());
    true
}

/// Moves the entry at `source` to `target`, shifting intervening entries.
/// Returns false when either index is outside the chain or both are equal.
pub fn move_entry_to_position(chain: &mut [String], source: usize, target: usize) -> bool {
    if source >= chain.len() || target >= chain.len() || source == target {
        return false;
    }
    if source < target {
        chain[source..=target].rotate_left(1);
    } else {
        chain[target..=source].rotate_right(1);
    }
    true
}

/// Moves the entry at `index` by `step` places. Returns false at either edge.
pub fn move_entry(chain: &mut [String], index: usize, step: isize) -> bool {
    let Some(target) = index.checked_add_signed(step) else {
        return false;
    };
    move_entry_to_position(chain, index, target)
}

/// Splits a trailing `:thinking` level off a candidate. Only a suffix the
/// catalog names (or `auto`) counts, so a model id that contains a colon
/// stays whole.
pub fn split_thinking<'a>(token: &'a str, levels: &[String]) -> (&'a str, Option<&'a str>) {
    match token.rsplit_once(':') {
        Some((base, suffix)) if suffix == "auto" || levels.iter().any(|level| level == suffix) => {
            (base, Some(suffix))
        }
        _ => (token, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_of(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|entry| (*entry).to_owned()).collect()
    }

    #[test]
    fn parse_keeps_order_and_drops_blank_candidates() {
        assert_eq!(
            parse_chain(" anthropic/a:high , ,openai/b ,"),
            chain_of(&["anthropic/a:high", "openai/b"])
        );
        assert!(parse_chain("  ").is_empty());
    }

    #[test]
    fn join_round_trips_a_parsed_chain() {
        let saved = "@smol,anthropic/a:high,web/exa";
        assert_eq!(join_chain(&parse_chain(saved)), saved);
    }

    #[test]
    fn push_appends_once() {
        let mut chain = chain_of(&["a/x"]);
        assert!(push_entry(&mut chain, "b/y"));
        assert!(!push_entry(&mut chain, "a/x"));
        assert_eq!(chain, chain_of(&["a/x", "b/y"]));
    }

    #[test]
    fn moving_to_position_shifts_intervening_entries_and_preserves_tokens() {
        let mut chain = chain_of(&["a/x:high", "b/y:auto", "@role:low", "d/z"]);
        assert!(move_entry_to_position(&mut chain, 2, 0));
        assert_eq!(
            chain,
            chain_of(&["@role:low", "a/x:high", "b/y:auto", "d/z"])
        );
        assert!(move_entry_to_position(&mut chain, 0, 2));
        assert_eq!(
            chain,
            chain_of(&["a/x:high", "b/y:auto", "@role:low", "d/z"])
        );
    }

    #[test]
    fn moving_to_same_or_outside_position_is_a_no_op() {
        let mut chain = chain_of(&["a", "b", "c"]);
        assert!(!move_entry_to_position(&mut chain, 1, 1));
        assert!(!move_entry_to_position(&mut chain, 3, 0));
        assert!(!move_entry_to_position(&mut chain, 0, 3));
        assert_eq!(chain, chain_of(&["a", "b", "c"]));
    }

    #[test]
    fn neighbor_moves_shift_instead_of_swapping() {
        let mut chain = chain_of(&["a", "b", "c"]);
        assert!(move_entry(&mut chain, 0, 1));
        assert_eq!(chain, chain_of(&["b", "a", "c"]));
        assert!(move_entry(&mut chain, 2, -1));
        assert_eq!(chain, chain_of(&["b", "c", "a"]));
        assert!(!move_entry(&mut chain, 0, -1));
        assert!(!move_entry(&mut chain, 2, 1));
    }

    #[test]
    fn thinking_suffix_splits_only_for_known_levels() {
        let levels = chain_of(&["low", "high"]);
        assert_eq!(split_thinking("p/m:high", &levels), ("p/m", Some("high")));
        assert_eq!(split_thinking("p/m:auto", &levels), ("p/m", Some("auto")));
        assert_eq!(split_thinking("p/m:v2", &levels), ("p/m:v2", None));
    }
}
