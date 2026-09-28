//! The imperative `web-sys` terminal renderer. One cell row is one element, and
//! a virtual tree over it would put a diff between the replica and the painted
//! grid on every frame, so this crate owns the DOM contract and holds no
//! framework code — the state machine is testable outside a browser.
//!
//! `CellGridRenderer` is the single owner of a pane's painted terminal: the
//! reader intent machine, the immutable history sheet, the reconcile watermark
//! and the one writer of `scrollTop`. Its `impl` blocks are split across
//! sibling modules by concern; that is a file split, not a type split, because
//! the per-frame state those methods share is exactly what keeps painted
//! history from disagreeing with the frame that described it. It paints
//! through the `RenderElement` seam: `web_sys::Element` in a browser, and an
//! in-memory element in the native test tier.
//!
//! Depends on `roost-protocol` for the cell model and `roost-client-core` for
//! the absolute history arithmetic. It depends on nothing else of the workspace,
//! and it re-implements nothing either of those two owns.

#![forbid(unsafe_code)]

pub mod backfill;
pub mod block_placeholder;
pub mod cell_geometry;
pub mod cell_renderer;
pub mod cell_renderer_dom;
pub mod cell_row;
pub mod echo_overlay;
#[cfg(target_arch = "wasm32")]
pub mod element_style;
pub mod find;
pub mod input;
pub mod link_target;
pub mod links;
pub mod mouse_forward;
pub mod painted_history;
pub mod presentation;
pub mod reader_intent;
pub mod reader_scroll;
pub mod render_element;
pub mod scheduler;
pub mod startup_progress;
pub mod terminal_presentation;

pub use block_placeholder::{DEFAULT_CELL_ROW_PX, SCROLLBACK_BLOCK_ROWS, block_placeholder};
pub use cell_geometry::{TerminalCellGeometry, cell_from_point, grid_geometry_from_box};
pub use cell_renderer::CellGridRenderer;
pub use cell_renderer_dom::{DomSetupError, GhostCursor};
pub use cell_row::{
    FindHit, LINK_KEY_ATTR, ROW_COLUMNS_ATTR, ROW_HAS_LINKS_ATTR, TERMINAL_LINK_CLASS,
    TERMINAL_LINK_TARGET_ATTR,
};
pub use link_target::{TerminalLinkTarget, classify_terminal_link_target};
pub use painted_history::{MAX_HELD_SCROLLBACK_ROWS, PaintedHistory};
pub use presentation::{
    BackfillAnchor, LiveInteractionResult, NO_LIVE_INTERACTION_RESULT, PaintedRowText,
    RendererEpochSeq, RendererFrameMode, RendererIncidentObserver, RendererIncidentPhase,
    RendererPaintPresentation, RendererPresentationSnapshot, RendererProjection,
    RendererTerminalModeSnapshot,
};
pub use reader_intent::{
    BOTTOM_FOLLOW_SETTLE_MS, BOTTOM_FOLLOW_SLACK_ROWS, RENDERER_HOLD_LINK, RENDERER_HOLD_SELECTION,
    ReaderAnchor, ReaderIntent, ReaderIntentReason, ReconcileBlockReason, ScrollBoxGeometry,
};
pub use render_element::{ElementRect, RenderElement};
