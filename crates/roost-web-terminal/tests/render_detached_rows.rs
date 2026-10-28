//! Sparse frame delivery must not trust detached viewport row handles.

#[path = "render_support/mod.rs"]
mod render_support;

use render_support::{FakeEl, delta_frame, full_frame, mount, row, vp_el, vp_rows};
use roost_protocol::cell::CellGridFrame;

fn child(viewport: &FakeEl, index: usize) -> FakeEl {
    viewport
        .children()
        .into_iter()
        .filter(|child| child.has_class("cell-row"))
        .nth(index)
        .expect("viewport row exists")
}

#[test]
fn detached_viewport_rows_refuse_sparse_deltas_until_a_full_reconciles_them() {
    for scrolling in [false, true] {
        let (container, mut renderer) = mount();
        let viewport = vp_el(&container);
        assert!(renderer.apply_full_frame(&full_frame(80, rows(&["A", "B", "C"]), 0)));
        let detached_parent = FakeEl::new("detached-parent");
        child(&viewport, 0).detach();
        let stale_watermark = renderer.reconciled_epoch_seq();
        let delta = if scrolling {
            CellGridFrame {
                scrollback_total: 1,
                ..delta_frame(80, 3, rows(&["B", "C", "D"]), vec![row(0, "A")], 2)
            }
        } else {
            delta_frame(80, 3, vec![row(1, "B-changed")], Vec::new(), 2)
        };
        let moved_row = child(&viewport, 0);
        detached_parent.splice_child(0, &moved_row);
        assert!(
            !renderer.apply_delta_frames(&[delta]),
            "detached viewport rows must refuse sparse deltas (scrolling={scrolling})"
        );
        assert_eq!(
            renderer.reconciled_epoch_seq(),
            stale_watermark,
            "a refused sparse delta must not advance the reconciled watermark"
        );
        assert!(renderer.apply_full_frame(&full_frame(
            80,
            if scrolling {
                rows(&["B", "C", "D"])
            } else {
                rows(&["A", "B", "C"])
            },
            if scrolling { 1 } else { 0 },
        )));
        assert_eq!(
            vp_rows(&container)
                .iter()
                .map(FakeEl::text_content)
                .collect::<Vec<_>>(),
            if scrolling {
                vec!["B".to_string(), "C".to_string(), "D".to_string()]
            } else {
                vec!["A".to_string(), "B".to_string(), "C".to_string()]
            },
            "a canonical full restores every viewport row"
        );
    }
}

fn rows(texts: &[&str]) -> Vec<roost_protocol::cell::CellRow> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| row(index as u32, text))
        .collect()
}
