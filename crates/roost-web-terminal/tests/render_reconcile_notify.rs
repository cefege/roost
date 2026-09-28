//! Reconciliation notification and the cursor blink attribute: the pane's
//! first-reconcile and per-reconcile hooks fire only after a COMPLETED DOM
//! reconcile, and the cursor defaults to solid. Ported from the notification
//! cases of `apps/web/tests/renderer/cellRenderer.reconcile.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::cell::Cell;
use std::rc::Rc;

use render_support::{FakeEl, delta_frame, full_frame, mount, row, vp_el};
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::presentation::{LiveInteractionResult, RendererEpochSeq};
use roost_web_terminal::{CellGridRenderer, RenderElement};

fn counter() -> (Rc<Cell<u32>>, Rc<Cell<u32>>) {
    let count = Rc::new(Cell::new(0));
    (count.clone(), count)
}

#[test]
fn reconcile_hooks_fire_only_after_a_completed_dom_reconciliation() {
    let container = FakeEl::container();
    let (first, first_hook) = counter();
    let (each, each_hook) = counter();
    let on_first: Box<dyn FnOnce()> = Box::new(move || first_hook.set(first_hook.get() + 1));
    let on_each: Box<dyn Fn()> = Box::new(move || each_hook.set(each_hook.get() + 1));
    let mut renderer =
        CellGridRenderer::with_callbacks(&container, Some(on_first), Some(on_each), None).unwrap();

    let rejected = CellGridFrame {
        rows: 2,
        ..full_frame(80, vec![row(0, "rejected")], 0)
    };
    assert!(!renderer.apply_full_frame(&rejected));
    assert_eq!((first.get(), each.get()), (0, 0));

    renderer.set_selection_hold(true);
    assert!(renderer.apply_full_frame(&full_frame(80, vec![row(0, "held")], 0)));
    assert_eq!(
        renderer.reconciled_epoch_seq(),
        RendererEpochSeq {
            grid_epoch: None,
            seq: None
        }
    );
    assert_eq!((first.get(), each.get()), (0, 0));

    let resumed = LiveInteractionResult {
        reconciled: true,
        anchor_changed: true,
    };
    assert_eq!(renderer.prepare_live_interaction(), resumed);
    let reconciled = RendererEpochSeq {
        grid_epoch: Some("test-grid:0".to_string()),
        seq: Some(1),
    };
    assert_eq!(renderer.reconciled_epoch_seq(), reconciled);
    assert_eq!((first.get(), each.get()), (1, 1));

    assert!(renderer.apply_delta_frames(&[delta_frame(
        80,
        1,
        vec![row(0, "delta")],
        Vec::new(),
        2
    )]));
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "later-full")], 0)
    }));
    assert_eq!((first.get(), each.get()), (1, 3));
}

#[test]
fn the_cursor_defaults_to_solid_and_only_the_explicit_blink_attribute_changes() {
    let (container, mut renderer) = mount();
    assert!(renderer.apply_full_frame(&full_frame(80, vec![row(0, "visible")], 0)));
    let cursor = vp_el(&container)
        .children()
        .into_iter()
        .find(|child| child.class_name() == "cell-cursor")
        .unwrap();
    let blink = |cursor: &FakeEl| cursor.attribute("data-blink");
    assert_eq!(blink(&cursor).as_deref(), Some("false"));
    renderer.set_cursor_blink_enabled(false);
    assert_eq!(blink(&cursor).as_deref(), Some("false"));
    renderer.set_cursor_blink_enabled(true);
    assert_eq!(blink(&cursor).as_deref(), Some("true"));
    renderer.set_cursor_blink_enabled(true);
    assert_eq!(blink(&cursor).as_deref(), Some("true"));
    assert_eq!(cursor.attribute("data-visible").as_deref(), Some("true"));
    assert_eq!(cursor.style("display").as_deref(), Some("block"));
    renderer.set_cursor_blink_enabled(false);
    assert_eq!(blink(&cursor).as_deref(), Some("false"));
    assert_eq!(cursor.attribute("data-visible").as_deref(), Some("true"));
    assert_eq!(cursor.style("display").as_deref(), Some("block"));
}
