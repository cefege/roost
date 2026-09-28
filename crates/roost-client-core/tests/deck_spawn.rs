//! A deck spawn's answer against a real core: a refused split is an error card
//! that commits no layout and asks for no route, and admitted spawns land in
//! the pane they were asked from. Ports
//! `apps/web/tests/terminalDeckOperations.split.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deck_support;

use deck_support::*;
use roost_client_core::deck::{DeckIntent, DeckSpawn};
use roost_client_core::store::ToastKind;
use roost_client_core::store::layout::{all_leaves, find_leaf_of_tab};
use roost_protocol::layout::document::LayoutDirection;

fn focused_core() -> roost_client_core::ClientCore {
    let mut core = core();
    open_sessions(&mut core, &[1]);
    deck(
        &mut core,
        DeckIntent::Observed {
            folder: Some(folder(&[1])),
            followed_session_id: Some(sid(1)),
            compact: false,
            visible_pane_count: 1,
        },
    );
    core
}

#[test]
fn a_refused_split_reports_the_failure_and_commits_no_layout_or_route() {
    let mut core = focused_core();
    let before = stored_layout(&core);
    let split = DeckSpawn::Split { pane_id: before.focused_pane_id.clone(), direction: LayoutDirection::Row };
    core.handle(split.refused("coordinator keeper update preparation in progress"));
    let cards: Vec<_> = core.store().toasts.toasts().map(|toast| (toast.msg.clone(), toast.kind)).collect();
    assert_eq!(
        cards,
        vec![("Split terminal failed: coordinator keeper update preparation in progress".to_owned(), ToastKind::Err)],
    );
    assert_eq!(stored_layout(&core), before, "no layout is committed");
    assert_eq!(navigated(&core), None, "no route is asked for");
}

#[test]
fn an_admitted_split_lands_after_its_pane_and_shows_the_new_terminal() {
    let mut core = focused_core();
    let pane_id = stored_layout(&core).focused_pane_id;
    open_sessions(&mut core, &[1, 2]);
    let split = DeckSpawn::Split { pane_id: pane_id.clone(), direction: LayoutDirection::Row };
    deck(&mut core, split.landed(folder(&[1, 2]), sid(2), false));
    let after = stored_layout(&core);
    let leaves = all_leaves(&after.root);
    assert_eq!(leaves.len(), 2, "the pane split in two");
    assert_eq!(leaves[0].pane_id, pane_id, "the new pane lands AFTER the split one");
    assert_eq!(leaves[1].tabs, vec![sid(2)]);
    assert_eq!(navigated(&core), Some(format!("/s/{}", sid(2))));
}

#[test]
fn an_admitted_new_tab_focuses_its_pane_and_the_route_pulls_it_in() {
    let mut core = core();
    open_sessions(&mut core, &[1, 2]);
    stored(&mut core, layout(row_split("split", leaf("left", &[1], None), leaf("right", &[2], None)), "right"));
    open_sessions(&mut core, &[1, 2, 3]);
    let new_tab = DeckSpawn::NewTab { pane_id: "left".to_owned() };
    deck(&mut core, new_tab.landed(folder(&[1, 2, 3]), sid(3), false));
    assert_eq!(stored_layout(&core).focused_pane_id, "left", "the asking pane owns the keyboard");
    assert_eq!(navigated(&core), Some(format!("/s/{}", sid(3))));
    deck(
        &mut core,
        DeckIntent::Observed {
            folder: Some(folder(&[1, 2, 3])),
            followed_session_id: Some(sid(3)),
            compact: false,
            visible_pane_count: 2,
        },
    );
    let landed = core.store().deck.resolve_layout(&folder(&[1, 2, 3]));
    let leaf = find_leaf_of_tab(&landed.root, &sid(3)).expect("the new terminal is in the layout");
    assert_eq!(leaf.pane_id, "left", "the new terminal opens in the pane it was asked from");
    assert_eq!(leaf.selected_tab, sid(3));
}
