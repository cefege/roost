//! The runtime pane tree: construction, the pure walkers, and the ratio and
//! focus repairs every mutation runs before it commits.
//!
//! Ported from `apps/web/src/store/paneLayout.ts` minus its mutations, which
//! live in `tree_edit` beside the file that reads them. Ratios are clamped to
//! the shared `roost_protocol::layout` bounds, and a direction is the shared
//! `LayoutDirection` rather than a second row/col pair.
//!
//! NOTHING HERE CROSSES A WIRE. Runtime pane ids are browser-local, which is
//! exactly why the export in `document` mints fresh positional keys instead.

use std::collections::BTreeSet;

use roost_protocol::layout::document::LayoutDirection;
use roost_protocol::layout::{LAYOUT_RATIO_MAX, LAYOUT_RATIO_MIN};
use serde::{Deserialize, Serialize};

use super::PaneIdSource;

/// A pane, with the ordered tabs its strip shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLeaf {
    /// Stable for as long as this pane lives, so a divider and a frame address
    /// the same pane.
    pub pane_id: String,
    /// Ordered session ids. A tab is a session; the session may hold several.
    pub tabs: Vec<String>,
    /// A member of `tabs`, or empty exactly when `tabs` is.
    pub selected_tab: String,
}

/// A divider between two subtrees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneSplit {
    /// Stable, so a drag addresses one divider rather than a path.
    pub id: String,
    /// `Row` is left/right, `Col` is top/bottom.
    pub direction: LayoutDirection,
    /// The fraction of the primary axis given to `a`, within the shared bounds.
    pub ratio: f64,
    /// The first child. Depth-first order is first, then second, everywhere.
    pub a: Box<PaneNode>,
    /// The second child.
    pub b: Box<PaneNode>,
}

/// One node of the tree: a pane or a divider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneNode {
    /// A pane.
    Leaf(PaneLeaf),
    /// A divider.
    Split(PaneSplit),
}

/// A whole folder's arrangement: the tree, and which pane has the keyboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    /// The root node. Always a node, never a list: a layout with no panes is
    /// an empty root leaf, which is the one empty tree that can still be
    /// focused and reconciled.
    pub root: PaneNode,
    /// The pane that owns the keyboard. Always names a leaf in `root`.
    pub focused_pane_id: String,
}

/// One tab in the flat, phone-shaped order, with the pane that owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatTab {
    /// The session id.
    pub tab_id: String,
    /// The pane the tab sits in, for the persistent-selection path.
    pub pane_id: String,
}

/// A single pane holding every given session, the first one selected.
pub fn default_layout(session_ids: &[String], ids: &mut dyn PaneIdSource) -> PaneLayout {
    let pane_id = ids.mint_pane_id();
    PaneLayout {
        root: PaneNode::Leaf(PaneLeaf {
            pane_id: pane_id.clone(),
            tabs: session_ids.to_vec(),
            selected_tab: session_ids.first().cloned().unwrap_or_default(),
        }),
        focused_pane_id: pane_id,
    }
}

/// Every pane, in first-before-second order.
pub fn all_leaves(node: &PaneNode) -> Vec<&PaneLeaf> {
    match node {
        PaneNode::Leaf(leaf) => vec![leaf],
        PaneNode::Split(split) => {
            let mut leaves = all_leaves(&split.a);
            leaves.extend(all_leaves(&split.b));
            leaves
        }
    }
}

/// The pane with this id, if the tree holds one.
pub fn find_leaf<'leaf>(node: &'leaf PaneNode, pane_id: &str) -> Option<&'leaf PaneLeaf> {
    match node {
        PaneNode::Leaf(leaf) => (leaf.pane_id == pane_id).then_some(leaf),
        PaneNode::Split(split) => {
            find_leaf(&split.a, pane_id).or_else(|| find_leaf(&split.b, pane_id))
        }
    }
}

/// The pane holding this tab, if the tree holds it.
pub fn find_leaf_of_tab<'leaf>(node: &'leaf PaneNode, tab: &str) -> Option<&'leaf PaneLeaf> {
    match node {
        PaneNode::Leaf(leaf) => leaf.tabs.iter().any(|held| held == tab).then_some(leaf),
        PaneNode::Split(split) => {
            find_leaf_of_tab(&split.a, tab).or_else(|| find_leaf_of_tab(&split.b, tab))
        }
    }
}

/// The tree flattened into one ordered tab list, for a host that paints one
/// terminal. Pane topology stays desktop-only; the order is first-before-second
/// leaf order, then tab order within each leaf.
pub fn flat_tabs(root: &PaneNode) -> Vec<FlatTab> {
    all_leaves(root)
        .into_iter()
        .flat_map(|leaf| {
            leaf.tabs.iter().map(move |tab_id| FlatTab {
                tab_id: tab_id.clone(),
                pane_id: leaf.pane_id.clone(),
            })
        })
        .collect()
}

/// The pane a single-terminal host paints: the URL-active session's pane when
/// the layout holds it, else the first occupied pane, else the focused pane.
///
/// Read-only on purpose. A compact host navigates; it does not re-point desktop
/// focus or collapse a topology it is not rendering.
pub fn compact_leaf_for_layout<'layout>(
    layout: &'layout PaneLayout,
    active_session_id: Option<&str>,
) -> Option<&'layout PaneLeaf> {
    let active_leaf = active_session_id.and_then(|session| find_leaf_of_tab(&layout.root, session));
    active_leaf
        .or_else(|| {
            all_leaves(&layout.root)
                .into_iter()
                .find(|leaf| !leaf.tabs.is_empty())
        })
        .or_else(|| find_leaf(&layout.root, &layout.focused_pane_id))
        .or_else(|| all_leaves(&layout.root).into_iter().next())
}

/// Replace the leaf with `pane_id` by `edit`, which may return a split and so
/// grow the tree. A `pane_id` that names no leaf returns the tree unchanged.
///
/// `FnMut`, and re-borrowed into each child, because the walk visits BOTH
/// subtrees looking for the one pane and only the matching leaf calls `edit`. A
/// `FnOnce` would be consumed by the first subtree the walk reached, which is
/// whichever one happened to be walked first rather than the one the caller
/// named.
pub(crate) fn update_leaf(
    node: &PaneNode,
    pane_id: &str,
    mut edit: impl FnMut(&PaneLeaf) -> PaneNode,
) -> PaneNode {
    match node {
        PaneNode::Leaf(leaf) if leaf.pane_id == pane_id => edit(leaf),
        PaneNode::Leaf(_) => node.clone(),
        PaneNode::Split(split) => PaneNode::Split(PaneSplit {
            a: Box::new(update_leaf(&split.a, pane_id, &mut edit)),
            b: Box::new(update_leaf(&split.b, pane_id, &mut edit)),
            ..split.clone()
        }),
    }
}

/// Set the ratio of the split with `split_id`, clamped to the shared bounds.
pub fn set_ratio(node: &PaneNode, split_id: &str, ratio: f64) -> PaneNode {
    match node {
        PaneNode::Leaf(_) => node.clone(),
        PaneNode::Split(split) => PaneNode::Split(PaneSplit {
            ratio: if split.id == split_id {
                normalize_pane_ratio(ratio)
            } else {
                split.ratio
            },
            a: Box::new(set_ratio(&split.a, split_id, ratio)),
            b: Box::new(set_ratio(&split.b, split_id, ratio)),
            ..split.clone()
        }),
    }
}

/// Drop `tab` from a pane, moving its selection to the tab that took its place.
pub(crate) fn remove_tab_from_leaf(leaf: &PaneLeaf, tab: &str) -> PaneLeaf {
    let Some(index) = leaf.tabs.iter().position(|held| held == tab) else {
        return leaf.clone();
    };
    let tabs: Vec<String> = leaf
        .tabs
        .iter()
        .filter(|held| *held != tab)
        .cloned()
        .collect();
    let selected_tab = if leaf.selected_tab == tab {
        tabs.get(index)
            .or_else(|| tabs.get(index.saturating_sub(1)))
            .or_else(|| tabs.first())
            .cloned()
            .unwrap_or_default()
    } else {
        leaf.selected_tab.clone()
    };
    PaneLeaf {
        pane_id: leaf.pane_id.clone(),
        tabs,
        selected_tab,
    }
}

/// Drop `tab` from whichever pane holds it, without collapsing what it empties.
pub(crate) fn remove_tab_everywhere(node: &PaneNode, tab: &str) -> PaneNode {
    match node {
        PaneNode::Leaf(leaf) => PaneNode::Leaf(remove_tab_from_leaf(leaf, tab)),
        PaneNode::Split(split) => PaneNode::Split(PaneSplit {
            a: Box::new(remove_tab_everywhere(&split.a, tab)),
            b: Box::new(remove_tab_everywhere(&split.b, tab)),
            ..split.clone()
        }),
    }
}

/// Collapse only the empty panes the caller nominates, bottom-up: a split that
/// loses one child is replaced by that child's surviving sibling.
///
/// Limited to nominated panes on purpose. A pane emptied by a session that
/// CLOSED is a layout decision, and one emptied by a reconcile prune is not; a
/// collapse that cannot tell them apart deletes panes the user is looking at.
/// An empty root leaf has no parent to collapse into, so it survives.
pub fn collapse_empties(node: &PaneNode, collapsible_pane_ids: &BTreeSet<String>) -> PaneNode {
    match node {
        PaneNode::Leaf(_) => node.clone(),
        PaneNode::Split(split) => {
            let first = collapse_empties(&split.a, collapsible_pane_ids);
            let second = collapse_empties(&split.b, collapsible_pane_ids);
            if is_collapsible_empty(&first, collapsible_pane_ids) {
                return second;
            }
            if is_collapsible_empty(&second, collapsible_pane_ids) {
                return first;
            }
            PaneNode::Split(PaneSplit {
                a: Box::new(first),
                b: Box::new(second),
                ..split.clone()
            })
        }
    }
}

fn is_collapsible_empty(node: &PaneNode, collapsible_pane_ids: &BTreeSet<String>) -> bool {
    match node {
        PaneNode::Leaf(leaf) => {
            leaf.tabs.is_empty() && collapsible_pane_ids.contains(&leaf.pane_id)
        }
        PaneNode::Split(_) => false,
    }
}

/// Clamp a ratio into the shared bounds, treating every non-finite input.
///
/// FINITENESS FIRST, THEN BOTH ENDS. A NaN compares false against every bound,
/// so a range check alone would pass it through and hand the renderer a
/// geometry with no answer. The infinities are named before the clamp for the
/// same reason: v2 does it in JavaScript, where `Math.max` propagates a NaN,
/// and Rust's `f64::max` does not -- so the guard has to be here rather than
/// inherited from the clamp.
pub(crate) fn normalize_pane_ratio(ratio: f64) -> f64 {
    if ratio.is_nan() {
        return (LAYOUT_RATIO_MIN + LAYOUT_RATIO_MAX) / 2.0;
    }
    if ratio == f64::INFINITY {
        return LAYOUT_RATIO_MAX;
    }
    if ratio == f64::NEG_INFINITY {
        return LAYOUT_RATIO_MIN;
    }
    ratio.max(LAYOUT_RATIO_MIN).min(LAYOUT_RATIO_MAX)
}

/// Clamp every ratio in a stored tree, so a document written by an older build
/// cannot paint a divider outside the portable bounds.
pub(crate) fn normalize_split_ratios(node: &PaneNode) -> PaneNode {
    match node {
        PaneNode::Leaf(_) => node.clone(),
        PaneNode::Split(split) => PaneNode::Split(PaneSplit {
            ratio: normalize_pane_ratio(split.ratio),
            a: Box::new(normalize_split_ratios(&split.a)),
            b: Box::new(normalize_split_ratios(&split.b)),
            ..split.clone()
        }),
    }
}

/// Point focus at a pane that exists, falling back to the first one.
pub fn fix_focus(root: &PaneNode, preferred: &str) -> String {
    if find_leaf(root, preferred).is_some() {
        return preferred.to_owned();
    }
    first_leaf(root).pane_id.clone()
}

fn first_leaf(node: &PaneNode) -> &PaneLeaf {
    match node {
        PaneNode::Leaf(leaf) => leaf,
        PaneNode::Split(split) => first_leaf(&split.a),
    }
}
