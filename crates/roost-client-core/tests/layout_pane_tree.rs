//! The runtime pane tree: the ratio clamp at both ends, and the mutations a
//! tiling gesture takes.
//!
//! Mirrors `apps/web/tests/paneLayout.test.ts`, with the case the port makes
//! load-bearing added: a clamp that has to name every non-finite input before
//! it clamps either end, because a NaN compares false against both bounds.
//!
//! Geometry and the persisted-record bounds live beside this file in
//! `layout_geometry_records.rs`; this one is the tree alone.

mod layout_support;

use roost_client_core::store::layout::{
    PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, close_tab, default_layout, find_leaf,
    move_tab, reconcile, select_tab, set_ratio, split_leaf,
};
use roost_protocol::layout::document::LayoutDirection;
use roost_protocol::layout::{LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN};

use layout_support::{CountedIds, session_ids};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";

fn leaf(pane_id: &str, tabs: &[&str], selected: &str) -> PaneNode {
    PaneNode::Leaf(PaneLeaf {
        pane_id: pane_id.to_owned(),
        tabs: session_ids(tabs),
        selected_tab: selected.to_owned(),
    })
}

fn row_split(id: &str, ratio: f64, a: PaneNode, b: PaneNode) -> PaneNode {
    PaneNode::Split(PaneSplit {
        id: id.to_owned(),
        direction: LayoutDirection::Row,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    })
}

fn split_ratio(node: &PaneNode) -> f64 {
    match node {
        PaneNode::Split(split) => split.ratio,
        PaneNode::Leaf(_) => f64::NAN,
    }
}

fn tabs_of(node: &PaneNode, pane_id: &str) -> Vec<String> {
    find_leaf(node, pane_id)
        .map(|found| found.tabs.clone())
        .unwrap_or_default()
}

#[test]
fn a_ratio_is_clamped_at_both_ends_and_every_non_finite_input_is_named() {
    let tree = row_split(
        "split-1",
        0.5,
        leaf("left", &[ALPHA], ALPHA),
        leaf("right", &[], ""),
    );
    let clamped = |ratio: f64| split_ratio(&set_ratio(&tree, "split-1", ratio));

    // BOTH ENDS. A guard that only refused the small side would let a divider be
    // dragged past the far edge of its own area.
    assert_eq!(clamped(0.0), LAYOUT_RATIO_MIN);
    assert_eq!(clamped(-4.0), LAYOUT_RATIO_MIN);
    assert_eq!(clamped(1.0), LAYOUT_RATIO_MAX);
    assert_eq!(clamped(7.5), LAYOUT_RATIO_MAX);
    // A NaN compares false against both bounds, so a range check alone passes
    // it; the midpoint is the only answer that is inside the range.
    assert_eq!(
        clamped(f64::NAN),
        (LAYOUT_RATIO_MIN + LAYOUT_RATIO_MAX) / 2.0
    );
    assert_eq!(clamped(f64::INFINITY), LAYOUT_RATIO_MAX);
    assert_eq!(clamped(f64::NEG_INFINITY), LAYOUT_RATIO_MIN);
    // A split that is not the addressed one is left exactly as it was.
    assert_eq!(split_ratio(&set_ratio(&tree, "split-other", 0.9)), 0.5);
}

#[test]
fn reconcile_prunes_dead_tabs_and_collapses_only_the_pane_the_prune_emptied() {
    let layout = PaneLayout {
        root: row_split(
            "split-1",
            0.5,
            leaf("p1", &[ALPHA, BETA], ALPHA),
            leaf("p2", &[GAMMA], GAMMA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let reconciled = reconcile(&layout, &session_ids(&[ALPHA, GAMMA]));
    // One tab died in p1 and one did not, so p1 survives holding one tab.
    assert_eq!(tabs_of(&reconciled.root, "p1"), session_ids(&[ALPHA]));
    assert_eq!(reconciled.focused_pane_id, "p1");

    // The same prune, emptying p2 as well: now p2 collapses into p1, and the
    // collapse is a layout decision the prune caused rather than a coincidence.
    let reconciled = reconcile(&layout, &session_ids(&[ALPHA]));
    assert_eq!(
        all_leaves(&reconciled.root).len(),
        1
    );
    assert!(find_leaf(&reconciled.root, "p1").is_some());
    assert!(find_leaf(&reconciled.root, "p2").is_none());
}

#[test]
fn reconcile_appends_a_never_placed_live_session_to_the_focused_pane() {
    let layout = PaneLayout {
        root: leaf("p1", &[ALPHA], ALPHA),
        focused_pane_id: "p1".to_owned(),
    };
    let reconciled = reconcile(&layout, &session_ids(&[ALPHA, BETA]));
    assert_eq!(tabs_of(&reconciled.root, "p1"), session_ids(&[ALPHA, BETA]));
    // The selection does not move under a session the user never picked.
    assert_eq!(
        find_leaf(&reconciled.root, "p1").map(|pane| pane.selected_tab.clone()),
        Some(ALPHA.to_owned())
    );
}

#[test]
fn a_split_moves_the_focus_to_the_new_pane_and_leaves_the_source_holding_the_rest() {
    let mut ids = CountedIds::new("pane");
    let start = default_layout(&session_ids(&[ALPHA, BETA]), &mut ids);
    let split = split_leaf(
        &start,
        "pane-1",
        LayoutDirection::Row,
        BETA,
        false,
        &mut ids,
    );
    let leaves = all_leaves(&split.root);
    assert_eq!(leaves.len(), 2);
    assert_eq!(tabs_of(&split.root, "pane-1"), session_ids(&[ALPHA]));
    // The split mints the NEW PANE first and the divider second, so the pane
    // the gesture created is `pane-2` and the split holding it is `pane-3`.
    assert_eq!(tabs_of(&split.root, "pane-2"), session_ids(&[BETA]));
    assert_eq!(split.focused_pane_id, "pane-2");

    // Splitting a pane by moving its own only tab leaves nothing behind, so the
    // arrangement is refused rather than doubled.
    let refused = split_leaf(
        &start,
        "pane-1",
        LayoutDirection::Col,
        ALPHA,
        true,
        &mut ids,
    );
    assert_eq!(refused, start);
}

#[test]
fn a_move_between_panes_selects_the_moved_tab_and_closes_the_pane_it_emptied() {
    let layout = PaneLayout {
        root: row_split(
            "split-1",
            0.5,
            leaf("p1", &[ALPHA], ALPHA),
            leaf("p2", &[BETA], BETA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let moved = move_tab(&layout, ALPHA, "p2", None);
    // p1 held one tab and the move took it, so p1 collapses into p2 rather than
    // sitting in the deck as a pane with no strip and no way to close.
    assert_eq!(
        all_leaves(&moved.root).len(),
        1
    );
    assert_eq!(tabs_of(&moved.root, "p2"), session_ids(&[BETA, ALPHA]));
    assert_eq!(moved.focused_pane_id, "p2");
    assert_eq!(
        find_leaf(&moved.root, "p2").map(|pane| pane.selected_tab.clone()),
        Some(ALPHA.to_owned())
    );
}

#[test]
fn closing_the_last_tab_of_a_pane_collapses_it_into_its_sibling() {
    let layout = PaneLayout {
        root: row_split(
            "split-1",
            0.5,
            leaf("p1", &[ALPHA, BETA], ALPHA),
            leaf("p2", &[GAMMA], GAMMA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let after_one = close_tab(&layout, ALPHA);
    assert_eq!(tabs_of(&after_one.root, "p1"), session_ids(&[BETA]));
    assert_eq!(
        all_leaves(&after_one.root).len(),
        2
    );

    let after_all = close_tab(&layout, BETA);
    assert_eq!(
        all_leaves(&after_all.root).len(),
        1
    );
    assert_eq!(tabs_of(&after_all.root, "p2"), session_ids(&[GAMMA]));
    // Focus followed the collapse rather than naming a pane that is gone.
    assert!(find_leaf(&after_all.root, &after_all.focused_pane_id).is_some());
}

#[test]
fn selecting_a_tab_focuses_its_pane_and_an_unknown_tab_changes_nothing() {
    let layout = PaneLayout {
        root: row_split(
            "split-1",
            0.5,
            leaf("p1", &[ALPHA], ALPHA),
            leaf("p2", &[BETA], BETA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let selected = select_tab(&layout, BETA);
    assert_eq!(selected.focused_pane_id, "p2");
    assert_eq!(select_tab(&layout, "not-live"), layout);
}
