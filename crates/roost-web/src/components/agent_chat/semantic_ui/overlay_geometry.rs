//! Measured native overlay placement in viewport coordinates.
//! Reuses the shared menu edge/gap policy and horizontal collision rules.
//! Browser measurement supplies the visible surface bounds and panel extent.

use crate::components::deck::deck_dom::ClientBox;
use crate::components::menu_placement::{EDGE_MARGIN_PX, Horizontal, TRIGGER_GAP_PX, place_menu};

use super::super::overlay::options::{Dimension, OverlayOptions};
use super::super::overlay::{OverlayAnchor, OverlaySide};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Placement {
    pub left: f64,
    pub top: f64,
    pub max_width: f64,
    pub max_height: f64,
    pub width: f64,
}

pub(super) fn place_overlay(
    anchor: &OverlayAnchor,
    target: ClientBox,
    bounds: ClientBox,
    width: f64,
    height: f64,
) -> Placement {
    let margin = EDGE_MARGIN_PX
        .min(bounds.width / 2.0)
        .min(bounds.height / 2.0);
    let available_width = (bounds.width - 2.0 * margin).max(0.0);
    let available_height = (bounds.height - 2.0 * margin).max(0.0);
    let width = width.min(available_width).max(0.0);
    let height = height.min(available_height).max(0.0);
    let mut placement = Placement {
        left: bounds.left + (bounds.width - width) / 2.0,
        top: bounds.top + (bounds.height - height) / 2.0,
        max_width: available_width,
        max_height: available_height,
        width,
    };
    match anchor {
        OverlayAnchor::Top
        | OverlayAnchor::TopCenter
        | OverlayAnchor::TopLeft
        | OverlayAnchor::TopRight => {
            placement.top = bounds.top + margin;
        }
        OverlayAnchor::Bottom
        | OverlayAnchor::BottomCenter
        | OverlayAnchor::BottomLeft
        | OverlayAnchor::BottomRight => {
            placement.top = bounds.bottom() - margin - height;
        }
        OverlayAnchor::Center | OverlayAnchor::LeftCenter | OverlayAnchor::RightCenter => {}
        OverlayAnchor::Node { .. } | OverlayAnchor::Caret { .. } => {
            let prefer_up = !matches!(
                anchor,
                OverlayAnchor::Node {
                    side: OverlaySide::Below,
                    ..
                }
            );
            let above = (target.top - bounds.top - margin - TRIGGER_GAP_PX).max(0.0);
            let below = (bounds.bottom() - target.bottom() - margin - TRIGGER_GAP_PX).max(0.0);
            let (preferred, other) = if prefer_up {
                (above, below)
            } else {
                (below, above)
            };
            let up = if preferred >= height || preferred >= other {
                prefer_up
            } else {
                !prefer_up
            };
            let menu = place_menu(target, bounds, bounds, up);
            placement.max_width = menu.max_width.min(available_width);
            placement.max_height = if up { above } else { below }.min(available_height);
            let width = width.min(placement.max_width);
            placement.left = match menu.horizontal {
                Horizontal::Left(left) => bounds.left + left,
                Horizontal::Right(right) => bounds.right() - right - width,
            };
            placement.top = if up {
                target.top - TRIGGER_GAP_PX - height.min(placement.max_height)
            } else {
                target.bottom() + TRIGGER_GAP_PX
            };
        }
    }
    if matches!(
        anchor,
        OverlayAnchor::TopLeft | OverlayAnchor::BottomLeft | OverlayAnchor::LeftCenter
    ) {
        placement.left = bounds.left + margin;
    } else if matches!(
        anchor,
        OverlayAnchor::TopRight | OverlayAnchor::BottomRight | OverlayAnchor::RightCenter
    ) {
        placement.left = bounds.right() - margin - width;
    }
    placement.width = width.min(placement.max_width);
    placement.left = placement.left.clamp(
        bounds.left + margin,
        bounds.right() - margin - width.min(placement.max_width),
    );
    placement.top = placement.top.clamp(
        bounds.top + margin,
        bounds.bottom() - margin - height.min(placement.max_height),
    );
    placement
}

pub(super) fn place_configured_overlay(
    anchor: &OverlayAnchor,
    options: &OverlayOptions,
    target: ClientBox,
    bounds: ClientBox,
    measured: (f64, f64),
    cell: (f64, f64),
) -> Placement {
    let safe = inset_bounds(
        bounds,
        options
            .margin
            .map(|margin| {
                [
                    margin[0] * cell.1,
                    margin[1] * cell.0,
                    margin[2] * cell.1,
                    margin[3] * cell.0,
                ]
            })
            .unwrap_or([EDGE_MARGIN_PX; 4]),
    );
    // The shared menu primitive owns its edge margin; expansion cancels it
    // after canonical per-side margins have already been applied.
    let menu_bounds = ClientBox {
        left: safe.left - EDGE_MARGIN_PX,
        top: safe.top - EDGE_MARGIN_PX,
        width: safe.width + 2.0 * EDGE_MARGIN_PX,
        height: safe.height + 2.0 * EDGE_MARGIN_PX,
    };
    let width = options
        .width
        .map(|width| width.resolve(bounds.width, cell.0))
        .unwrap_or(measured.0)
        .max(options.min_width.unwrap_or_default() * cell.0)
        .max(cell.0)
        .clamp(0.0, safe.width);
    let max_height = options
        .max_height
        .map(|height| height.resolve(bounds.height, cell.1))
        .unwrap_or(safe.height)
        .max(cell.1)
        .clamp(0.0, safe.height);
    let height = measured.1.min(max_height).max(0.0);
    let mut placement = place_overlay(anchor, target, menu_bounds, width, height);
    placement.max_height = placement.max_height.min(max_height);
    let height = height.min(placement.max_height);
    if let Some(col) = options.col {
        placement.left = resolve_position(
            col,
            bounds.left,
            safe.left,
            safe.width - placement.width,
            cell.0,
        );
    }
    if let Some(row) = options.row {
        placement.top = resolve_position(row, bounds.top, safe.top, safe.height - height, cell.1);
    }
    placement.left = (placement.left + options.offset_x * cell.0)
        .clamp(safe.left, safe.right() - placement.width);
    placement.top =
        (placement.top + options.offset_y * cell.1).clamp(safe.top, safe.bottom() - height);
    placement
}

fn resolve_position(value: Dimension, absolute: f64, inset: f64, range: f64, cell: f64) -> f64 {
    match value {
        Dimension::Cells(value) => absolute + value * cell,
        Dimension::Percent(fraction) => inset + range.max(0.0) * fraction,
    }
}

fn inset_bounds(bounds: ClientBox, margin: [f64; 4]) -> ClientBox {
    let left = margin[3].min(bounds.width).max(0.0);
    let top = margin[0].min(bounds.height).max(0.0);
    ClientBox {
        left: bounds.left + left,
        top: bounds.top + top,
        width: (bounds.width - left - margin[1]).max(0.0),
        height: (bounds.height - top - margin[2]).max(0.0),
    }
}

#[cfg(test)]
#[path = "overlay_geometry_tests.rs"]
mod tests;
