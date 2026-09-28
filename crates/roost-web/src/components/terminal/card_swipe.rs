//! Swipe-to-close for one tab-grid terminal card (Chrome's
//! TabGridItemTouchHelperCallback): a horizontal drag slides the card and
//! fades it, a full-travel drag or a directional flick dismisses it, anything
//! else springs back, and a vertical drag belongs to the grid's scroll.
//! Target-independent; `terminal_card` feeds it touches. Ports the gesture of
//! `apps/web/src/components/terminal/TerminalCard.tsx`; the thresholds are
//! DECK's `deck_swipe`.

use crate::components::deck::deck_swipe::should_dismiss_card;

/// Travel before the gesture picks an axis.
const AXIS_SLOP_PX: f64 = 10.0;
/// Horizontal travel past which the trailing click is swallowed.
const SWIPED_PX: f64 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Axis {
    #[default]
    None,
    Horizontal,
    Vertical,
}

/// One card's gesture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CardSwipe {
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_ms: f64,
    velocity: f64,
    dx: f64,
    axis: Axis,
    swiped: bool,
}

impl CardSwipe {
    /// A finger went down.
    pub fn start(&mut self, x: f64, y: f64, now_ms: f64) {
        *self = Self {
            start_x: x,
            start_y: y,
            last_x: x,
            last_ms: now_ms,
            ..Self::default()
        };
    }

    /// The finger moved: the card's new offset, or `None` while the axis is
    /// undecided or vertical, which leaves the move to the grid's scroll.
    pub fn move_to(&mut self, x: f64, y: f64, now_ms: f64) -> Option<f64> {
        let dx = x - self.start_x;
        let dy = y - self.start_y;
        if self.axis == Axis::None {
            if dx.abs() < AXIS_SLOP_PX && dy.abs() < AXIS_SLOP_PX {
                return None;
            }
            self.axis = if dx.abs() > dy.abs() * 1.5 {
                Axis::Horizontal
            } else {
                Axis::Vertical
            };
        }
        if self.axis != Axis::Horizontal {
            return None;
        }
        self.dx = dx;
        if dx.abs() > SWIPED_PX {
            self.swiped = true;
        }
        let elapsed = now_ms - self.last_ms;
        if elapsed > 0.0 {
            self.velocity = (x - self.last_x) / elapsed;
        }
        self.last_x = x;
        self.last_ms = now_ms;
        Some(dx)
    }

    /// The finger lifted: true when the card is dismissed. A short drag that
    /// did not dismiss does not swallow the next tap.
    pub fn end(&mut self) -> bool {
        let dismissed = should_dismiss_card(self.dx, self.velocity);
        if dismissed {
            self.swiped = true;
        } else {
            self.dx = 0.0;
            self.swiped = false;
        }
        dismissed
    }

    /// The card's offset.
    pub fn dx(&self) -> f64 {
        self.dx
    }

    /// Swallow the click that trails a swipe: answers true once.
    pub fn take_swiped(&mut self) -> bool {
        std::mem::take(&mut self.swiped)
    }
}
