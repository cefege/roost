//! The predictive-echo painter's frame batching: a burst of paint requests
//! costs one overlay write per animation frame, the latest request wins, a
//! clear is a request of its own, and a cancelled frame writes nothing.
//!
//! Behaviour of v2's `apps/web/src/renderer/predictiveEchoPaint.ts`
//! (`PredictionPainter.request` / `cancel` / `flush`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::predictive_echo::{EchoPaint, PredictedCell};
use roost_web_terminal::echo_overlay::{PaintFlush, PredictionPainter};

fn paint(ch: &str, caret: u32) -> EchoPaint {
    EchoPaint {
        cells: vec![PredictedCell {
            row: 0,
            col: caret.saturating_sub(1),
            ch: ch.to_string(),
        }],
        flagged: false,
        caret_col: Some(caret),
    }
}

#[test]
fn a_burst_of_requests_inside_one_frame_paints_once_with_the_latest() {
    let mut painter = PredictionPainter::default();
    assert!(painter.request(Some(paint("a", 1))), "the first request asks for a frame");
    assert!(!painter.request(Some(paint("b", 2))), "one frame serves the burst");
    assert!(!painter.request(Some(paint("c", 3))));
    assert_eq!(painter.flush(), Some(PaintFlush::Paint(paint("c", 3))));
    assert_eq!(painter.flush(), None, "the frame's write is taken once");
    assert!(painter.request(Some(paint("d", 4))), "the next request asks again");
}

#[test]
fn a_clear_is_a_request_and_no_request_writes_nothing() {
    let mut painter = PredictionPainter::default();
    assert_eq!(painter.flush(), None);
    assert!(painter.request(Some(paint("a", 1))));
    painter.request(None);
    assert_eq!(
        painter.flush(),
        Some(PaintFlush::Clear),
        "the clear came last, so the overlay and caret are cleared"
    );
}

#[test]
fn a_cancelled_frame_writes_nothing_into_a_disposed_overlay() {
    let mut painter = PredictionPainter::default();
    painter.request(Some(paint("a", 1)));
    assert!(painter.cancel(), "an outstanding frame is reported for cancelling");
    assert_eq!(painter.flush(), None);
    assert!(!painter.cancel(), "nothing is outstanding any more");
}
