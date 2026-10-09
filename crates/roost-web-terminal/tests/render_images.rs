//! The image layer follows the painted frame through full and delta paints.
//!
//! These tests use the same fake element seam as the cell-row renderer tests,
//! including DOM order and the on-demand image-key contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::sync::Arc;

use render_support::{FakeEl, full_frame, mount, numbered_rows, row, vp_el};
use roost_protocol::cell::{CellGridFrame, ImagePlacement};
use roost_web_terminal::RenderElement;

fn placement(image_key: u64, row: u64, col: u16, z_index: i32) -> ImagePlacement {
    ImagePlacement {
        image_key,
        row,
        col,
        columns: 4,
        rows: 2,
        source_x: 0,
        source_y: 0,
        source_width: 2,
        source_height: 2,
        image_width: 2,
        image_height: 2,
        offset_x_px: 0,
        offset_y_px: 0,
        z_index,
    }
}

fn child_with_class(parent: &FakeEl, class_name: &str) -> FakeEl {
    parent
        .children()
        .into_iter()
        .find(|child| child.has_class(class_name))
        .expect("renderer layer exists")
}

fn layer_order(viewport: &FakeEl) -> Vec<String> {
    viewport.children().iter().map(FakeEl::class_name).collect()
}

fn assert_image_layers_follow_rows(viewport: &FakeEl) {
    let classes = layer_order(viewport);
    assert_eq!(
        classes.first().map(String::as_str),
        Some("cell-images-below")
    );
    let image_layer = classes
        .iter()
        .position(|class| class == "cell-images")
        .expect("above-image layer exists");
    let cursor = classes
        .iter()
        .position(|class| class == "cell-cursor")
        .expect("cursor exists");
    let ghosts = classes
        .iter()
        .position(|class| class == "cell-ghosts")
        .expect("ghost layer exists");
    let last_row = classes
        .iter()
        .rposition(|class| class.split_whitespace().any(|name| name == "cell-row"))
        .expect("viewport rows exist");

    assert!(last_row < image_layer);
    assert!(image_layer < cursor);
    assert!(cursor < ghosts);
}

fn image_frame(rows: u32, image: ImagePlacement) -> CellGridFrame {
    let mut frame = full_frame(80, numbered_rows(rows, 0), 0);
    frame.image_placements = Some(Arc::<[ImagePlacement]>::from(vec![image]));
    frame
}

#[test]
fn a_full_frame_paints_image_geometry_and_fetch_demand() {
    let (container, mut renderer) = mount();
    let frame = image_frame(4, placement(7, 3, 5, 0));

    assert!(renderer.apply(&frame));
    assert_eq!(renderer.wanted_image_keys(), vec![7]);
    renderer.install_image(7, b"png");
    assert!(renderer.wanted_image_keys().is_empty());

    let viewport = vp_el(&container);
    let images = child_with_class(&viewport, "cell-images");
    let painted = images.children();
    assert_eq!(painted.len(), 1);
    assert!(painted[0].has_class("cell-image"));
    assert_eq!(
        painted[0].style("top").as_deref(),
        Some("calc(3 * 1lh + 0 * 1lh)")
    );
    assert_eq!(
        painted[0].style("left").as_deref(),
        Some("calc(5 * 1ch + 0 * 1ch)")
    );
    assert_eq!(painted[0].style("width").as_deref(), Some("calc(4 * 1ch)"));
    assert_eq!(painted[0].style("height").as_deref(), Some("calc(2 * 1lh)"));
}

#[test]
fn negative_z_images_use_the_below_layer_and_empty_delta_removes_them() {
    let (container, mut renderer) = mount();
    let frame = image_frame(4, placement(11, 1, 2, -1));

    assert!(renderer.apply(&frame));
    renderer.install_image(11, b"png");
    let viewport = vp_el(&container);
    let below = child_with_class(&viewport, "cell-images-below");
    let above = child_with_class(&viewport, "cell-images");
    assert_eq!(below.children().len(), 1);
    assert!(above.children().is_empty());

    let mut delta = render_support::delta_frame(80, 4, Vec::new(), Vec::new(), 2);
    delta.image_placements = Some(Vec::<ImagePlacement>::new().into());
    assert!(renderer.apply(&delta));
    assert!(below.children().is_empty());
    assert!(above.children().is_empty());
    assert!(renderer.wanted_image_keys().is_empty());
}

#[test]
fn full_repaint_keeps_layers_ordered_around_rows_and_overlays() {
    let (container, mut renderer) = mount();
    let frame = image_frame(4, placement(19, 1, 3, 0));
    assert!(renderer.apply(&frame));
    renderer.install_image(19, b"png");

    let mut replacement = image_frame(4, placement(19, 1, 3, 0));
    replacement.grid_epoch = "test-grid:1".to_string();
    replacement.seq = 2;
    assert!(renderer.apply(&replacement));

    let viewport = vp_el(&container);
    assert_image_layers_follow_rows(&viewport);
    assert_eq!(
        child_with_class(&viewport, "cell-images").children().len(),
        1
    );
    assert_eq!(layer_order(&viewport).len(), 8);
}

#[test]
fn a_delta_inserted_viewport_row_stays_before_the_image_layer() {
    let (container, mut renderer) = mount();
    let initial = CellGridFrame {
        viewport_rows: vec![row(0, "same"), row(1, "b"), row(2, "c")],
        ..image_frame(3, placement(23, 1, 1, 0))
    };
    assert!(renderer.apply(&initial));
    renderer.install_image(23, b"png");

    let mut delta = render_support::delta_frame(
        80,
        3,
        vec![row(0, "same"), row(1, "n1"), row(2, "n2")],
        vec![row(0, "same")],
        2,
    );
    delta.scrollback_total = 1;
    assert!(renderer.apply(&delta));

    let viewport = vp_el(&container);
    assert_image_layers_follow_rows(&viewport);
    assert_eq!(
        viewport
            .children()
            .iter()
            .filter(|child| child.has_class("cell-row"))
            .count(),
        3
    );
}

#[test]
fn failed_image_keys_are_not_requested_again() {
    let (_, mut renderer) = mount();
    assert!(renderer.apply(&image_frame(4, placement(31, 0, 0, 0))));
    assert_eq!(renderer.wanted_image_keys(), vec![31]);

    renderer.image_failed(31);

    assert!(renderer.wanted_image_keys().is_empty());
}
