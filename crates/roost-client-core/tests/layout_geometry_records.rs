//! Geometry and persistence: the rects a tree paints at, and the bounds a
//! stored tree is read back under.
//!
//! Split from `layout_pane_tree.rs` because these are the two halves that touch
//! storage and pixels rather than the tree, and because the restore bounds are
//! worth reading beside the geometry's own bounds: both are the same question
//! asked at different ends of the pipeline.

mod layout_support;

use roost_client_core::MemoryKeyValueStore;
use roost_client_core::store::layout::{
    DIVIDER_PX, LAYOUT_TREE_MAX_DEPTH, LayoutRecords, LayoutRecordsError, PaneLayout, PaneLeaf,
    PaneNode, PaneRect, PaneSplit, find_leaf, find_leaf_of_tab, layout_rects, layout_view,
};
use roost_protocol::layout::document::LayoutDirection;

use layout_support::{CountedIds, ok, session_ids};

const ALPHA: &str = "alpha";
const BETA: &str = "beta";
const GAMMA: &str = "gamma";
const FOLDER: &str = "worker::/work";

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

/// The bytes a persisted record holds: a map of folder bucket to tree, which is
/// what `LayoutRecords::restore` deserializes. Serializing a bare `PaneLayout`
/// here would be refused as `NotARecord` for its SHAPE, and a test that wants a
/// bound enforced must not be passing because the payload was the wrong type.
fn record_payload(layout: &PaneLayout) -> String {
    let mut folders = std::collections::BTreeMap::new();
    folders.insert(FOLDER.to_owned(), layout.clone());
    ok(serde_json::to_string(&folders), "encode a persisted record")
}

/// A chain of `levels` splits over one pane, so its deepest leaf sits `levels`
/// below the root. `LayoutRecords` counts a leaf at the root as depth 1, so
/// `levels + 1` is the depth the bound is checked against.
fn nested_tree(levels: usize) -> PaneLayout {
    let mut node = leaf("pane-1", &[ALPHA], ALPHA);
    for level in 0..levels {
        node = PaneNode::Split(PaneSplit {
            id: format!("split-{level}"),
            direction: LayoutDirection::Row,
            ratio: 0.5,
            a: Box::new(node),
            b: Box::new(leaf(&format!("pane-spare-{level}"), &[], "")),
        });
    }
    PaneLayout {
        root: node,
        focused_pane_id: "pane-1".to_owned(),
    }
}

fn tabs_of(node: &PaneNode, pane_id: &str) -> Vec<String> {
    find_leaf(node, pane_id)
        .map(|found| found.tabs.clone())
        .unwrap_or_default()
}

#[test]
fn two_panes_split_the_area_and_the_divider_takes_the_gutter() {
    let layout = PaneLayout {
        root: row_split(
            "split-1",
            0.5,
            leaf("p1", &[ALPHA], ALPHA),
            leaf("p2", &[BETA], BETA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let (panes, dividers) = layout_view(&layout, 1000.0, 500.0);
    assert_eq!(panes.len(), 2);
    assert_eq!(dividers.len(), 1);

    let first = ok(panes.first(), "a first pane");
    let second = ok(panes.get(1), "a second pane");
    let divider = ok(dividers.first(), "a divider");
    // The two panes and the gutter between them fill the width exactly, so a
    // resize that re-derives the rects cannot leave a seam or an overhang.
    assert_eq!(first.rect.w + divider.rect.w + second.rect.w, 1000.0);
    assert_eq!(first.rect.w, 497.0);
    assert_eq!(divider.rect.w, DIVIDER_PX);
    assert_eq!(second.rect.x, 503.0);
    assert_eq!(first.rect.h, 500.0);
    // Focus is read off the layout, not off the tree, so a parked pane still
    // knows whether it owns the keyboard.
    assert!(first.focused);
    assert!(!second.focused);
    assert_eq!(divider.region_start, 0.0);
    assert_eq!(divider.region_len, 1000.0);
}

#[test]
fn an_area_narrower_than_the_gutter_yields_no_negative_rect() {
    let tree = row_split(
        "split-1",
        0.5,
        leaf("p1", &[ALPHA], ALPHA),
        leaf("p2", &[BETA], BETA),
    );
    // Both ends of the range, not one: a collapsed strip and an over-wide one
    // are the two ways a rect leaves its parent.
    for width in [0.0, 1.0, DIVIDER_PX - 1.0] {
        let rects = layout_rects(
            &tree,
            PaneRect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: 0.0,
            },
        );
        for rect in rects.panes.values() {
            assert!(rect.w >= 0.0, "width {width} produced {}", rect.w);
            assert!(rect.h >= 0.0, "width {width} produced {}", rect.h);
        }
        assert_eq!(rects.panes.len(), 2);
    }
}

#[test]
fn a_persisted_tree_is_refused_on_either_side_of_its_depth_bound() {
    // The accepted side, so the refusal below is about the bound and not about a
    // tree the restore cannot represent at all.
    let at_bound = record_payload(&nested_tree(LAYOUT_TREE_MAX_DEPTH - 1));
    let mut records = LayoutRecords::new();
    assert_eq!(ok(records.restore(&at_bound), "restore at the bound"), 1);
    let before = ok(records.snapshot(), "snapshot");

    let past_bound = record_payload(&nested_tree(LAYOUT_TREE_MAX_DEPTH));
    // Pinned to the variant. `LayoutRecordsError` has three arms, and a bare
    // `is_err()` here would be equally happy about a `NotARecord` payload that
    // never reached the bound at all — which is exactly how this test used to
    // pass.
    assert!(
        matches!(refused, Err(LayoutRecordsError::Malformed { .. })),
        "a tree one level past the bound was restored: {refused:?}"
    );
    // The whole payload is refused, so the record is not half-restored.
    assert_eq!(ok(records.snapshot(), "snapshot"), before);
}

#[test]
fn a_persisted_tree_that_contradicts_itself_is_refused() {
    let bad_ratio = PaneLayout {
        root: row_split(
            "split-1",
            4.0,
            leaf("p1", &[ALPHA], ALPHA),
            leaf("p2", &[BETA], BETA),
        ),
        focused_pane_id: "p1".to_owned(),
    };
    let unheld_selection = PaneLayout {
        root: leaf("p1", &[ALPHA], BETA),
        focused_pane_id: "p1".to_owned(),
    };
    let absent_focus = PaneLayout {
        root: leaf("p1", &[ALPHA], ALPHA),
        focused_pane_id: "p2".to_owned(),
    };
    let mut records = LayoutRecords::new();
    records.commit(
        FOLDER,
        PaneLayout {
            root: leaf("kept", &[ALPHA], ALPHA),
            focused_pane_id: "kept".to_owned(),
        },
    );
    let before = ok(records.snapshot(), "snapshot");

    for (name, layout) in [
        ("a ratio out of bounds", bad_ratio),
        ("a pane selecting a tab it does not hold", unheld_selection),
        ("a focused pane that is not in the tree", absent_focus),
    ] {
        let restored = records.restore(&payload);
        // Three different rules, one assertion each: `Malformed` is the arm
        // that means the payload WAS a record and its tree failed an invariant.
        // `NotARecord` here would mean the payload never parsed, and the test
        // would be green without any of the three rules having run.
        assert!(
            matches!(restored, Err(LayoutRecordsError::Malformed { .. })),
            "{name} was restored: {restored:?}"
        );
        assert_eq!(
            ok(records.snapshot(), "snapshot"),
            before,
            "{name} changed state"
        );
    }
}

#[test]
fn a_persisted_record_round_trips_through_the_key_value_store() {
    let mut ids = CountedIds::new("pane");
    let mut records = LayoutRecords::new();
    records.seed_if_absent(FOLDER, &session_ids(&[ALPHA, BETA]), &mut ids);
    let first = ok(records.snapshot(), "snapshot");
    // Seeding twice must not mint a second pane: a churning pane id is a deck
    // that remounts every terminal on every render.
    records.seed_if_absent(FOLDER, &session_ids(&[ALPHA, BETA]), &mut ids);
    assert_eq!(ok(records.snapshot(), "snapshot"), first);

    let store = MemoryKeyValueStore::new();
    ok(records.persist(&store), "persist");
    let mut restored = LayoutRecords::new();
    assert_eq!(ok(restored.restore_from(&store), "restore"), 1);
    assert_eq!(ok(restored.snapshot(), "snapshot"), first);
    let stored = ok(restored.stored(FOLDER), "a stored layout");
    assert_eq!(tabs_of(&stored.root, "pane-1"), session_ids(&[ALPHA, BETA]));
}

#[test]
fn a_resolved_layout_folds_the_live_set_into_what_was_stored() {
    let mut ids = CountedIds::new("pane");
    let mut records = LayoutRecords::new();
    records.seed_if_absent(FOLDER, &session_ids(&[ALPHA, BETA]), &mut ids);
    let resolved = records.resolve(FOLDER, &session_ids(&[ALPHA, GAMMA]), &mut ids);
    // BETA is gone and GAMMA was never placed, so both facts land in the tree a
    // host is about to paint.
    assert_eq!(
        tabs_of(&resolved.root, "pane-1"),
        session_ids(&[ALPHA, GAMMA])
    );
    assert!(find_leaf_of_tab(&resolved.root, BETA).is_none());
}
