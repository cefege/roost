//! The per-frame deck view: nothing paints before measurement, a compact host
//! paints the followed session's pane full-bleed, the spotlight card and the
//! swipe neighbour claim slots, and parked tabs size to their pane's terminal
//! area. Ports the memos of `apps/web/src/components/deck/terminal-deck-model.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deck_support;

use std::collections::BTreeMap;

use deck_support::*;
use roost_client_core::deck::{
    DeckSize, WarmSet, deck_session_id, deck_view, mobile_tab_ids, mounted_session_ids,
    park_size_by_session, slot_by_session, spotlight_pane, spotlight_rect,
};
use roost_client_core::store::layout::PaneRect;

const DESK: DeckSize = DeckSize {
    w: 1200.0,
    h: 800.0,
};

fn two_panes() -> roost_client_core::store::layout::PaneLayout {
    layout(
        row_split(
            "split",
            leaf("left", &[1, 2], Some(1)),
            leaf("right", &[3], Some(3)),
        ),
        "left",
    )
}

#[test]
fn nothing_paints_before_the_deck_is_measured() {
    let arrangement = two_panes();
    let unmeasured = DeckSize { w: 0.0, h: 800.0 };
    assert!(
        deck_view(
            Some(&arrangement),
            unmeasured,
            &BTreeMap::new(),
            false,
            None
        )
        .panes
        .is_empty()
    );
    assert!(
        deck_view(None, DESK, &BTreeMap::new(), false, None)
            .panes
            .is_empty()
    );
}

#[test]
fn a_compact_host_paints_the_followed_sessions_pane_full_bleed() {
    let arrangement = two_panes();
    let view = deck_view(
        Some(&arrangement),
        DESK,
        &BTreeMap::new(),
        true,
        Some(&sid(2)),
    );
    assert_eq!(view.panes.len(), 1);
    let pane = &view.panes[0];
    assert_eq!(pane.pane_id, "left");
    assert_eq!(
        pane.selected_tab,
        sid(2),
        "the route wins over the stored selection"
    );
    assert_eq!(
        pane.rect,
        PaneRect {
            x: 0.0,
            y: 0.0,
            w: 1200.0,
            h: 800.0
        }
    );
    assert!(pane.focused);
    assert!(view.dividers.is_empty());
    assert_eq!(
        mobile_tab_ids(Some(&arrangement), true),
        vec![sid(1), sid(2), sid(3)]
    );
    assert!(mobile_tab_ids(Some(&arrangement), false).is_empty());
}

#[test]
fn a_divider_mid_drag_paints_at_its_transient_ratio() {
    let arrangement = two_panes();
    let settled = deck_view(Some(&arrangement), DESK, &BTreeMap::new(), false, None);
    let dragging = BTreeMap::from([("split".to_owned(), 0.25)]);
    let moving = deck_view(Some(&arrangement), DESK, &dragging, false, None);
    assert!(moving.panes[0].rect.w < settled.panes[0].rect.w);
    assert_eq!(moving.dividers.len(), 1);
}

#[test]
fn the_spotlight_floats_the_selected_tab_over_an_inset_card() {
    let arrangement = two_panes();
    let view = deck_view(Some(&arrangement), DESK, &BTreeMap::new(), false, None);
    assert!(
        spotlight_pane(&view, Some(&sid(2)), false).is_none(),
        "an unselected tab cannot float"
    );
    assert!(
        spotlight_pane(&view, Some(&sid(3)), true).is_none(),
        "a compact host never floats"
    );
    let pane = spotlight_pane(&view, Some(&sid(3)), false).expect("the right pane floats");
    let card = spotlight_rect(DESK).expect("a measured deck");
    assert_eq!(
        card,
        PaneRect {
            x: 72.0,
            y: 48.0,
            w: 1056.0,
            h: 704.0
        }
    );
    let small = spotlight_rect(DeckSize { w: 300.0, h: 200.0 }).expect("measured");
    assert_eq!(
        (small.x, small.y),
        (24.0, 24.0),
        "the margin never drops below 24px"
    );
    let slots = slot_by_session(&view, Some((pane, card)), None, false);
    let floated = &slots[&sid(3)];
    assert!(floated.spotlit && floated.focused);
    assert_eq!(floated.rect, card);
    assert!(!slots[&sid(1)].spotlit);
    assert!(!slots.contains_key(&sid(2)), "a parked tab has no slot");
}

#[test]
fn the_swipe_neighbour_rides_in_over_the_one_compact_pane() {
    let arrangement = two_panes();
    let view = deck_view(
        Some(&arrangement),
        DESK,
        &BTreeMap::new(),
        true,
        Some(&sid(1)),
    );
    let slots = slot_by_session(&view, None, Some(&sid(2)), true);
    let neighbour = &slots[&sid(2)];
    assert_eq!(neighbour.pane_id, "left");
    assert!(!neighbour.focused);
    assert!(!slot_by_session(&view, None, Some(&sid(2)), false).contains_key(&sid(2)));
}

#[test]
fn parked_tabs_size_to_their_panes_terminal_area() {
    let arrangement = two_panes();
    let view = deck_view(Some(&arrangement), DESK, &BTreeMap::new(), false, None);
    let parks = park_size_by_session(&view, 35.0);
    let left = view.panes[0].rect;
    assert_eq!(
        parks[&sid(2)],
        DeckSize {
            w: left.w,
            h: 765.0
        },
        "a hidden tab parks at its reveal size"
    );
    assert_eq!(parks[&sid(1)], parks[&sid(2)]);
    let squat = deck_view(
        Some(&arrangement),
        DeckSize { w: 1200.0, h: 20.0 },
        &BTreeMap::new(),
        false,
        None,
    );
    assert_eq!(
        park_size_by_session(&squat, 35.0)[&sid(3)].h,
        0.0,
        "never negative"
    );
}

#[test]
fn only_slotted_or_warm_open_sessions_stay_mounted() {
    let arrangement = two_panes();
    let view = deck_view(Some(&arrangement), DESK, &BTreeMap::new(), false, None);
    let slots = slot_by_session(&view, None, None, false);
    let mut warm = WarmSet::new();
    let open = [sid(1), sid(2), sid(3), sid(4)];
    warm.advance(&open.iter().cloned().collect(), &[sid(4)], 8);
    assert_eq!(
        mounted_session_ids(&open, &warm, &slots),
        vec![sid(1), sid(3), sid(4)]
    );
}

#[test]
fn an_overlay_route_keeps_following_the_last_session() {
    assert_eq!(
        deck_session_id(Some("a"), true, Some("b")),
        Some("a".to_owned())
    );
    assert_eq!(
        deck_session_id(None, false, Some("b")),
        Some("b".to_owned())
    );
    assert_eq!(
        deck_session_id(None, true, Some("b")),
        None,
        "a visible surface with no route shows nothing"
    );
}
