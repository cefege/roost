//! The compact deck bar's count badge: the 1-based position/total fraction,
//! and every case that must fall back to the bare total rather than print a
//! nonsense position. Ports `apps/web/tests/deckTabBadge.test.ts`.

use roost_client_core::deck::{DeckTabBadge, deck_tab_badge};

fn badge(text: &str, description: &str, fraction: bool) -> DeckTabBadge {
    DeckTabBadge {
        text: text.to_owned(),
        description: description.to_owned(),
        fraction,
    }
}

#[test]
fn a_lone_terminal_stays_a_bare_number() {
    assert_eq!(
        deck_tab_badge(1, Some(0)),
        badge("1", "1 terminal in this workspace", false)
    );
}

#[test]
fn first_of_two_reads_one_of_two() {
    assert_eq!(
        deck_tab_badge(2, Some(0)),
        badge("1/2", "terminal 1 of 2 in this workspace", true)
    );
}

#[test]
fn last_of_two_reads_two_of_two() {
    assert_eq!(
        deck_tab_badge(2, Some(1)),
        badge("2/2", "terminal 2 of 2 in this workspace", true)
    );
}

#[test]
fn last_of_five_reads_five_of_five() {
    assert_eq!(
        deck_tab_badge(5, Some(4)),
        badge("5/5", "terminal 5 of 5 in this workspace", true)
    );
}

#[test]
fn an_unknown_active_terminal_falls_back_to_the_total() {
    assert_eq!(
        deck_tab_badge(5, None),
        badge("5", "5 terminals in this workspace", false)
    );
}

#[test]
fn an_index_past_the_end_never_prints_six_of_five() {
    assert_eq!(
        deck_tab_badge(5, Some(5)),
        badge("5", "5 terminals in this workspace", false)
    );
}

#[test]
fn an_empty_list_reads_zero() {
    assert_eq!(
        deck_tab_badge(0, None),
        badge("0", "0 terminals in this workspace", false)
    );
}
