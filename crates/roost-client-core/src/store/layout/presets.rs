//! The "Arrange" presets: a folder's sessions turned into a tidy tree, and the
//! one non-rebuild preset that only rebalances the tree already there.
//!
//! Ported from `apps/web/src/store/paneLayoutPresets.ts`. `layout_rects`
//! renders any tree, so a preset needs no renderer change; `reconcile` then
//! folds in whatever live session the preset did not place.
//!
//! `Balance` is deliberately NOT a rebuild. It keeps the current panes, tab
//! groups and focus and recomputes only the ratios, because "make the panes
//! even" and "put each session in its own pane" are different requests and a
//! user who asked for the first does not get the second.

use roost_protocol::layout::document::LayoutDirection;

use super::PaneIdSource;
use super::tree::{PaneLayout, PaneLeaf, PaneNode, PaneSplit, all_leaves, default_layout};

/// A preset that rebuilds the tree, one session per pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetKind {
    /// Equal columns.
    Even,
    /// Equal full-width rows.
    Rows,
    /// A grid, alternating row and col down the tree.
    Tiled,
    /// One pane on the left, the rest stacked on the right.
    MainVertical,
}

impl PresetKind {
    /// The wire spelling, as `UiArrange.preset` carries it.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Even => "even",
            Self::Rows => "rows",
            Self::Tiled => "tiled",
            Self::MainVertical => "main-vertical",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(preset: &str) -> Option<Self> {
        match preset {
            "even" => Some(Self::Even),
            "rows" => Some(Self::Rows),
            "tiled" => Some(Self::Tiled),
            "main-vertical" => Some(Self::MainVertical),
            _ => None,
        }
    }
}

/// What an arrange request asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrangeKind {
    /// Rebuild the tree from the folder's live sessions.
    Preset(PresetKind),
    /// Keep the tree and equalize the pane areas.
    Balance,
}

impl ArrangeKind {
    /// Parse the wire spelling of an arrange command.
    pub fn parse(preset: &str) -> Option<Self> {
        match preset {
            "balance" => Some(Self::Balance),
            other => PresetKind::parse(other).map(Self::Preset),
        }
    }

    /// The wire spelling this request carries.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Preset(kind) => kind.as_wire(),
            Self::Balance => "balance",
        }
    }
}

/// The arrangement a rebuild preset produces.
///
/// `ids` is filtered of empties, and a folder with one session left gets the
/// plain single pane rather than a split with an empty half: a preset is a
/// request for panes, not for dividers.
pub fn preset_layout(
    kind: PresetKind,
    session_ids: &[String],
    ids: &mut dyn PaneIdSource,
) -> PaneLayout {
    let sessions: Vec<String> = session_ids
        .iter()
        .filter(|session| !session.is_empty())
        .cloned()
        .collect();
    if sessions.len() <= 1 {
        return default_layout(&sessions, ids);
    }
    let root = match kind {
        PresetKind::MainVertical => PaneNode::Split(PaneSplit {
            id: ids.mint_pane_id(),
            direction: LayoutDirection::Row,
            ratio: super::tree::normalize_pane_ratio(0.6),
            a: Box::new(leaf_of(&sessions[0], ids)),
            b: Box::new(build_balanced(
                &sessions[1..],
                LayoutDirection::Col,
                false,
                ids,
            )),
        }),
        PresetKind::Rows => build_balanced(&sessions, LayoutDirection::Col, false, ids),
        PresetKind::Even => build_balanced(&sessions, LayoutDirection::Row, false, ids),
        PresetKind::Tiled => build_balanced(&sessions, LayoutDirection::Row, true, ids),
    };
    let focused_pane_id = all_leaves(&root)
        .first()
        .map(|leaf| leaf.pane_id.clone())
        .unwrap_or_default();
    PaneLayout {
        root,
        focused_pane_id,
    }
}

/// Equalize the pane areas, keeping every pane, tab group and the focus.
pub fn balance_layout(layout: &PaneLayout) -> PaneLayout {
    PaneLayout {
        root: balance_node(&layout.root),
        focused_pane_id: layout.focused_pane_id.clone(),
    }
}

/// The single dispatcher an arrange command goes through.
pub fn arrange_layout(
    kind: ArrangeKind,
    layout: &PaneLayout,
    session_ids: &[String],
    ids: &mut dyn PaneIdSource,
) -> PaneLayout {
    match kind {
        ArrangeKind::Preset(preset) => preset_layout(preset, session_ids, ids),
        ArrangeKind::Balance => balance_layout(layout),
    }
}

/// A balanced binary tree over the sessions, each split taking the size share of
/// its first subtree so every leaf ends up near equal.
///
/// `alternate` flips the axis at each level, which is the whole difference
/// between equal columns and a grid.
fn build_balanced(
    sessions: &[String],
    direction: LayoutDirection,
    alternate: bool,
    ids: &mut dyn PaneIdSource,
) -> PaneNode {
    let Some(first) = sessions.first() else {
        // An empty subtree cannot be a leaf with no tab: the caller filters
        // empties, so reaching here means a caller that did not, and an empty
        // pane here would be a hole in the deck that reconcile cannot fill.
        return PaneNode::Leaf(PaneLeaf {
            pane_id: ids.mint_pane_id(),
            tabs: Vec::new(),
            selected_tab: String::new(),
        });
    };
    if sessions.len() == 1 {
        return leaf_of(first, ids);
    }
    let middle = sessions.len().div_ceil(2);
    let child_direction = if alternate {
        other_direction(direction.clone())
    } else {
        direction.clone()
    };
    let ratio = middle as f64 / sessions.len() as f64;
    PaneNode::Split(PaneSplit {
        id: ids.mint_pane_id(),
        direction: direction.clone(),
        // Clamped on the way in, not on the way out: an export re-parses through
        // the shared parser, which refuses a ratio outside the portable bounds,
        // so a preset that built one would produce an arrangement this client
        // could not share.
        ratio: super::tree::normalize_pane_ratio(ratio),
        a: Box::new(build_balanced(
            &sessions[..middle],
            child_direction.clone(),
            alternate,
            ids,
        )),
        b: Box::new(build_balanced(
            &sessions[middle..],
            child_direction,
            alternate,
            ids,
        )),
    })
}

fn balance_node(node: &PaneNode) -> PaneNode {
    let PaneNode::Split(split) = node else {
        return node.clone();
    };
    let first = balance_node(&split.a);
    let second = balance_node(&split.b);
    let first_leaves = all_leaves(&first).len();
    let second_leaves = all_leaves(&second).len();
    let total = first_leaves + second_leaves;
    // A skewed tree trades exact equal areas for reachable divider geometry and
    // a document the coordinator will admit: a 1%-99% split has no divider you
    // can grab and a ratio the portable bounds refuse.
    let ratio = if total == 0 {
        split.ratio
    } else {
        super::tree::normalize_pane_ratio(first_leaves as f64 / total as f64)
    };
    PaneNode::Split(PaneSplit {
        ratio,
        a: Box::new(first),
        b: Box::new(second),
        ..split.clone()
    })
}

fn leaf_of(session_id: &str, ids: &mut dyn PaneIdSource) -> PaneNode {
    PaneNode::Leaf(PaneLeaf {
        pane_id: ids.mint_pane_id(),
        tabs: vec![session_id.to_owned()],
        selected_tab: session_id.to_owned(),
    })
}

fn other_direction(direction: LayoutDirection) -> LayoutDirection {
    match direction {
        LayoutDirection::Col => LayoutDirection::Row,
        LayoutDirection::Row | LayoutDirection::Other(_) => LayoutDirection::Col,
    }
}
