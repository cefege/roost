//! The clamp that decides whether a controller direction scrolls the pane or
//! leaves it, ported from `apps/web/tests/terminalReaderScroll.dom.test.ts`
//! against the native decision in `reader_scroll.rs`. `None` IS the "this box
//! cannot travel" answer the pad router reads before handing the direction to
//! focus navigation; a decision that answered a move at an edge would dead-end
//! the pad inside the terminal.

use roost_web_terminal::ScrollBoxGeometry;
use roost_web_terminal::reader_scroll::{PAD_SCROLL_STEP_PX, reader_scroll_target};

fn scroll_box(scroll_top: f64, scroll_height: f64, client_height: f64) -> ScrollBoxGeometry {
    ScrollBoxGeometry {
        scroll_top,
        scroll_height,
        client_height,
    }
}

#[test]
fn reports_no_travel_at_either_edge() {
    assert_eq!(
        reader_scroll_target(scroll_box(0.0, 1000.0, 400.0), -PAD_SCROLL_STEP_PX),
        None,
        "a box at its top cannot scroll up"
    );
    assert_eq!(
        reader_scroll_target(scroll_box(600.0, 1000.0, 400.0), PAD_SCROLL_STEP_PX),
        None,
        "a box at its bottom cannot scroll down"
    );
}

#[test]
fn clamps_an_overshoot_to_the_scroll_maximum() {
    assert_eq!(
        reader_scroll_target(scroll_box(550.0, 1000.0, 400.0), PAD_SCROLL_STEP_PX),
        Some(600.0)
    );
    assert_eq!(
        reader_scroll_target(scroll_box(50.0, 1000.0, 400.0), -PAD_SCROLL_STEP_PX),
        Some(0.0),
        "and an upward overshoot to the top"
    );
}

#[test]
fn moves_by_the_full_step_inside_the_range() {
    assert_eq!(
        reader_scroll_target(scroll_box(200.0, 1000.0, 400.0), -PAD_SCROLL_STEP_PX),
        Some(200.0 - PAD_SCROLL_STEP_PX)
    );
    assert_eq!(
        reader_scroll_target(scroll_box(200.0, 1000.0, 400.0), PAD_SCROLL_STEP_PX),
        Some(200.0 + PAD_SCROLL_STEP_PX)
    );
}

#[test]
fn reports_no_travel_when_the_box_has_no_scroll_range() {
    assert_eq!(
        reader_scroll_target(scroll_box(0.0, 400.0, 400.0), PAD_SCROLL_STEP_PX),
        None
    );
    assert_eq!(
        reader_scroll_target(scroll_box(10.0, 400.0, 400.0), -PAD_SCROLL_STEP_PX),
        None,
        "a box with no range never travels, even from a stale reported position"
    );
    assert_eq!(
        reader_scroll_target(scroll_box(0.0, 300.0, 400.0), PAD_SCROLL_STEP_PX),
        None,
        "content shorter than the box has no range either"
    );
}
