//! The mutations: every one is `Layout -> Layout`, pure, and returns the tree
//! unchanged rather than failing.
//!
//! Ported from the mutation half of `apps/web/src/store/paneLayout.ts`. The
//! reason a mutation returns the input instead of an error is the same one the
//! v2 original had: a tiling gesture arrives from a pointer that has already
//! moved on, and a layout that refuses the gesture is a layout that stays
//! consistent, where a half-applied one is a grid the user cannot interpret.
//!
//! A mutation is therefore a REPLACEMENT, never an edit in place, and a caller
//! that commits the result is the only writer of a stored arrangement.

use std::collections::BTreeSet;

use roost_protocol::layout::document::LayoutDirection;

use super::PaneIdSource;
use super::tree::{
    PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, collapse_empties, find_leaf,
    find_leaf_of_tab, fix_focus, normalize_split_ratios, remove_tab_everywhere, update_leaf,
};

/// Fold the live session set into a stored arrangement: clamp legacy ratios,
/// prune dead tabs, collapse only the panes that prune emptied, then append
/// never-placed live sessions to the focused pane.
///
/// The collapse set is built from the prune, never from "a pane that is empty
/// now": a pane can be empty because its session CLOSED (a layout decision) or
/// because a session stopped being live (a fact), and only the second is
/// allowed to delete a pane.
pub fn reconcile(layout: &PaneLayout, live_ids: &[String]) -> PaneLayout {
    let live: BTreeSet<&str> = live_ids.iter().map(String::as_str).collect();
    let mut emptied_by_prune: BTreeSet<String> = BTreeSet::new();
    let pruned = map_leaves(&normalize_split_ratios(&layout.root), |leaf| {
        let tabs: Vec<String> = leaf
            .tabs
            .iter()
            .filter(|tab| live.contains(tab.as_str()))
            .cloned()
            .collect();
        if tabs.len() == leaf.tabs.len() {
            return leaf.clone();
        }
        if !leaf.tabs.is_empty() && tabs.is_empty() {
            emptied_by_prune.insert(leaf.pane_id.clone());
        }
        let selected_tab = if live.contains(leaf.selected_tab.as_str()) {
            leaf.selected_tab.clone()
        } else {
            tabs.first().cloned().unwrap_or_default()
        };
        PaneLeaf {
            pane_id: leaf.pane_id.clone(),
            tabs,
            selected_tab,
        }
    });
    let mut root = collapse_empties(&pruned, &emptied_by_prune);
    let placed: BTreeSet<&str> = all_leaves(&root)
        .iter()
        .flat_map(|leaf| leaf.tabs.iter().map(String::as_str))
        .collect();
    let orphans: Vec<String> = live_ids
        .iter()
        .filter(|session| !placed.contains(session.as_str()))
        .cloned()
        .collect();
    let focused_pane_id = fix_focus(&root, &layout.focused_pane_id);
    if !orphans.is_empty() {
        let first_orphan = orphans[0].clone();
        root = update_leaf(&root, &focused_pane_id, move |leaf| {
            let mut tabs = leaf.tabs.clone();
            tabs.extend(orphans.iter().cloned());
            PaneNode::Leaf(PaneLeaf {
                pane_id: leaf.pane_id.clone(),
                tabs,
                selected_tab: if leaf.selected_tab.is_empty() {
                    first_orphan.clone()
                } else {
                    leaf.selected_tab.clone()
                },
            })
        });
    }
    PaneLayout {
        root,
        focused_pane_id,
    }
}

fn map_leaves(node: &PaneNode, edit: impl FnMut(&PaneLeaf) -> PaneLeaf) -> PaneNode {
    match node {
        PaneNode::Leaf(leaf) => PaneNode::Leaf(edit(leaf)),
        PaneNode::Split(split) => PaneNode::Split(PaneSplit {
            a: Box::new(map_leaves(&split.a, &mut edit)),
            b: Box::new(map_leaves(&split.b, &mut edit)),
            ..split.clone()
        }),
    }
}

/// Move `moving_tab` into a brand-new pane split off `target_pane_id`.
/// `insert_first` puts the new pane on the left or top side, and focus follows
/// it: the pane the gesture created is the pane the user is now looking at.
///
/// Returns the layout unchanged when the target pane does not exist, when the
/// direction is not one this build can render, or when the split would move a
/// pane's only tab out from under it.
pub fn split_leaf(
    layout: &PaneLayout,
    target_pane_id: &str,
    direction: LayoutDirection,
    moving_tab: &str,
    insert_first: bool,
    ids: &mut dyn PaneIdSource,
) -> PaneLayout {
    let portable = matches!(direction, LayoutDirection::Row | LayoutDirection::Col);
    if !portable {
        return layout.clone();
    }
    let Some(target) = find_leaf(&layout.root, target_pane_id) else {
        return layout.clone();
    };
    let source_pane_id =
        find_leaf_of_tab(&layout.root, moving_tab).map(|leaf| leaf.pane_id.clone());
    // Splitting a pane by moving its own only tab leaves nothing behind, so the
    // split would be a second pane holding the same session.
    if source_pane_id.as_deref() == Some(target_pane_id) && target.tabs.len() <= 1 {
        return layout.clone();
    }
    let new_pane_id = ids.mint_pane_id();
    let split_id = ids.mint_pane_id();
    let moved = PaneNode::Leaf(PaneLeaf {
        pane_id: new_pane_id.clone(),
        tabs: vec![moving_tab.to_owned()],
        selected_tab: moving_tab.to_owned(),
    });
    let root = collapse_empties(
        &update_leaf(
            &remove_tab_everywhere(&layout.root, moving_tab),
            target_pane_id,
            move |leaf| {
                let kept = PaneNode::Leaf(leaf.clone());
                let (first, second) = if insert_first {
                    (moved, kept)
                } else {
                    (kept, moved)
                };
                PaneNode::Split(PaneSplit {
                    id: split_id.clone(),
                    direction: direction.clone(),
                    ratio: 0.5,
                    a: Box::new(first),
                    b: Box::new(second),
                })
            },
        ),
        &source_pane_id.into_iter().collect(),
    );
    PaneLayout {
        root,
        focused_pane_id: new_pane_id,
    }
}

/// Move `tab` into `to_pane_id` at `index`, selecting it there. A move inside
/// one pane is a reorder of that pane's strip, which is why the index means
/// "position among the OTHER tabs" there and "absolute position" here.
pub fn move_tab(
    layout: &PaneLayout,
    tab: &str,
    to_pane_id: &str,
    index: Option<usize>,
) -> PaneLayout {
    let Some(target) = find_leaf(&layout.root, to_pane_id) else {
        return layout.clone();
    };
    let source_pane_id = find_leaf_of_tab(&layout.root, tab).map(|leaf| leaf.pane_id.clone());
    if source_pane_id.as_deref() == Some(to_pane_id) {
        return reorder_tab(layout, to_pane_id, move_within(&target.tabs, tab, index));
    }
    let mut destination = target.tabs.clone();
    let at = index.map_or(destination.len(), |wanted| wanted.min(destination.len()));
    destination.insert(at, tab.to_owned());
    let root = collapse_empties(
        &update_leaf(
            &remove_tab_everywhere(&layout.root, tab),
            to_pane_id,
            move |leaf| {
                PaneNode::Leaf(PaneLeaf {
                    pane_id: leaf.pane_id.clone(),
                    tabs: destination.clone(),
                    selected_tab: tab.to_owned(),
                })
            },
        ),
        &source_pane_id.into_iter().collect(),
    );
    PaneLayout {
        root,
        focused_pane_id: to_pane_id.to_owned(),
    }
}

fn move_within(tabs: &[String], tab: &str, index: Option<usize>) -> Vec<String> {
    let mut rest: Vec<String> = tabs.iter().filter(|held| *held != tab).cloned().collect();
    let at = index.map_or(rest.len(), |wanted| wanted.min(rest.len()));
    rest.insert(at, tab.to_owned());
    rest
}

/// Replace a pane's tab order. The pane's selection and every other pane are
/// untouched: a reorder moves strips, it does not change what is showing.
pub fn reorder_tab(layout: &PaneLayout, pane_id: &str, ordered_tabs: Vec<String>) -> PaneLayout {
    let root = update_leaf(&layout.root, pane_id, move |leaf| {
        PaneNode::Leaf(PaneLeaf {
            tabs: ordered_tabs.clone(),
            ..leaf.clone()
        })
    });
    PaneLayout {
        root,
        focused_pane_id: layout.focused_pane_id.clone(),
    }
}

/// Select a tab in the pane that holds it, and focus that pane.
pub fn select_tab(layout: &PaneLayout, tab: &str) -> PaneLayout {
    let Some(leaf) = find_leaf_of_tab(&layout.root, tab) else {
        return layout.clone();
    };
    let pane_id = leaf.pane_id.clone();
    let root = update_leaf(&layout.root, &pane_id, move |leaf| {
        PaneNode::Leaf(PaneLeaf {
            selected_tab: tab.to_owned(),
            ..leaf.clone()
        })
    });
    PaneLayout {
        root,
        focused_pane_id: pane_id,
    }
}

/// Point focus at a pane that exists.
pub fn focus_pane(layout: &PaneLayout, pane_id: &str) -> PaneLayout {
    if find_leaf(&layout.root, pane_id).is_none() {
        return layout.clone();
    }
    PaneLayout {
        root: layout.root.clone(),
        focused_pane_id: pane_id.to_owned(),
    }
}

/// Drop a closed tab, collapse only the pane this close emptied, and keep focus
/// on a pane that exists.
pub fn close_tab(layout: &PaneLayout, tab: &str) -> PaneLayout {
    let closing_pane_id = find_leaf_of_tab(&layout.root, tab).map(|leaf| leaf.pane_id.clone());
    let root = collapse_empties(
        &remove_tab_everywhere(&layout.root, tab),
        &closing_pane_id.into_iter().collect(),
    );
    PaneLayout {
        focused_pane_id: fix_focus(&root, &layout.focused_pane_id),
        root,
    }
}
