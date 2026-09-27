//! The "Arrange" presets and the one preset that does not rebuild.
//!
//! Mirrors `apps/web/tests/paneLayoutPresets.test.ts`, with the case the port
//! makes load-bearing added: a preset that builds a ratio the portable
//! document would refuse produces an arrangement this client cannot share, so
//! the ratio is clamped where it is built.

mod layout_support;

use roost_client_core::store::layout::{
    ArrangeKind, PaneLayout, PaneNode, PresetKind, all_leaves, arrange_layout, balance_layout,
    export_layout_document, find_leaf, preset_layout,
};
use roost_protocol::layout::LayoutDirection;

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
        let layout = ok(preset_layout(kind, &ids(), &mut source), "preset");
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
    let even = ok(preset_layout(PresetKind::Even, &ids(), &mut source), "even");
    let mut even_dirs = Vec::new();
    directions(&even.root, &mut even_dirs);
    // Equal columns: every split on this build's tree is a row.
    assert!(even_dirs.iter().all(|dir| *dir == LayoutDirection::Row));

    let rows = ok(preset_layout(PresetKind::Rows, &ids(), &mut source), "rows");
    let mut row_dirs = Vec::new();
    directions(&rows.root, &mut row_dirs);
    assert!(row_dirs.iter().all(|dir| *dir == LayoutDirection::Col));

    // A grid alternates, so the two axes both appear.
    let tiled = ok(
        preset_layout(PresetKind::Tiled, &ids(), &mut source),
        "tiled",
    );
    let mut tiled_dirs = Vec::new();
    directions(&tiled.root, &mut tiled_dirs);
    assert!(tiled_dirs.contains(&LayoutDirection::Row));
    assert!(tiled_dirs.contains(&LayoutDirection::Col));

    // Main-vertical is one row split with the rest stacked in a column.
    let main = ok(
        preset_layout(PresetKind::MainVertical, &ids(), &mut source),
        "main-vertical",
    );
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
        let layout = ok(preset_layout(kind, &ids(), &mut source), "preset");
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
    let single = ok(
        preset_layout(PresetKind::Tiled, &session_ids(&["a"]), &mut source),
        "one session",
    );
    assert!(matches!(single.root, PaneNode::Leaf(_)));
    assert_eq!(all_leaves(&single.root).len(), 1);

    // An empty session id is filtered rather than becoming an empty pane.
    let filtered = ok(
        preset_layout(PresetKind::Even, &session_ids(&["a", ""]), &mut source),
        "filtered",
    );
    assert_eq!(all_leaves(&filtered.root).len(), 1);
}

#[test]
fn balance_keeps_the_tree_and_only_moves_ratios() {
    let mut source = CountedIds::new("preset");
    let skewed = ok(preset_layout(PresetKind::Even, &ids(), &mut source), "even");
    // Push the root split to one end, the way a drag would.
    let mut dragged = skewed.clone();
    if let PaneNode::Split(split) = &mut dragged.root {
        split.ratio = 0.9;
    }
    let balanced: PaneLayout = ok(balance_layout(&dragged), "balance");
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
    // Every split now takes its subtree's share, so no divider sits at an end.
    let mut ratios = Vec::new();
    fn collect(node: &PaneNode, out: &mut Vec<f64>) {
        match node {
            PaneNode::Leaf(_) => {}
            PaneNode::Split(split) => {
                out.push(split.ratio);
                collect(&split.a, out);
                collect(&split.b, out);
            }
        }
    }
    collect(&balanced.root, &mut ratios);
    assert!(
        ratios.iter().all(|ratio| (0.4..=0.6).contains(ratio)),
        "{ratios:?}"
    );
}

#[test]
fn arrange_dispatches_a_rebuild_and_a_rebalance() {
    let mut source = CountedIds::new("preset");
    let existing = ok(preset_layout(PresetKind::Even, &ids(), &mut source), "even");
    // Balance is the only non-rebuild: the arrangement it returns still holds
    // this client's pane ids and tab groups.
    let balanced = ok(
        arrange_layout(ArrangeKind::Balance, &existing, &ids(), &mut source),
        "balance",
    );
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
            Some(wire.to_owned())
        );
    }
    assert_eq!(ArrangeKind::parse("balance"), Some(ArrangeKind::Balance));
    assert_eq!(ArrangeKind::parse("diagonal"), None);
}
