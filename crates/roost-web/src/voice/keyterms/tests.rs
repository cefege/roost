//! The tests for `voice::keyterms`, split out so the module stays under the cap.

use super::*;

fn terms(context: &TerminalContext) -> Vec<String> {
    extract(context)
        .into_iter()
        .map(|keyterm| keyterm.term)
        .collect()
}

fn rank(context: &TerminalContext, term: &str) -> usize {
    let found = terms(context);
    found
        .iter()
        .position(|candidate| candidate == term)
        .unwrap_or_else(|| panic!("{term} missing from {found:?}"))
}

#[test]
fn structural_tokens_outrank_plain_words() {
    assert!(structural_bonus("coordFactory") > structural_bonus("factory"));
    assert!(structural_bonus("compose_dictation") > structural_bonus("dictation"));
    assert!(structural_bonus("TAILSCALE") > structural_bonus("tailscale"));
    assert!(structural_bonus("kysely-query") > structural_bonus("query"));
    assert!(structural_bonus("node22") > structural_bonus("node"));
    assert!(structural_bonus("Kysely") > structural_bonus("kysely"));
}

#[test]
fn numbers_addresses_and_the_common_vocabulary_are_not_terms() {
    assert!(!keep("42"));
    assert!(!keep("a"));
    assert!(!keep("a1b2c3"));
    assert!(!keep("0f9e2b1a"));
    assert!(!keep("the"));
    assert!(!keep("error"));
    assert!(keep("kysely"));
    assert!(keep("coordFactory"));
    assert!(keep("roost-v3"));
}

#[test]
fn a_path_is_reduced_to_the_name_that_would_be_spoken() {
    assert_eq!(normalize("./src/components/voice.rs"), "voice");
    assert_eq!(normalize("crates/roost-web/Cargo.toml"), "Cargo");
    assert_eq!(normalize("(kysely)"), "kysely");
    assert_eq!(normalize("roost-v3"), "roost-v3");
}

#[test]
fn a_compound_name_is_heard_as_words() {
    assert_eq!(
        spoken_form("coordFactory").as_deref(),
        Some("coord factory")
    );
    assert_eq!(
        spoken_form("compose_dictation").as_deref(),
        Some("compose dictation")
    );
    // An acronym is one word, not seven: splitting on every capital would hand
    // the recognizer a phrase nobody said.
    assert_eq!(spoken_form("TAILSCALE"), None);
    assert_eq!(spoken_form("kysely"), None);
    assert_eq!(spoken_form("roost-v3").as_deref(), Some("roost v3"));
    assert_eq!(spoken_form("a=b"), None);
}

#[test]
fn a_keyterm_is_charged_by_the_words_it_contains() {
    assert_eq!(token_count("coordFactory"), 2);
    assert_eq!(token_count("compose dictation"), 2);
    assert_eq!(token_count("kysely"), 1);
}

#[test]
fn the_visible_grid_seeds_the_vocabulary() {
    let context = TerminalContext {
        grid: "running coordFactory on the kysely box".to_owned(),
        ..TerminalContext::default()
    };
    let found = terms(&context);
    assert!(found.iter().any(|term| term == "coordFactory"));
    assert!(found.iter().any(|term| term == "kysely"));
}

#[test]
fn what_the_operator_typed_outranks_what_the_screen_shows() {
    let context = TerminalContext {
        grid: "error in tailnetd".to_owned(),
        input: "coordFactory tailnetd".to_owned(),
        ..TerminalContext::default()
    };
    assert!(rank(&context, "coordFactory") < rank(&context, "tailnetd"));
}

#[test]
fn recent_scrollback_counts_more_than_ancient_scrollback() {
    let context = TerminalContext {
        scrollback: "ancient kysely crash\nfresh roost_coord crash".to_owned(),
        ..TerminalContext::default()
    };
    assert!(rank(&context, "roost_coord") < rank(&context, "kysely"));
}

#[test]
fn a_lexicon_seeds_a_session_with_nothing_on_screen() {
    let context = TerminalContext {
        lexicon: vec!["kysely".to_owned()],
        ..TerminalContext::default()
    };
    assert!(terms(&context).iter().any(|term| term == "kysely"));
}

#[test]
fn a_repeated_word_down_a_log_does_not_outrank_a_typed_one() {
    let context = TerminalContext {
        scrollback: "kysely\nkysely\nkysely\nkysely".to_owned(),
        input: "coordFactory".to_owned(),
        ..TerminalContext::default()
    };
    assert!(rank(&context, "coordFactory") < rank(&context, "kysely"));
}

#[test]
fn a_project_product_name_is_carried_as_a_phrase() {
    let context = TerminalContext {
        grid: "loading Roost Coord now".to_owned(),
        ..TerminalContext::default()
    };
    assert!(terms(&context).iter().any(|term| term == "Roost Coord"));
}

#[test]
fn the_vocabulary_is_bounded_so_a_noisy_terminal_cannot_blow_the_url() {
    let grid = (0..400)
        .map(|index| format!("token{index}"))
        .collect::<Vec<String>>()
        .join(" ");
    let found = extract(&TerminalContext {
        grid,
        ..TerminalContext::default()
    });
    assert!(found.len() <= MAX_KEYTERM_ENTRIES, "{} terms", found.len());
}

#[test]
fn an_empty_context_produces_no_keyterms() {
    assert!(extract(&TerminalContext::default()).is_empty());
}
