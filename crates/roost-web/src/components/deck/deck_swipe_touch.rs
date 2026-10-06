//! The compact deck's touch reading: which touches start a tab swipe (one
//! finger, clear of the drawer's edge band), the axis lock that leaves
//! vertical travel to the terminal, the release velocity over the last
//! samples, and which touches end an armed drag without a release. Fed by
//! `terminal_deck_swipe`'s listener; target-independent. Ports the listener
//! half of `apps/web/src/components/deck/terminal-deck-swipe.ts`.

use super::deck_dom::DeckTouch;
use crate::motion::edge_swipe_drawer::{EDGE_PX, LockedAxis, lock_axis};

/// While tracking, samples older than this are dropped, ms.
const TRACK_WINDOW_MS: f64 = 120.0;
/// At release, only the samples this recent measure the fling, ms.
const RELEASE_WINDOW_MS: f64 = 80.0;

/// What one touch sample means for the swipe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchStep {
    /// Nothing for the deck.
    Ignored,
    /// The first horizontal move: arm a swipe at this travel, then track it.
    Armed {
        /// Travel from the touch start, px.
        delta_x: f64,
    },
    /// A later horizontal move.
    Tracked {
        /// Travel from the touch start, px.
        delta_x: f64,
    },
    /// The finger lifted after an armed drag.
    Released {
        /// Travel from the touch start to the last move, px.
        delta_x: f64,
        /// Release speed, px/ms.
        velocity: f64,
    },
    /// An armed drag ended without a release: a second finger landed, the
    /// browser cancelled the touch, or a new touch began while the drag was
    /// still armed (its release never reached the deck). The swipe springs back.
    Cancelled,
}

impl TouchStep {
    /// Whether the move belongs to the swipe, so the terminal must not also
    /// scroll it.
    pub fn consumes(self) -> bool {
        matches!(self, Self::Armed { .. } | Self::Tracked { .. })
    }
}

/// One finger's swipe tracking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SwipeTouchTracker {
    start: (f64, f64),
    last_x: f64,
    axis: Option<LockedAxis>,
    armed: bool,
    tracking: bool,
    samples: Vec<(f64, f64)>,
}

impl SwipeTouchTracker {
    /// Read one sample. `compact` is whether the host paints one pane.
    pub fn step(&mut self, touch: DeckTouch, compact: bool) -> TouchStep {
        match touch {
            DeckTouch::Start {
                x,
                y,
                touches,
                at_ms,
            } => {
                let interrupted = self.end_armed_drag();
                self.axis = None;
                self.tracking = compact && touches == 1 && x > EDGE_PX;
                if self.tracking {
                    self.start = (x, y);
                    self.last_x = x;
                    self.samples = vec![(x, at_ms)];
                }
                interrupted
            }
            DeckTouch::Move { x, y, at_ms } => self.track(x, y, at_ms),
            DeckTouch::End { at_ms } => self.release(at_ms),
            DeckTouch::Cancel => {
                self.tracking = false;
                self.end_armed_drag()
            }
        }
    }

    fn end_armed_drag(&mut self) -> TouchStep {
        if !std::mem::take(&mut self.armed) {
            return TouchStep::Ignored;
        }
        self.samples.clear();
        TouchStep::Cancelled
    }

    fn track(&mut self, x: f64, y: f64, at_ms: f64) -> TouchStep {
        if !self.tracking {
            return TouchStep::Ignored;
        }
        let delta_x = x - self.start.0;
        if self.axis.is_none() {
            match lock_axis(delta_x, y - self.start.1) {
                LockedAxis::None => return TouchStep::Ignored,
                locked => self.axis = Some(locked),
            }
        }
        if self.axis != Some(LockedAxis::X) {
            return TouchStep::Ignored;
        }
        self.samples.push((x, at_ms));
        while self.samples.len() > 2 && self.samples[0].1 < at_ms - TRACK_WINDOW_MS {
            self.samples.remove(0);
        }
        self.last_x = x;
        if self.armed {
            return TouchStep::Tracked { delta_x };
        }
        self.armed = true;
        TouchStep::Armed { delta_x }
    }

    fn release(&mut self, at_ms: f64) -> TouchStep {
        self.tracking = false;
        if !self.armed {
            return TouchStep::Ignored;
        }
        self.armed = false;
        while self.samples.len() > 1 && self.samples[0].1 < at_ms - RELEASE_WINDOW_MS {
            self.samples.remove(0);
        }
        let velocity = match (self.samples.first(), self.samples.last()) {
            (Some(first), Some(last)) if last.1 > first.1 => {
                (last.0 - first.0) / (last.1 - first.1)
            }
            _ => 0.0,
        };
        self.samples.clear();
        TouchStep::Released {
            delta_x: self.last_x - self.start.0,
            velocity,
        }
    }
}
