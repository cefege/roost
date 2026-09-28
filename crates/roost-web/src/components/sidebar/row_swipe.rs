//! Swipe-to-close for a touch session row: the axis lock, the left-only drag
//! offset, the tap-suppression mark, and the release decision. The gesture
//! half of `apps/web/src/components/sidebar/SessionRow.tsx`; `SessionRow`
//! feeds it touch points and renders its offset.

/// How far left a release must be to close the row.
pub const SWIPE_CLOSE_THRESHOLD_PX: f64 = 96.0;
/// Movement under this on both axes has not chosen an axis yet.
pub const SWIPE_AXIS_SLOP_PX: f64 = 8.0;
/// A drag past this is not a tap, so the release's click must not navigate.
pub const SWIPE_TAP_SLOP_PX: f64 = 10.0;

/// Which axis the finger committed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SwipeAxis {
    #[default]
    Undecided,
    Horizontal,
    Vertical,
}

/// What a release does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeRelease {
    /// Spring back to rest.
    SpringBack,
    /// Slide out and close.
    Close,
}

/// One row's gesture.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RowSwipe {
    start_x: f64,
    start_y: f64,
    axis: SwipeAxis,
    offset_x: f64,
    swiped: bool,
    tracking: bool,
}

impl RowSwipe {
    /// A finger landed.
    pub fn start(&mut self, x: f64, y: f64) {
        *self = Self {
            start_x: x,
            start_y: y,
            offset_x: self.offset_x,
            tracking: true,
            ..Self::default()
        };
    }

    /// The finger moved. `true` when the row owns the gesture now, so the
    /// caller must prevent the page from scrolling.
    pub fn track(&mut self, x: f64, y: f64, viewport_width: f64) -> bool {
        let dx = x - self.start_x;
        let dy = y - self.start_y;
        if self.axis == SwipeAxis::Undecided {
            if dx.abs() < SWIPE_AXIS_SLOP_PX && dy.abs() < SWIPE_AXIS_SLOP_PX {
                return false;
            }
            self.axis = if dx.abs() > dy.abs() {
                SwipeAxis::Horizontal
            } else {
                SwipeAxis::Vertical
            };
        }
        if self.axis != SwipeAxis::Horizontal {
            return false;
        }
        self.offset_x = dx.clamp(-viewport_width.max(0.0), 0.0);
        if self.offset_x < -SWIPE_TAP_SLOP_PX {
            self.swiped = true;
        }
        true
    }

    /// The finger lifted.
    pub fn release(&mut self, viewport_width: f64) -> SwipeRelease {
        self.tracking = false;
        if self.offset_x <= -SWIPE_CLOSE_THRESHOLD_PX {
            self.offset_x = -viewport_width.max(0.0);
            SwipeRelease::Close
        } else {
            self.offset_x = 0.0;
            SwipeRelease::SpringBack
        }
    }

    /// Whether the release's click is the end of a swipe (and is consumed).
    pub fn take_swiped(&mut self) -> bool {
        std::mem::take(&mut self.swiped)
    }

    /// The row's horizontal offset, `<= 0`.
    pub fn offset_x(&self) -> f64 {
        self.offset_x
    }

    /// Whether a finger is down (the row follows it without a transition).
    pub fn tracking(&self) -> bool {
        self.tracking
    }
}
