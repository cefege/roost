//! Pointer hit-test geometry: the VIEWPORT box is the origin, not the scroll
//! container, whose top sits the whole painted history above row 1. Ported
//! from the `viewportCellGeometry` cases of
//! `apps/web/tests/renderer/cellRenderer.geometry.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    CELL_PX, PAD_TOP, PANE_PX, ROW_PX, mount, row, seed_held_history, seed_held_history_to, vp_el,
};
use roost_protocol::cell::CellRow;
use roost_web_terminal::{ElementRect, RenderElement, cell_from_point};

fn history(count: u32, from: u32) -> Vec<CellRow> {
    (from..from + count)
        .map(|index| row(index, &format!("g{index}")))
        .collect()
}

fn grid(count: u32) -> Vec<CellRow> {
    (0..count)
        .map(|index| row(index, &format!("v{index}")))
        .collect()
}

#[test]
fn geometry_is_the_viewports_box_not_the_scroll_containers() {
    let (container, mut renderer) = mount();
    seed_held_history_to(&mut renderer, 80, grid(24), history(300, 500), 800);
    container.set_scroll_top_raw(120.0);
    let geometry = renderer
        .viewport_cell_geometry()
        .expect("a painted grid has geometry");
    let container_top = container.bounding_rect().top;
    let history_px = (500.0 + 300.0) * ROW_PX;
    assert_eq!(geometry.top, container_top + PAD_TOP - 120.0 + history_px);
    assert!(geometry.top - container_top > history_px - 120.0);
    assert_eq!(vp_el(&container).bounding_rect().top, geometry.top);
    assert_eq!(geometry.row_height, ROW_PX);
    assert_eq!(geometry.cell_width, CELL_PX);
    assert_eq!(geometry.left, vp_el(&container).bounding_rect().left);
    assert_eq!((geometry.cols, geometry.rows), (80, 24));
}

#[test]
fn a_click_resolves_to_the_row_the_user_aimed_at_over_painted_history() {
    let (container, mut renderer) = mount();
    seed_held_history_to(&mut renderer, 80, grid(24), history(300, 500), 800);
    container.set_scroll_top_raw(120.0);
    let geometry = renderer.viewport_cell_geometry().unwrap();
    let x = geometry.left + 2.0 * CELL_PX + CELL_PX / 2.0;
    let y = geometry.top + 4.0 * ROW_PX + ROW_PX / 2.0;
    assert_eq!(cell_from_point(geometry, x, y), (3, 5));
    assert_eq!(cell_from_point(geometry, geometry.left + PANE_PX, y).0, 80);
    assert_eq!(
        cell_from_point(geometry, x, geometry.top + 24.0 * ROW_PX + 4.0).1,
        24
    );
    assert_eq!(
        cell_from_point(geometry, x, container.bounding_rect().top).1,
        1
    );
}

#[test]
fn no_frame_and_an_unmeasurable_viewport_box_report_no_geometry() {
    let (container, mut renderer) = mount();
    assert_eq!(renderer.viewport_cell_geometry(), None);
    seed_held_history(&mut renderer, 80, grid(2), Vec::new());
    assert!(renderer.viewport_cell_geometry().is_some());
    vp_el(&container).set_bounding_rect_override(Some(ElementRect::default()));
    assert_eq!(renderer.viewport_cell_geometry(), None);
}
