//! The drag-arming gate for pointer drags: a drag arms once the pointer has
//! travelled the threshold from its start in ANY direction. Ports
//! `apps/web/src/lib/dragThreshold.ts`; read by the deck's pane-strip tab drag.
//!
//! Euclidean, never horizontal-only: a straight-down split-drag leaves a 40px
//! tab before any x-delta accumulates, so an x-only gate never armed it.

/// How far, in CSS pixels, the pointer travels before a press becomes a drag.
pub const DRAG_THRESHOLD_PX: f64 = 8.0;

/// Whether a pointer that went down at (`start_x`, `start_y`) and is now at
/// (`x`, `y`) has travelled far enough to arm a drag.
pub fn drag_armed(start_x: f64, start_y: f64, x: f64, y: f64) -> bool {
    (x - start_x).hypot(y - start_y) >= DRAG_THRESHOLD_PX
}
