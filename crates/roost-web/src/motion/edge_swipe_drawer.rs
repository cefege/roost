//! The compact drawer's swipe geometry: which axis a touch locks to, where the
//! drawer sits under the finger, and whether a release commits. Ports
//! `apps/web/src/lib/edgeSwipeDrawer.ts`; read by
//! `components::layout::drawer_gesture` (the gesture state machine).
//!
//! Open is rightward-positive and close is leftward-negative: a flick in the
//! wrong direction never commits either way.

/// A touch must START within this many pixels of the left edge to open.
pub const EDGE_PX: f64 = 24.0;
/// Travel under which neither axis has won yet.
pub const ARM_PX: f64 = 10.0;
/// Horizontal wins only when `|dx| > |dy| * AXIS_RATIO`.
pub const AXIS_RATIO: f64 = 1.5;
/// Fraction of the drawer's width past which a release commits.
const COMMIT_FRACTION: f64 = 0.3;
/// Pixels per millisecond past which a release commits regardless of distance.
const FLICK_VELOCITY: f64 = 0.8;

/// The axis a gesture has locked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockedAxis {
    /// Still under the arm gate.
    None,
    /// Horizontal: the drawer follows the finger.
    X,
    /// Vertical: the gesture belongs to whatever scrolls underneath.
    Y,
}

/// Which axis a gesture with this travel locks to.
pub fn lock_axis(dx: f64, dy: f64) -> LockedAxis {
    if dx.abs() < ARM_PX && dy.abs() < ARM_PX {
        return LockedAxis::None;
    }
    if dx.abs() > dy.abs() * AXIS_RATIO {
        LockedAxis::X
    } else {
        LockedAxis::Y
    }
}

/// The drawer's `translateX` while dragging it OPEN: from `-width` (closed)
/// toward 0 (open), clamped to that range.
pub fn open_offset_px(dx: f64, width: f64) -> f64 {
    -width + dx.min(width).max(0.0)
}

/// Whether releasing an open drag commits: past 30% of the width, or a
/// rightward flick.
pub fn should_open(dx: f64, velocity: f64, width: f64) -> bool {
    dx >= width * COMMIT_FRACTION || velocity >= FLICK_VELOCITY
}

/// The drawer's `translateX` while dragging it CLOSED: from 0 (open) toward
/// `-width` (off the left edge), clamped to that range.
pub fn close_offset_px(dx: f64, width: f64) -> f64 {
    dx.max(-width).min(0.0)
}

/// Whether releasing a close drag commits: past 30% of the width leftward, or a
/// leftward flick.
pub fn should_close(dx: f64, velocity: f64, width: f64) -> bool {
    dx <= -width * COMMIT_FRACTION || velocity <= -FLICK_VELOCITY
}
