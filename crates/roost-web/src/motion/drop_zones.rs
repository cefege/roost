//! Drag-to-tile routing: whether a tab dropped on a pane SPLITS it on a side
//! (pointer in the outer band) or MERGES into it (pointer in the centre). Ports
//! `apps/web/src/lib/dropZones.ts`; read by the deck's drop routing and its
//! drop-zone overlay.
//!
//! The band is `max(80px, 25%)` of each dimension; the nearest in-band edge
//! wins and a tie favours the horizontal edges.

/// A rectangle in the deck's coordinate space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// Where a dragged tab would land inside one pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropZone {
    /// Merge into the pane.
    Center,
    /// Split, the new pane on the left.
    Left,
    /// Split, the new pane on the right.
    Right,
    /// Split, the new pane on top.
    Top,
    /// Split, the new pane below.
    Bottom,
    /// The home pane's body centre: show an overlay, but the drop is a reorder.
    Reorder,
}

impl DropZone {
    /// Tie-break order: horizontal edges first.
    const fn tie_rank(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Top => 2,
            Self::Bottom => 3,
            Self::Center => 4,
            Self::Reorder => 5,
        }
    }
}

/// The axis a split lays its two panes along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    /// Side by side.
    Row,
    /// Stacked.
    Col,
}

/// The split an edge zone maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitPlacement {
    /// The split's axis.
    pub dir: SplitDir,
    /// Whether the NEW pane takes the first slot.
    pub insert_first: bool,
}

/// A pane's id and its rectangle.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneBox {
    /// The layout's pane id.
    pub pane_id: String,
    /// Where it is drawn.
    pub rect: Rect,
}

/// The pane and zone a dragged tab targets.
#[derive(Debug, Clone, PartialEq)]
pub struct TileTarget {
    /// The pane under the pointer.
    pub pane_id: String,
    /// That pane's rectangle.
    pub rect: Rect,
    /// The zone inside it.
    pub zone: DropZone,
}

const EDGE_RATIO: f64 = 0.25;
const EDGE_MIN: f64 = 80.0;

/// The zone a pointer at (`x`, `y`) lands in, in the same space as `rect`.
pub fn drop_zone_for(rect: Rect, x: f64, y: f64) -> DropZone {
    let band_x = EDGE_MIN.max(rect.w * EDGE_RATIO);
    let band_y = EDGE_MIN.max(rect.h * EDGE_RATIO);
    let distances = [
        (DropZone::Left, x - rect.x, band_x),
        (DropZone::Right, rect.x + rect.w - x, band_x),
        (DropZone::Top, y - rect.y, band_y),
        (DropZone::Bottom, rect.y + rect.h - y, band_y),
    ];
    distances
        .into_iter()
        .filter(|(_, distance, band)| distance < band)
        .min_by(|left, right| {
            left.1
                .total_cmp(&right.1)
                .then(left.0.tie_rank().cmp(&right.0.tie_rank()))
        })
        .map_or(DropZone::Center, |(zone, _, _)| zone)
}

/// The split an edge zone maps to; `None` for a merge or a reorder.
pub fn zone_to_split(zone: DropZone) -> Option<SplitPlacement> {
    let (dir, insert_first) = match zone {
        DropZone::Left => (SplitDir::Row, true),
        DropZone::Right => (SplitDir::Row, false),
        DropZone::Top => (SplitDir::Col, true),
        DropZone::Bottom => (SplitDir::Col, false),
        DropZone::Center | DropZone::Reorder => return None,
    };
    Some(SplitPlacement { dir, insert_first })
}

/// The region a zone highlights: the half the new pane takes, or the whole
/// pane for a merge or a reorder.
pub fn zone_rect(rect: Rect, zone: DropZone) -> Rect {
    let half_w = rect.w / 2.0;
    let half_h = rect.h / 2.0;
    match zone {
        DropZone::Left => Rect { w: half_w, ..rect },
        DropZone::Right => Rect {
            x: rect.x + half_w,
            w: half_w,
            ..rect
        },
        DropZone::Top => Rect { h: half_h, ..rect },
        DropZone::Bottom => Rect {
            y: rect.y + half_h,
            h: half_h,
            ..rect
        },
        DropZone::Center | DropZone::Reorder => rect,
    }
}

/// Route a drag pointer (deck-local `x`, `y`) to a tile target.
///
/// `None` off every pane, and over the ORIGIN pane's strip band, where the strip
/// slide is the cue. `strip_h` is the tab-strip band at each pane's top.
pub fn tile_target_for(
    panes: &[PaneBox],
    origin_pane_id: &str,
    x: f64,
    y: f64,
    strip_h: f64,
) -> Option<TileTarget> {
    let pane = panes.iter().find(|pane| {
        x >= pane.rect.x
            && x < pane.rect.x + pane.rect.w
            && y >= pane.rect.y
            && y < pane.rect.y + pane.rect.h
    })?;
    let home = pane.pane_id == origin_pane_id;
    let target = |zone| TileTarget {
        pane_id: pane.pane_id.clone(),
        rect: pane.rect,
        zone,
    };
    if y < pane.rect.y + strip_h {
        return (!home).then(|| target(DropZone::Center));
    }
    match drop_zone_for(pane.rect, x, y) {
        DropZone::Center if home => Some(target(DropZone::Reorder)),
        zone => Some(target(zone)),
    }
}
