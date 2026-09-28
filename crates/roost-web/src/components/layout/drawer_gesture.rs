//! The compact drawer's swipe gesture as a state machine: a left-edge touch
//! arms an open, any touch on an open drawer arms a close, the axis locks, the
//! drawer follows the finger, and the release commits by distance or flick.
//! Ports the handlers of `apps/web/src/components/layout/MobileSidebarDrawer.tsx`;
//! driven by `mobile_sidebar_drawer`'s window touch listeners, with the geometry
//! from `motion::edge_swipe_drawer`.

use crate::motion::drawer_drag::DrawerSettle;
use crate::motion::edge_swipe_drawer::{
    EDGE_PX, LockedAxis, close_offset_px, lock_axis, open_offset_px, should_close, should_open,
};

/// How far back the velocity window reaches while the finger moves.
const MOVE_WINDOW_MS: f64 = 120.0;
/// How far back the release velocity is measured.
const RELEASE_WINDOW_MS: f64 = 80.0;

/// One touch sample.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    x: f64,
    at_ms: f64,
}

/// What a move asks of the host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DrawerMove {
    /// Not a drawer gesture (yet, or any more).
    Ignore,
    /// Consume the event and put the drawer at this `translateX`.
    Drag {
        /// Pixels.
        offset_px: f64,
    },
}

/// The gesture in progress.
#[derive(Debug, Default)]
pub struct DrawerGesture {
    mode: Option<DrawerSettle>,
    start_x: f64,
    start_y: f64,
    latest_x: f64,
    axis_locked: bool,
    armed: bool,
    candidate: bool,
    samples: Vec<Sample>,
}

impl DrawerGesture {
    /// A touch began. `excluded_target` is a touch on the deck's tab bar or a
    /// swipeable row, which keep their own horizontal gestures.
    pub fn touch_start(
        &mut self,
        touches: u32,
        x: f64,
        y: f64,
        drawer_open: bool,
        excluded_target: bool,
        now_ms: f64,
    ) {
        self.mode = None;
        self.candidate = false;
        if touches != 1 {
            return;
        }
        if drawer_open {
            if excluded_target {
                return;
            }
            self.mode = Some(DrawerSettle::Close);
        } else if x <= EDGE_PX {
            self.mode = Some(DrawerSettle::Open);
        } else {
            return;
        }
        self.candidate = true;
        self.start_x = x;
        self.start_y = y;
        self.latest_x = x;
        self.axis_locked = false;
        self.armed = false;
        self.samples = vec![Sample { x, at_ms: now_ms }];
    }

    /// The finger moved; `width` is the viewport width.
    pub fn touch_move(&mut self, x: f64, y: f64, width: f64, now_ms: f64) -> DrawerMove {
        let Some(mode) = self.mode.filter(|_| self.candidate) else {
            return DrawerMove::Ignore;
        };
        let dx = x - self.start_x;
        let dy = y - self.start_y;
        if !self.axis_locked {
            match lock_axis(dx, dy) {
                LockedAxis::None => return DrawerMove::Ignore,
                LockedAxis::Y => {
                    self.candidate = false;
                    return DrawerMove::Ignore;
                }
                LockedAxis::X => self.axis_locked = true,
            }
        }
        let wrong_way = match mode {
            DrawerSettle::Open => dx <= 0.0,
            DrawerSettle::Close => dx >= 0.0,
        };
        if wrong_way {
            self.candidate = false;
            return DrawerMove::Ignore;
        }
        self.armed = true;
        self.samples.push(Sample { x, at_ms: now_ms });
        while self.samples.len() > 2 && self.samples[0].at_ms < now_ms - MOVE_WINDOW_MS {
            self.samples.remove(0);
        }
        self.latest_x = x;
        let offset_px = match mode {
            DrawerSettle::Close => close_offset_px(dx, width),
            DrawerSettle::Open => open_offset_px(dx, width),
        };
        DrawerMove::Drag { offset_px }
    }

    /// The finger lifted (or the touch was cancelled): the settle to perform
    /// and whether it commits, or `None` when no drag was armed.
    pub fn touch_end(&mut self, width: f64, now_ms: f64) -> Option<(DrawerSettle, bool)> {
        let mode = self.mode.filter(|_| self.armed);
        let result = mode.map(|mode| {
            let dx = self.latest_x - self.start_x;
            while self.samples.len() > 1 && self.samples[0].at_ms < now_ms - RELEASE_WINDOW_MS {
                self.samples.remove(0);
            }
            let velocity = match (self.samples.first(), self.samples.last()) {
                (Some(first), Some(last)) if last.at_ms > first.at_ms => {
                    (last.x - first.x) / (last.at_ms - first.at_ms)
                }
                _ => 0.0,
            };
            let commit = match mode {
                DrawerSettle::Open => should_open(dx, velocity, width),
                DrawerSettle::Close => should_close(dx, velocity, width),
            };
            (mode, commit)
        });
        self.mode = None;
        self.armed = false;
        self.candidate = false;
        self.samples.clear();
        result
    }
}
