//! Pixels from ratios: the rect every pane paints at, and the divider handles
//! between them.
//!
//! Ported from the geometry half of `apps/web/src/store/paneLayout.ts`. The
//! RATIO IS THE TRUTH and the rects are derived, so a resize re-derives every
//! pane rather than rescaling the last frame's boxes -- a scaled box is a box
//! whose scroll position no longer describes the same rows.
//!
//! The ratio a rect is derived from is the one in the tree, clamped by
//! `reconcile` on the way in. The clamp here is the second half of that
//! defence: an area smaller than the gutter, or a ratio from a tree that never
//! went through `reconcile`, must still produce rects inside the parent.

use std::collections::BTreeMap;

use roost_protocol::layout::document::LayoutDirection;

use super::tree::{PaneLayout, PaneNode, all_leaves};

/// The gutter reserved between two split children for the drag divider, in px.
pub const DIVIDER_PX: f64 = 6.0;

/// A box in the deck's coordinate space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    /// Left edge, px.
    pub x: f64,
    /// Top edge, px.
    pub y: f64,
    /// Width, px.
    pub w: f64,
    /// Height, px.
    pub h: f64,
}

/// The drag handle for one divider.
#[derive(Debug, Clone, PartialEq)]
pub struct DividerRect {
    /// The handle's own box, gutter wide on the split's axis.
    pub rect: PaneRect,
    /// The split this handle drags.
    pub split_id: String,
    /// Which axis the handle sits on.
    pub direction: LayoutDirection,
    /// The split's current ratio, so a release that did not move is a no-op.
    pub ratio: f64,
    /// The px origin of the split's whole area along its axis.
    pub region_start: f64,
    /// The px length of that area, which a drag maps back onto a ratio.
    pub region_len: f64,
}

/// Everything a host needs to render one pane this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneView {
    /// The pane.
    pub pane_id: String,
    /// Where it paints.
    pub rect: PaneRect,
    /// Its whole tab list, for the strip.
    pub tab_ids: Vec<String>,
    /// The tab that paints inside the rect.
    pub selected_tab: String,
    /// Whether this pane owns the keyboard.
    pub focused: bool,
}

/// Every pane's rect and every divider, for one area.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutRects {
    /// One rect per pane, keyed by pane id.
    pub panes: BTreeMap<String, PaneRect>,
    /// The handles, in the order the walk reached them.
    pub dividers: Vec<DividerRect>,
}

/// Assign each pane a rect inside `area`, and collect the divider handles.
pub fn layout_rects(root: &PaneNode, area: PaneRect) -> LayoutRects {
    let mut panes: BTreeMap<String, PaneRect> = BTreeMap::new();
    let mut dividers: Vec<DividerRect> = Vec::new();
    walk(root, area, &mut panes, &mut dividers);
    LayoutRects { panes, dividers }
}

fn walk(
    node: &PaneNode,
    area: PaneRect,
    panes: &mut BTreeMap<String, PaneRect>,
    dividers: &mut Vec<DividerRect>,
) {
    let PaneNode::Split(split) = node else {
        if let PaneNode::Leaf(leaf) = node {
            panes.insert(leaf.pane_id.clone(), area);
        }
        return;
    };
    let gutter = DIVIDER_PX;
    if matches!(split.direction, LayoutDirection::Col) {
        let first = ((area.h - gutter) * split.ratio).max(0.0);
        let second = (area.h - gutter - first).max(0.0);
        walk(&split.a, PaneRect { h: first, ..area }, panes, dividers);
        dividers.push(DividerRect {
            rect: PaneRect {
                y: area.y + first,
                h: gutter,
                ..area
            },
            split_id: split.id.clone(),
            direction: split.direction.clone(),
            ratio: split.ratio,
            region_start: area.y,
            region_len: area.h,
        });
        walk(
            &split.b,
            PaneRect {
                y: area.y + first + gutter,
                h: second,
                ..area
            },
            panes,
            dividers,
        );
        return;
    }
    // Anything that is not `Col` divides left/right, including a direction a
    // newer peer wrote. `parse_layout_document_v1` refuses such a direction, so
    // reaching here means the tree was built in this process, and a row split
    // is the only shape that keeps the tree renderable.
    let first = ((area.w - gutter) * split.ratio).max(0.0);
    let second = (area.w - gutter - first).max(0.0);
    walk(&split.a, PaneRect { w: first, ..area }, panes, dividers);
    dividers.push(DividerRect {
        rect: PaneRect {
            x: area.x + first,
            w: gutter,
            ..area
        },
        split_id: split.id.clone(),
        direction: split.direction.clone(),
        ratio: split.ratio,
        region_start: area.x,
        region_len: area.w,
    });
    walk(
        &split.b,
        PaneRect {
            x: area.x + first + gutter,
            w: second,
            ..area
        },
        panes,
        dividers,
    );
}

/// The render list for one frame: a view per pane, in first-before-second order,
/// plus the divider handles. A host paints each pane's SELECTED tab at its rect
/// and keeps the pane's other tabs mounted but hidden.
pub fn layout_view(
    layout: &PaneLayout,
    width: f64,
    height: f64,
) -> (Vec<PaneView>, Vec<DividerRect>) {
    let area = PaneRect {
        x: 0.0,
        y: 0.0,
        w: width,
        h: height,
    };
    let rects = layout_rects(&layout.root, area);
    let mut panes: Vec<PaneView> = Vec::new();
    for leaf in all_leaves(&layout.root) {
        let Some(rect) = rects.panes.get(&leaf.pane_id) else {
            continue;
        };
        panes.push(PaneView {
            pane_id: leaf.pane_id.clone(),
            rect: *rect,
            tab_ids: leaf.tabs.clone(),
            selected_tab: leaf.selected_tab.clone(),
            focused: leaf.pane_id == layout.focused_pane_id,
        });
    }
    (panes, rects.dividers)
}
