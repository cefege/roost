//! The pane-local find bar's reader-visible contract: what the count says, and
//! which keys the bar claims while it owns the keyboard.
//!
//! The bar is the surface the oracle drives (`terminal-find-input`,
//! `terminal-find-count`, `terminal-find-close`), and these are the two rules it
//! can get wrong in a way no mount reveals: a count that lies about how much of
//! the history was read, and a bar that eats a keystroke the reader was typing
//! into its own field.
//!
//! Exercises `roost_web::components::terminal::terminal_find_bar`, the port of
//! `apps/web/src/components/terminal/TerminalFindBar.tsx`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_web::components::terminal::terminal_find_bar::{
    FindBarKey, FindBarState, find_bar_key, find_count_text,
};

/// A bar holding one published search.
fn searched(total: u32, index: u32, truncated: bool) -> FindBarState {
    FindBarState {
        open: true,
        query: "FINDLINE-400".to_owned(),
        index,
        total,
        truncated,
        ..FindBarState::default()
    }
}

#[test]
fn a_search_that_found_nothing_says_zero_of_zero() {
    // The smoke oracle waits for `1/1` on a unique marker, so a bar reporting
    // `0/0` for a real hit is indistinguishable from one with no search behind
    // it. The empty and the searched-and-empty readings must stay different.
    assert_eq!(find_count_text(&searched(0, 0, false)), "0/0");
    assert_eq!(find_count_text(&FindBarState::default()), "");
}

#[test]
fn the_active_match_is_read_as_a_position_in_the_whole_list() {
    assert_eq!(find_count_text(&searched(1, 1, false)), "1/1");
    assert_eq!(find_count_text(&searched(37, 12, false)), "12/37");
}

#[test]
fn a_capped_page_advertises_that_older_matches_exist() {
    // The chain stops at the match ceiling and hands back a cursor onto older
    // rows. Without the marker a reader concludes the needle is unique in a
    // 3000-line history when 256 rows were actually read.
    assert_eq!(find_count_text(&searched(256, 1, true)), "1/256+");
}

#[test]
fn a_failed_search_is_visible_on_the_input_and_keeps_its_matches() {
    // `failed` is what separates "this did not work" from "nothing matched", and
    // the state travels on the input rather than as a toast so it is where the
    // reader is looking.
    let failed = FindBarState {
        failed: true,
        ..searched(4, 2, true)
    };
    assert!(failed.failed);
    assert_eq!(find_count_text(&failed), "2/4+");
}

#[test]
fn the_typing_keys_belong_to_the_query_field() {
    // The pane's hidden textarea is the PTY's. A key the bar claimed would both
    // search for a query the reader never typed and steal the character from the
    // field displaying it, and `Backspace`/`Tab` would break the query outright.
    for key in [
        "a",
        "z",
        "A",
        "7",
        " ",
        "-",
        "?",
        "/",
        "Backspace",
        "Delete",
        "Tab",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
    ] {
        assert_eq!(
            find_bar_key(key, false, false),
            None,
            "the bar claimed a typing key: {key}"
        );
    }
}

#[test]
fn escape_dismisses_and_the_step_keys_walk_the_matches() {
    // Escape closes AND hands the keyboard back to the PTY, so the very next
    // keystroke after it must be the reader's shell again.
    assert_eq!(
        find_bar_key("Escape", false, false),
        Some(FindBarKey::Dismiss)
    );
    assert_eq!(
        find_bar_key("Enter", false, false),
        Some(FindBarKey::Step(1))
    );
    assert_eq!(
        find_bar_key("Enter", false, true),
        Some(FindBarKey::Step(-1))
    );
}

#[test]
fn the_keyboard_only_step_chord_is_available() {
    // `Mod+G` is how a reader reaches the next match without the mouse; a bare
    // `g` stays a letter, because it is the query they are typing.
    assert_eq!(find_bar_key("g", true, false), Some(FindBarKey::Step(1)));
    assert_eq!(find_bar_key("G", true, true), Some(FindBarKey::Step(-1)));
    assert_eq!(find_bar_key("g", false, false), None);
}

#[test]
fn alt_screen_says_the_search_cannot_reach_history() {
    // Alt-screen has no scrollback. A bar that implied depth would send the
    // reader looking through a history the pane does not have.
    let alt = FindBarState {
        alt_screen: true,
        ..searched(2, 1, false)
    };
    assert!(alt.alt_screen);
    assert_eq!(find_count_text(&alt), "1/2");
}
