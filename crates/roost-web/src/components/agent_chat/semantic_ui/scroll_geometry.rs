//! Pixel arithmetic behind native reveal and scroll requests.
//! `scroll_effects` reads the painted rectangles; these pure functions turn
//! them into the shift one request asks for, so placement is tested without a
//! DOM. Positive always means forward: down for the block axis, right inline.

use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiRevealAt, OmpUiScrollBy};

/// One axis of a viewport-space rectangle, compared by its edges so a move
/// never mixes a start with an end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Span {
    pub start: f64,
    pub end: f64,
}

/// How far the content moves so `node` lands where a reveal asks inside
/// `viewport`. Positive scrolls forward (down or right).
pub(super) fn reveal_shift(placement: OmpUiRevealAt, node: Span, viewport: Span) -> f64 {
    match placement {
        OmpUiRevealAt::Start => node.start - viewport.start,
        OmpUiRevealAt::End => node.end - viewport.end,
        OmpUiRevealAt::Nearest => {
            if node.start < viewport.start {
                node.start - viewport.start
            } else if node.end > viewport.end {
                node.end - viewport.end
            } else {
                0.0
            }
        }
    }
}

/// The inline axis follows the block placement only as far as `nearest`.
pub(super) fn inline_shift(node: Span, viewport: Span) -> f64 {
    reveal_shift(OmpUiRevealAt::Nearest, node, viewport)
}

/// A page is the viewport less a line, never less than the line itself.
pub(super) fn page_step(viewport: f64, line: f64) -> f64 {
    (viewport - line).max(line)
}

/// The signed block shift one press asks for; `start` and `end` are
/// absolute positions and carry no delta.
pub(super) fn direction_shift(by: OmpUiScrollBy, line: f64, page: f64) -> f64 {
    match by {
        OmpUiScrollBy::LineUp => -line,
        OmpUiScrollBy::LineDown => line,
        OmpUiScrollBy::PageUp => -page,
        OmpUiScrollBy::PageDown => page,
        OmpUiScrollBy::Start | OmpUiScrollBy::End => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roost_protocol::wire::agent_chat::semantic_ui::OmpUiRevealAt::{End, Nearest, Start};

    fn span(start: f64, end: f64) -> Span {
        Span { start, end }
    }

    #[test]
    fn a_start_reveal_aligns_the_node_top_however_far_away_it_is() {
        assert_eq!(
            reveal_shift(Start, span(900.0, 940.0), span(100.0, 600.0)),
            800.0
        );
        assert_eq!(
            reveal_shift(Start, span(20.0, 60.0), span(100.0, 600.0)),
            -80.0
        );
        assert_eq!(
            reveal_shift(Start, span(100.0, 140.0), span(100.0, 600.0)),
            0.0
        );
    }

    #[test]
    fn an_end_reveal_aligns_the_node_bottom() {
        assert_eq!(
            reveal_shift(End, span(900.0, 940.0), span(100.0, 600.0)),
            340.0
        );
        assert_eq!(
            reveal_shift(End, span(20.0, 60.0), span(100.0, 600.0)),
            -540.0
        );
        assert_eq!(
            reveal_shift(End, span(560.0, 600.0), span(100.0, 600.0)),
            0.0
        );
    }

    #[test]
    fn a_nearest_reveal_moves_only_an_outside_node_to_the_nearer_edge() {
        assert_eq!(
            reveal_shift(Nearest, span(120.0, 160.0), span(100.0, 600.0)),
            0.0
        );
        assert_eq!(
            reveal_shift(Nearest, span(40.0, 80.0), span(100.0, 600.0)),
            -60.0
        );
        assert_eq!(
            reveal_shift(Nearest, span(580.0, 640.0), span(100.0, 600.0)),
            40.0
        );
        assert_eq!(
            reveal_shift(Nearest, span(80.0, 640.0), span(100.0, 600.0)),
            -20.0
        );
    }

    #[test]
    fn the_inline_axis_follows_a_block_placement_only_as_far_as_nearest() {
        assert_eq!(inline_shift(span(200.0, 260.0), span(100.0, 400.0)), 0.0);
        assert_eq!(inline_shift(span(20.0, 60.0), span(100.0, 400.0)), -80.0);
        assert_eq!(inline_shift(span(380.0, 460.0), span(100.0, 400.0)), 60.0);
    }

    #[test]
    fn a_page_is_a_viewport_less_a_line_and_never_less_than_a_line() {
        assert_eq!(page_step(600.0, 24.0), 576.0);
        assert_eq!(page_step(24.0, 24.0), 24.0);
        assert_eq!(page_step(10.0, 24.0), 24.0);
    }

    #[test]
    fn pressing_a_direction_moves_forward_for_down_and_back_for_up() {
        assert_eq!(direction_shift(OmpUiScrollBy::LineDown, 24.0, 576.0), 24.0);
        assert_eq!(direction_shift(OmpUiScrollBy::LineUp, 24.0, 576.0), -24.0);
        assert_eq!(direction_shift(OmpUiScrollBy::PageDown, 24.0, 576.0), 576.0);
        assert_eq!(direction_shift(OmpUiScrollBy::PageUp, 24.0, 576.0), -576.0);
        assert_eq!(direction_shift(OmpUiScrollBy::Start, 24.0, 576.0), 0.0);
        assert_eq!(direction_shift(OmpUiScrollBy::End, 24.0, 576.0), 0.0);
    }
}
