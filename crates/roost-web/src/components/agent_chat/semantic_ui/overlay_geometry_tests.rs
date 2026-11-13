//! Consumer-visible overlay side, collision and movement invariants.
//! Covers measured content, clipped surfaces, narrow viewports and caret motion.
//! Shared menu margins and gaps remain the placement policy.

use super::*;

fn bounds(left: f64, top: f64, width: f64, height: f64) -> ClientBox {
    ClientBox {
        left,
        top,
        width,
        height,
    }
}

fn node_anchor(side: OverlaySide) -> OverlayAnchor {
    OverlayAnchor::Node {
        id: "target".into(),
        side,
    }
}

fn options_from(props: serde_json::Value) -> OverlayOptions {
    match props {
        serde_json::Value::Object(map) => OverlayOptions::parse(&map),
        _ => OverlayOptions::parse(&serde_json::Map::new()),
    }
}

#[test]
fn declared_side_wins_when_the_measured_panel_fits_even_in_a_short_gap() {
    let viewport = bounds(0.0, 0.0, 800.0, 600.0);
    let target = bounds(100.0, 100.0, 80.0, 20.0);
    let above = place_overlay(
        &node_anchor(OverlaySide::Above),
        target,
        viewport,
        200.0,
        60.0,
    );
    assert_eq!(above.top, 36.0);
    let below = place_overlay(
        &node_anchor(OverlaySide::Below),
        target,
        viewport,
        200.0,
        60.0,
    );
    assert_eq!(below.top, 124.0);
}

#[test]
fn collisions_flip_above_and_below_then_cap_content_to_the_room() {
    let viewport = bounds(0.0, 0.0, 320.0, 480.0);
    let top_target = bounds(20.0, 16.0, 40.0, 24.0);
    let down = place_overlay(
        &node_anchor(OverlaySide::Above),
        top_target,
        viewport,
        250.0,
        120.0,
    );
    assert_eq!(down.top, 44.0);
    let bottom_target = bounds(20.0, 440.0, 40.0, 24.0);
    let up = place_overlay(
        &node_anchor(OverlaySide::Below),
        bottom_target,
        viewport,
        250.0,
        120.0,
    );
    assert_eq!(up.top, 316.0);
    let tall = place_overlay(
        &node_anchor(OverlaySide::Below),
        bottom_target,
        viewport,
        250.0,
        900.0,
    );
    assert_eq!(tall.top, 8.0);
    assert_eq!(tall.max_height, 428.0);
}

#[test]
fn narrow_inset_viewports_keep_wide_content_inside_both_margins() {
    let phone = bounds(17.0, 52.0, 286.0, 400.0);
    for left in [20.0, 265.0] {
        let target = bounds(left, 350.0, 24.0, 24.0);
        let placed = place_overlay(
            &node_anchor(OverlaySide::Above),
            target,
            phone,
            800.0,
            200.0,
        );
        let width = 800.0_f64.min(placed.max_width);
        assert!(placed.left >= phone.left + EDGE_MARGIN_PX);
        assert!(placed.left + width <= phone.right() - EDGE_MARGIN_PX);
        assert!(placed.top >= phone.top + EDGE_MARGIN_PX);
        assert!(placed.top + 200.0_f64.min(placed.max_height) <= phone.bottom() - EDGE_MARGIN_PX);
    }
}

#[test]
fn caret_and_node_positions_follow_new_target_geometry_without_dock_assumptions() {
    let viewport = bounds(100.0, 40.0, 700.0, 600.0);
    let caret = OverlayAnchor::Caret {
        id: "editor".into(),
    };
    let before = place_overlay(
        &caret,
        bounds(160.0, 420.0, 0.0, 20.0),
        viewport,
        240.0,
        100.0,
    );
    let after = place_overlay(
        &caret,
        bounds(200.0, 260.0, 0.0, 20.0),
        viewport,
        240.0,
        100.0,
    );
    assert_eq!(after.left - before.left, 40.0);
    assert_eq!(after.top - before.top, -160.0);
    let resized = place_overlay(
        &caret,
        bounds(200.0, 260.0, 0.0, 20.0),
        bounds(100.0, 40.0, 300.0, 250.0),
        240.0,
        100.0,
    );
    assert!(resized.left + 240.0_f64.min(resized.max_width) <= 392.0);
    assert!(resized.top + 100.0_f64.min(resized.max_height) <= 282.0);
}

#[test]
fn top_center_and_bottom_use_the_visible_surface_including_keyboard_inset() {
    let viewport = bounds(20.0, 80.0, 300.0, 250.0);
    let top = place_overlay(&OverlayAnchor::Top, viewport, viewport, 200.0, 60.0);
    let center = place_overlay(&OverlayAnchor::Center, viewport, viewport, 200.0, 60.0);
    let bottom = place_overlay(&OverlayAnchor::Bottom, viewport, viewport, 200.0, 60.0);
    assert_eq!(top.top, 88.0);
    assert_eq!(center.top, 175.0);
    assert_eq!(bottom.top, 262.0);
    assert_eq!(center.left, 70.0);
}

#[test]
fn a_collapsed_surface_has_finite_zero_extent_instead_of_panicking() {
    let collapsed = bounds(20.0, 40.0, 0.0, 0.0);
    let placement = place_overlay(
        &node_anchor(OverlaySide::Above),
        collapsed,
        collapsed,
        200.0,
        100.0,
    );
    assert_eq!(placement.left, 20.0);
    assert_eq!(placement.top, 40.0);
    assert_eq!(placement.max_width, 0.0);
    assert_eq!(placement.max_height, 0.0);
}

#[test]
fn canonical_cells_percentages_minimums_offsets_and_asymmetric_margins_are_retained() {
    let viewport = bounds(100.0, 50.0, 800.0, 600.0);
    let options = options_from(serde_json::json!({
        "width": "25%", "minWidth": 30, "maxHeight": "40%",
        "offsetX": 3, "offsetY": 2,
        "margin": {"top": 1, "right": 2, "bottom": 3, "left": 4}
    }));
    let placed = place_configured_overlay(
        &OverlayAnchor::TopLeft,
        &options,
        viewport,
        viewport,
        (500.0, 900.0),
        (10.0, 20.0),
    );
    assert_eq!(placed.left, 170.0);
    assert_eq!(placed.top, 110.0);
    assert_eq!(placed.width, 300.0);
    assert_eq!(placed.max_height, 240.0);
}

#[test]
fn absolute_positions_use_surface_origin_but_percentages_use_remaining_margin_range() {
    let viewport = bounds(100.0, 50.0, 800.0, 600.0);
    let absolute = options_from(serde_json::json!({
        "width": 20, "maxHeight": 5, "row": 10, "col": 12, "margin": 1
    }));
    let placed = place_configured_overlay(
        &OverlayAnchor::BottomRight,
        &absolute,
        viewport,
        viewport,
        (500.0, 900.0),
        (10.0, 20.0),
    );
    assert_eq!(placed.left, 220.0);
    assert_eq!(placed.top, 250.0);
    assert_eq!(placed.width, 200.0);
    assert_eq!(placed.max_height, 100.0);
    let relative = options_from(serde_json::json!({
        "width": 20, "maxHeight": 5, "row": "100%", "col": "50%", "margin": 1
    }));
    let placed = place_configured_overlay(
        &OverlayAnchor::TopLeft,
        &relative,
        viewport,
        viewport,
        (500.0, 900.0),
        (10.0, 20.0),
    );
    assert_eq!(placed.left, 400.0);
    assert_eq!(placed.top, 530.0);
}

#[test]
fn every_terminal_corner_and_edge_anchor_keeps_its_horizontal_and_vertical_alignment() {
    let viewport = bounds(20.0, 80.0, 300.0, 250.0);
    for (anchor, left, top) in [
        (OverlayAnchor::TopLeft, 28.0, 88.0),
        (OverlayAnchor::TopRight, 112.0, 88.0),
        (OverlayAnchor::TopCenter, 70.0, 88.0),
        (OverlayAnchor::BottomLeft, 28.0, 262.0),
        (OverlayAnchor::BottomRight, 112.0, 262.0),
        (OverlayAnchor::BottomCenter, 70.0, 262.0),
        (OverlayAnchor::LeftCenter, 28.0, 175.0),
        (OverlayAnchor::RightCenter, 112.0, 175.0),
    ] {
        let placed = place_overlay(&anchor, viewport, viewport, 200.0, 60.0);
        assert_eq!((placed.left, placed.top), (left, top));
    }
}

#[test]
fn oversized_dimensions_and_offsets_clamp_inside_explicit_phone_margins() {
    let viewport = bounds(17.0, 52.0, 286.0, 300.0);
    let options = options_from(serde_json::json!({
        "width": 120, "minWidth": 100, "maxHeight": 100,
        "offsetX": 100, "offsetY": -100, "margin": {"left": 2, "right": 1, "top": 1, "bottom": 2}
    }));
    let placed = place_configured_overlay(
        &OverlayAnchor::BottomRight,
        &options,
        viewport,
        viewport,
        (900.0, 900.0),
        (8.0, 20.0),
    );
    assert_eq!(placed.left, 33.0);
    assert_eq!(placed.top, 72.0);
    assert_eq!(placed.width, 262.0);
    assert_eq!(placed.max_height, 240.0);
}

#[test]
fn explicit_zero_extents_keep_one_cell_until_the_surface_itself_collapses() {
    let viewport = bounds(0.0, 0.0, 300.0, 200.0);
    let options = options_from(serde_json::json!({"width": 0, "maxHeight": 0, "margin": 0}));
    let placed = place_configured_overlay(
        &OverlayAnchor::TopLeft,
        &options,
        viewport,
        viewport,
        (200.0, 100.0),
        (8.0, 20.0),
    );
    assert_eq!(placed.width, 8.0);
    assert_eq!(placed.max_height, 20.0);
    let collapsed = bounds(0.0, 0.0, 0.0, 0.0);
    let placed = place_configured_overlay(
        &OverlayAnchor::TopLeft,
        &options,
        collapsed,
        collapsed,
        (200.0, 100.0),
        (8.0, 20.0),
    );
    assert_eq!(placed.width, 0.0);
    assert_eq!(placed.max_height, 0.0);
}
