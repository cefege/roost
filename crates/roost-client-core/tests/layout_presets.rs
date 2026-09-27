//! The "Arrange" presets and the one preset that does not rebuild.
//!
//! Mirrors `apps/web/tests/paneLayoutPresets.test.ts`, with the case the port
//! makes load-bearing added: a preset that builds a ratio the portable
//! document would refuse produces an arrangement this client cannot share, so
//! the ratio is clamped where it is built.

mod layout_support;

use roost_client_core::store::layout::{
    ArrangeKind, PaneNode, PresetKind, all_leaves, arrange_layout, balance_layout,
    export_layout_document, find_leaf, preset_layout,
};
use roost_protocol::layout::document::LayoutDirection;

use layout_support::{CountedIds, ok, session_ids};

const FOLDER: &str = "worker::/work";

fn ids() -> Vec<String> {
    session_ids(&["a", "b", "c", "d", "e"])
}

fn directions(node: &PaneNode, out: &mut Vec<LayoutDirection>) {
    match node {
        PaneNode::Leaf(_) => {}
        PaneNode::Split(split) => {
            out.push(split.direction.clone());
            directions(&split.a, out);
            directions(&split.b, out);
        }
    }
}

#[test]
fn every_preset_puts_one_session_in_one_pane_and_focuses_the_first() {
    for kind in [
        PresetKind::Even,
        PresetKind::Rows,
        PresetKind::Tiled,
        PresetKind::MainVertical,
    ] {
        let mut source = CountedIds::new("preset");
        let layout = preset_layout(kind, &ids(), &mut source);
        let leaves = all_leaves(&layout.root);
        assert_eq!(
            leaves.len(),
            5,
            "{kind:?} did not give every session a pane"
        );
        for leaf in &leaves {
            assert_eq!(leaf.tabs.len(), 1, "{kind:?} put two sessions in a pane");
            assert_eq!(
                leaf.selected_tab, leaf.tabs[0],
                "{kind:?} left a pane unselected"
            );
        }
        assert_eq!(
            layout.focused_pane_id, leaves[0].pane_id,
            "{kind:?} focused another pane"
        );
    }
}

#[test]
fn the_presets_differ_in_the_axes_they_split_on() {
    let mut source = CountedIds::new("preset");
    let even = preset_layout(PresetKind::Even, &ids(), &mut source);
    let mut even_dirs = Vec::new();
    directions(&even.root, &mut even_dirs);
    // Equal columns: every split on this build's tree is a row, and five panes
    // really do produce four splits rather than an empty list that `all` waves
    // through.
    assert_eq!(even_dirs.len(), 4);
    assert!(even_dirs.iter().all(|dir| *dir == LayoutDirection::Row));

    let rows = preset_layout(PresetKind::Rows, &ids(), &mut source);
    let mut row_dirs = Vec::new();
    directions(&rows.root, &mut row_dirs);
    assert_eq!(row_dirs.len(), 4);
    assert!(row_dirs.iter().all(|dir| *dir == LayoutDirection::Col));

    // A grid alternates, so the two axes both appear.
    let tiled = preset_layout(PresetKind::Tiled, &ids(), &mut source);
    let mut tiled_dirs = Vec::new();
    directions(&tiled.root, &mut tiled_dirs);
    assert!(tiled_dirs.contains(&LayoutDirection::Row));
    assert!(tiled_dirs.contains(&LayoutDirection::Col));

    // Main-vertical is one row split with the rest stacked in a column.
    let main = preset_layout(PresetKind::MainVertical, &ids(), &mut source);
    let PaneNode::Split(split) = &main.root else {
        panic!("main-vertical is a split");
    };
    assert_eq!(split.direction, LayoutDirection::Row);
    assert_eq!(split.ratio, 0.6);
    assert_eq!(all_leaves(&split.a).len(), 1);
    assert_eq!(all_leaves(&split.b).len(), 4);
}

#[test]
fn a_preset_produces_a_document_the_shared_parser_admits() {
    for kind in [
        PresetKind::Even,
        PresetKind::Rows,
        PresetKind::Tiled,
        PresetKind::MainVertical,
    ] {
        let mut source = CountedIds::new("preset");
        let layout = preset_layout(kind, &ids(), &mut source);
        // The export re-parses through the coordinator's parser, so this fails
        // for any preset that builds a ratio outside the portable bounds.
        ok(
            export_layout_document(FOLDER, &ids(), &layout),
            "export a preset",
        );
    }
}

#[test]
fn a_folder_of_one_session_gets_a_pane_and_not_a_divider() {
    let mut source = CountedIds::new("preset");
    let single = preset_layout(PresetKind::Tiled, &session_ids(&["a"]), &mut source);
    assert!(matches!(single.root, PaneNode::Leaf(_)));
    assert_eq!(all_leaves(&single.root).len(), 1);

    // An empty session id is filtered rather than becoming an empty pane.
    let filtered = preset_layout(PresetKind::Even, &session_ids(&["a", ""]), &mut source);
    assert_eq!(all_leaves(&filtered.root).len(), 1);
}

#[test]
fn balance_keeps_the_tree_and_only_moves_ratios() {
    let mut source = CountedIds::new("preset");
    let skewed = preset_layout(PresetKind::Even, &ids(), &mut source);
    // Push the root split to one end, the way a drag would.
    let mut dragged = skewed.clone();
    if let PaneNode::Split(split) = &mut dragged.root {
        split.ratio = 0.9;
    }
    let balanced = balance_layout(&dragged);
    assert_eq!(balanced.focused_pane_id, dragged.focused_pane_id);
    let before: Vec<String> = all_leaves(&dragged.root)
        .iter()
        .flat_map(|leaf| leaf.tabs.clone())
        .collect();
    let after: Vec<String> = all_leaves(&balanced.root)
        .iter()
        .flat_map(|leaf| leaf.tabs.clone())
        .collect();
    assert_eq!(before, after, "balance moved a session");
    // Every split now takes the share of the panes in its FIRST subtree, so a
    // five-pane deck is 3/2 at the root and 2/1 below it, and no divider sits at
    // an end where there is nothing left to grab. The dragged 0.9 is gone.
    let PaneNode::Split(root) = &balanced.root else {
        panic!("a balanced split tree is a split");
    };
    assert_eq!(root.ratio, 0.6);
    fn assert_balanced(node: &PaneNode) {
        let PaneNode::Split(split) = node else {
            return;
        };
        let first = all_leaves(&split.a).len();
        let total = first + all_leaves(&split.b).len();
        assert_eq!(split.ratio, first as f64 / total as f64, "unbalanced");
        assert!(split.ratio > 0.1 && split.ratio < 0.9, "ratio at an end");
        assert_balanced(&split.a);
        assert_balanced(&split.b);
    }
    assert_balanced(&balanced.root);
}

#[test]
fn arrange_dispatches_a_rebuild_and_a_rebalance() {
    let mut source = CountedIds::new("preset");
    let existing = preset_layout(PresetKind::Even, &ids(), &mut source);
    // The rebuild arm goes back through the preset, so `arrange_layout` is not
    // a balance with a preset-shaped name on it.
    for kind in [
        PresetKind::Even,
        PresetKind::Rows,
        PresetKind::Tiled,
        PresetKind::MainVertical,
    ] {
        let rebuilt = arrange_layout(ArrangeKind::Preset(kind), &existing, &ids(), &mut source);
        assert_eq!(all_leaves(&rebuilt.root).len(), 5, "{kind:?} rebuilt");
    }

    // Balance is the only non-rebuild: the arrangement it returns still holds
    // this client's pane ids and tab groups.
    let balanced = arrange_layout(ArrangeKind::Balance, &existing, &ids(), &mut source);
    assert_eq!(all_leaves(&balanced.root).len(), 5);
    assert!(find_leaf(&balanced.root, &existing.focused_pane_id).is_some());

    for (wire, kind) in [
        ("even", PresetKind::Even),
        ("rows", PresetKind::Rows),
        ("tiled", PresetKind::Tiled),
        ("main-vertical", PresetKind::MainVertical),
    ] {
        assert_eq!(ArrangeKind::parse(wire), Some(ArrangeKind::Preset(kind)));
        assert_eq!(
            ArrangeKind::parse(wire).map(|kind| kind.as_wire()),
            Some(wire)
        );
    }
    assert_eq!(ArrangeKind::parse("balance"), Some(ArrangeKind::Balance));
    assert_eq!(ArrangeKind::parse("diagonal"), None);
}
