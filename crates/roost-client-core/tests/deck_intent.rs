//! The deck intents against a real core: seeding and route sync, tab and pane
//! selection with the spotlight follow, close with its undo, arrange, and the
//! stored arrangements surviving a reload. Ports the behaviour of
//! `apps/web/src/lib/deckOps.ts` and `apps/web/tests/deckRouteSelection.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deck_support;

use std::rc::Rc;

use deck_support::*;
use roost_client_core::MemoryKeyValueStore;
use roost_client_core::deck::DeckIntent;
use roost_client_core::store::layout::{ArrangeKind, PresetKind, all_leaves, find_leaf_of_tab};
use roost_client_core::store::pending_close::{CloseLabels, is_pending_close};

fn observed(ns: &[u8], followed: Option<u8>, compact: bool) -> DeckIntent {
    DeckIntent::Observed {
        folder: Some(folder(ns)),
        followed_session_id: followed.map(sid),
        compact,
        visible_pane_count: 1,
    }
}

#[test]
fn observing_seeds_the_folder_once_and_pulls_the_route_into_focus() {
    let mut core = core();
    deck(&mut core, observed(&[1, 2], Some(2), false));
    let seeded = stored_layout(&core);
    let leaves = all_leaves(&seeded.root);
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0].tabs, vec![sid(1), sid(2)]);
    assert_eq!(
        leaves[0].selected_tab,
        sid(2),
        "the route's session is selected"
    );
    let revision = core.store().revision();
    deck(&mut core, observed(&[1, 2], Some(2), false));
    assert_eq!(
        core.store().revision(),
        revision,
        "a settled observation writes nothing"
    );
    assert_eq!(stored_layout(&core), seeded, "pane ids do not churn");
}

#[test]
fn a_focused_empty_compact_deck_keeps_route_focus_and_selection_view_only() {
    let mut core = core();
    let before = layout(
        row_split(
            "split",
            leaf("empty", &[], None),
            leaf("live", &[1], Some(1)),
        ),
        "empty",
    );
    stored(&mut core, before.clone());
    deck(&mut core, observed(&[1], Some(1), true));
    deck(
        &mut core,
        DeckIntent::FocusPane {
            folder: folder(&[1]),
            pane_id: "live".to_owned(),
            compact: true,
        },
    );
    assert_eq!(
        navigated(&core),
        None,
        "neither the route sync nor the focus moved anything"
    );
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder(&[1]),
            session_id: sid(1),
            compact: true,
        },
    );
    assert_eq!(
        stored_layout(&core),
        before,
        "imported desktop focus survives the phone"
    );
    assert_eq!(
        navigated(&core),
        Some(format!("/s/{}", sid(1))),
        "the tap still navigates"
    );
}

#[test]
fn selecting_a_tab_persists_it_and_navigates_once_per_request() {
    let mut core = core();
    deck(&mut core, observed(&[1, 2], Some(1), false));
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder(&[1, 2]),
            session_id: sid(2),
            compact: false,
        },
    );
    assert_eq!(
        all_leaves(&stored_layout(&core).root)[0].selected_tab,
        sid(2)
    );
    let first = core
        .store()
        .deck
        .navigation()
        .cloned()
        .expect("a navigation");
    assert_eq!(first.path, format!("/s/{}", sid(2)));
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder(&[1, 2]),
            session_id: sid(2),
            compact: false,
        },
    );
    let second = core
        .store()
        .deck
        .navigation()
        .cloned()
        .expect("a navigation");
    assert!(
        second.sequence > first.sequence,
        "a repeated click is a new request"
    );
}

#[test]
fn a_tab_swapped_inside_the_floated_pane_keeps_the_spotlight() {
    let mut core = core();
    open_sessions(&mut core, &[1, 2, 3]);
    stored(
        &mut core,
        layout(
            row_split(
                "split",
                leaf("left", &[1, 2], Some(1)),
                leaf("right", &[3], Some(3)),
            ),
            "left",
        ),
    );
    deck(
        &mut core,
        DeckIntent::ToggleSpotlight {
            folder: folder(&[1, 2, 3]),
        },
    );
    assert_eq!(core.store().spotlight.session_id(), Some(sid(1).as_str()));
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder(&[1, 2, 3]),
            session_id: sid(2),
            compact: false,
        },
    );
    assert_eq!(
        core.store().spotlight.session_id(),
        Some(sid(2).as_str()),
        "the card follows its pane"
    );
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder(&[1, 2, 3]),
            session_id: sid(3),
            compact: false,
        },
    );
    assert_eq!(
        core.store().spotlight.session_id(),
        Some(sid(2).as_str()),
        "another pane's tab leaves it"
    );
    deck(
        &mut core,
        DeckIntent::ToggleSpotlight {
            folder: folder(&[1, 2, 3]),
        },
    );
    assert_eq!(
        core.store().spotlight.session_id(),
        None,
        "the toggle puts it back"
    );
}

#[test]
fn focusing_a_pane_navigates_to_its_tab_and_refocusing_is_a_no_op() {
    let mut core = core();
    stored(
        &mut core,
        layout(
            row_split(
                "split",
                leaf("left", &[1], Some(1)),
                leaf("right", &[2], Some(2)),
            ),
            "left",
        ),
    );
    deck(
        &mut core,
        DeckIntent::FocusPane {
            folder: folder(&[1, 2]),
            pane_id: "right".to_owned(),
            compact: false,
        },
    );
    assert_eq!(stored_layout(&core).focused_pane_id, "right");
    assert_eq!(navigated(&core), Some(format!("/s/{}", sid(2))));
    let revision = core.store().revision();
    deck(
        &mut core,
        DeckIntent::FocusPane {
            folder: folder(&[1, 2]),
            pane_id: "right".to_owned(),
            compact: false,
        },
    );
    assert_eq!(core.store().revision(), revision);
}

fn close(core: &mut roost_client_core::ClientCore, live: &[u8], n: u8, active: Option<u8>) {
    deck(
        core,
        DeckIntent::CloseTab {
            folder: Some(folder(live)),
            session_id: sid(n),
            active_session_id: active.map(sid),
            labels: CloseLabels::default(),
        },
    );
}

#[test]
fn closing_the_viewed_tab_lands_where_the_layout_will_paint_and_undo_restores_the_tiling() {
    let mut core = core();
    let before = layout(
        row_split(
            "split",
            leaf("left", &[1], Some(1)),
            leaf("right", &[2], Some(2)),
        ),
        "right",
    );
    stored(&mut core, before.clone());
    close(&mut core, &[1, 2], 2, Some(2));
    assert!(is_pending_close(core.store(), &sid(2)));
    let after = stored_layout(&core);
    assert_eq!(
        all_leaves(&after.root).len(),
        1,
        "the closed pane collapses"
    );
    assert_eq!(navigated(&core), Some(format!("/s/{}", sid(1))));
    deck(&mut core, DeckIntent::UndoClose { session_id: sid(2) });
    assert!(!is_pending_close(core.store(), &sid(2)));
    assert_eq!(
        stored_layout(&core),
        before,
        "the exact pre-close tiling comes back"
    );
    assert_eq!(
        navigated(&core),
        Some(format!("/s/{}", sid(2))),
        "the view follows it back"
    );
}

#[test]
fn closing_an_unviewed_tab_does_not_move_the_route_and_the_last_tab_goes_home() {
    let mut core = core();
    deck(&mut core, observed(&[1, 2], Some(1), false));
    close(&mut core, &[1, 2], 2, Some(1));
    assert_eq!(navigated(&core), None);
    close(&mut core, &[1], 1, Some(1));
    assert_eq!(navigated(&core), Some("/".to_owned()));
}

#[test]
fn arrange_rebuilds_from_the_live_set_and_keeps_the_route_session_selected() {
    let mut core = core();
    deck(&mut core, observed(&[1, 2, 3], Some(3), false));
    let arrange = DeckIntent::Arrange {
        folder: folder(&[1, 2, 3]),
        kind: ArrangeKind::Preset(PresetKind::Even),
        active_session_id: Some(sid(3)),
    };
    deck(&mut core, arrange);
    let arranged = stored_layout(&core);
    assert_eq!(all_leaves(&arranged.root).len(), 3);
    let holding = find_leaf_of_tab(&arranged.root, &sid(3)).expect("session 3 placed");
    assert_eq!(
        arranged.focused_pane_id, holding.pane_id,
        "focus follows the route"
    );
}

#[test]
fn a_folder_change_drops_the_spotlight_but_the_first_observation_does_not() {
    let mut core = core();
    open_sessions(&mut core, &[1, 2]);
    deck(
        &mut core,
        DeckIntent::ToggleSpotlight {
            folder: folder(&[1, 2]),
        },
    );
    deck(&mut core, observed(&[1, 2], Some(1), false));
    assert_eq!(core.store().spotlight.session_id(), Some(sid(1).as_str()));
    let elsewhere = DeckIntent::Observed {
        folder: None,
        followed_session_id: None,
        compact: false,
        visible_pane_count: 0,
    };
    deck(&mut core, elsewhere);
    assert_eq!(core.store().spotlight.session_id(), None);
}

#[test]
fn split_mints_a_fresh_pane_and_the_arrangement_survives_a_reload() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut first = core_over(&storage);
    deck(&mut first, observed(&[1, 2], Some(1), false));
    let split = DeckIntent::SplitPane {
        folder: folder(&[1, 2]),
        pane_id: stored_layout(&first).focused_pane_id,
        direction: roost_protocol::layout::document::LayoutDirection::Row,
        tab_id: sid(2),
        insert_first: false,
    };
    deck(&mut first, split);
    let committed = stored_layout(&first);
    let panes: Vec<String> = all_leaves(&committed.root)
        .iter()
        .map(|leaf| leaf.pane_id.clone())
        .collect();
    assert_eq!(panes.len(), 2);
    assert_ne!(panes[0], panes[1]);
    assert_eq!(navigated(&first), Some(format!("/s/{}", sid(2))));
    let reloaded = core_over(&storage);
    assert_eq!(stored_layout(&reloaded), committed);
}
