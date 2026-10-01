//! The vocabulary a session remembers, and the arithmetic of forgetting.
//!
//! Keyterm biasing reads what is on screen and what the operator has typed
//! recently; that is a cold start every time. The lexicon is what a session
//! leaves behind for the next one: terms the operator actually said, each one
//! decayed on every merge so a word that stopped being said stops being worth a
//! term.
//!
//! Pure on purpose. Decay, pruning and the cap are arithmetic over a map, and a
//! map is the one thing this tree can test without a browser. The host that
//! reads and writes `roost.keytermLexicon.v1` is `super::lexicon_store`.
//! Ports `apps/web/src/voice/keytermLexicon.ts`.

use std::collections::HashMap;

/// Where the persisted vocabulary lives. Verbatim v2's key.
pub const STORAGE_KEY: &str = "roost.keytermLexicon.v1";

/// What every remembered score is multiplied by on each merge. A term said once
/// is worth keeping through a few sessions of silence and not through a dozen.
pub const DECAY: f64 = 0.9;

/// Below this a term is dropped: its bytes in the URL buy nothing.
pub const PRUNE_BELOW: f64 = 0.3;

/// The most terms one browser remembers. Past this the map is the tail of the
/// vocabulary, which the scoring would have dropped anyway.
pub const MAX_STORED: usize = 200;

/// How many terms ride one connection's keyterms.
pub const TOP_TERMS: usize = 40;

/// A term and how often it has been heard.
pub type Lexicon = HashMap<String, f64>;

/// Fold a session's terms into what the browser already remembers.
///
/// Every remembered score decays, a term that has fallen below the floor is
/// dropped, and the terms heard this session each gain one. The result is capped,
/// best first, so the map cannot grow without bound.
#[must_use]
pub fn merge(previous: &Lexicon, terms: &[String]) -> Lexicon {
    let mut merged: Lexicon = previous
        .iter()
        .map(|(term, score)| (term.clone(), score * DECAY))
        .filter(|(_, score)| *score >= PRUNE_BELOW)
        .collect();
    for term in terms {
        let key = term.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        *merged.entry(key).or_insert(0.0) += 1.0;
    }
    cap(merged)
}

fn cap(mut lexicon: Lexicon) -> Lexicon {
    let mut ranked: Vec<(String, f64)> = lexicon.drain().collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(MAX_STORED);
    ranked.into_iter().collect()
}

/// The best terms this browser remembers, in rank order.
#[must_use]
pub fn top_terms(lexicon: &Lexicon, count: usize) -> Vec<String> {
    let mut ranked: Vec<(&String, &f64)> = lexicon.iter().collect();
    ranked.sort_by(|left, right| right.1.total_cmp(left.1).then_with(|| left.0.cmp(right.0)));
    ranked
        .into_iter()
        .take(count)
        .map(|(term, _)| term.clone())
        .collect()
}

/// Parse a stored lexicon, treating anything unreadable as nothing remembered.
///
/// A browser's storage is not a contract this code controls: another tab, an
/// older build or a hand-edited value can put anything there, and a dictation
/// that refuses to start because of it would be worse than one that forgot.
#[must_use]
pub fn parse(stored: &str) -> Lexicon {
    serde_json::from_str::<HashMap<String, f64>>(stored)
        .ok()
        .filter(|lexicon| {
            lexicon
                .values()
                .all(|score| score.is_finite() && *score > 0.0)
        })
        .map(cap)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{DECAY, Lexicon, MAX_STORED, PRUNE_BELOW, merge, parse, top_terms};

    fn heard(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|term| (*term).to_owned()).collect()
    }

    #[test]
    fn a_term_heard_this_session_scores_one_and_ranks_first() {
        let lexicon = merge(&Lexicon::new(), &heard(&["kysely"]));
        assert_eq!(lexicon.get("kysely"), Some(&1.0));
        assert_eq!(top_terms(&lexicon, 1), heard(&["kysely"]));
    }

    #[test]
    fn every_remembered_score_decays_on_every_merge() {
        let first = merge(&Lexicon::new(), &heard(&["kysely"]));
        let second = merge(&first, &[]);
        assert_eq!(second.get("kysely"), Some(&(1.0 * DECAY)));
        let third = merge(&second, &[]);
        assert_eq!(third.get("kysely"), Some(&(DECAY * DECAY)));
    }

    #[test]
    fn a_term_heard_again_gains_one_on_top_of_its_decay() {
        let first = merge(&Lexicon::new(), &heard(&["kysely"]));
        let second = merge(&first, &heard(&["kysely"]));
        assert_eq!(second.get("kysely"), Some(&(DECAY + 1.0)));
    }

    #[test]
    fn a_term_that_stops_being_said_is_eventually_forgotten() {
        let mut lexicon = merge(&Lexicon::new(), &heard(&["kysely"]));
        let mut rounds = 0;
        while lexicon.contains_key("kysely") && rounds < 100 {
            lexicon = merge(&lexicon, &[]);
            rounds += 1;
        }
        assert!(
            !lexicon.contains_key("kysely"),
            "still remembered after {rounds}"
        );
        // Twelve sessions of silence is where 0.9^n crosses 0.3: the term heard
        // once is still worth a term after a handful of quiet sessions, and not
        // after a dozen. The constants are v2's, so the number is theirs too.
        assert_eq!(rounds, 12);
        assert!(DECAY.powi(11) >= PRUNE_BELOW && DECAY.powi(12) < PRUNE_BELOW);
    }

    #[test]
    fn terms_are_remembered_case_insensitively() {
        let lexicon = merge(&Lexicon::new(), &heard(&["Kysely"]));
        assert!(lexicon.contains_key("kysely"));
    }

    #[test]
    fn the_map_is_capped_best_first() {
        let many: Vec<String> = (0..MAX_STORED + 40)
            .map(|index| format!("term{index}"))
            .collect();
        let lexicon = merge(&Lexicon::new(), &many);
        assert_eq!(lexicon.len(), MAX_STORED);
        // All terms here scored one, so the tie breaks by name and the tail is
        // what went: `term259` is beyond the cap, `term0` is inside it.
        assert!(lexicon.contains_key("term0"));
        assert!(!lexicon.contains_key("term259"));
    }

    #[test]
    fn top_terms_runs_best_first_and_stops_at_the_count() {
        let mut previous = Lexicon::new();
        previous.insert("low".to_owned(), 0.4);
        previous.insert("high".to_owned(), 9.0);
        assert_eq!(top_terms(&previous, 2), heard(&["high", "low"]));
        assert_eq!(top_terms(&previous, 1), heard(&["high"]));
        assert!(top_terms(&Lexicon::new(), 4).is_empty());
    }

    #[test]
    fn stored_json_round_trips_through_the_reader() {
        let lexicon = merge(&Lexicon::new(), &heard(&["kysely", "tailnetd"]));
        let stored = serde_json::to_string(&lexicon).expect("lexicon serialises");
        assert_eq!(parse(&stored), lexicon);
    }

    #[test]
    fn storage_this_code_does_not_control_is_forgotten_rather_than_fatal() {
        assert!(parse("not json").is_empty());
        assert!(parse("").is_empty());
        assert!(parse(r#"{"kysely":"loud"}"#).is_empty());
        assert!(parse(r#"{"kysely":-1}"#).is_empty());
        assert!(parse("{}").is_empty());
    }
}
