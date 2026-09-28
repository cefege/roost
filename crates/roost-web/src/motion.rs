//! Pointer, drawer, spring and overlay motion shared by the shell, the deck and
//! the overlays. Ports `apps/web/src/lib/{dragThreshold,drawerDrag,
//! edgeSwipeDrawer,resizeDrag,dropZones,gridFlip,spring,overlayMotion}.ts` and
//! `apps/web/src/browser/viewTransition.ts`.
//!
//! Every decision (thresholds, offsets, zones, spring steps, owner tokens) is a
//! native function with a test; the wasm32 adapters beside them only write
//! styles, schedule frames and hold listeners.

pub mod drag_threshold;
pub mod drawer_drag;
pub mod drop_zones;
pub mod edge_swipe_drawer;
pub mod grid_flip;
pub mod overlay_motion;
pub mod resize_drag;
#[cfg(target_arch = "wasm32")]
pub mod resize_pointer;
pub mod spring;
pub mod view_transition;
