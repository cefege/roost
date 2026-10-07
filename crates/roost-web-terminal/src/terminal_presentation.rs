//! Terminal presentation policy — separates stream receipt from painted state.
//! The pane owns its view activity and renderer; this module owns the bounded
//! receiving indicator, the reconciliation hold state and its foreground DOM
//! stall, the detached-view grace behind the "no live stream" indicator, and
//! the cursor-blink policy. Reads the renderer through
//! `PresentationRendererView`. Ports `apps/web/src/renderer/terminalPresentation.ts`.

mod controller;
mod renderer_view;
mod state;

pub use controller::{
    CatchUpStalled, PresentationFrameMark, PresentationInputs, PresentationPane,
    TerminalPresentationController,
};
pub use state::{
    DETACHED_GRACE_MS, FRAME_ACTIVITY_WINDOW_MS, TerminalPresentationActivity,
    TerminalPresentationInput, TerminalPresentationState, TerminalViewHandleStatus,
    derive_terminal_presentation_state,
};

use crate::presentation::RendererEpochSeq;
use crate::reader_intent::ReaderIntentReason;

/// How long the DOM may sit behind canonical on a foreground pane before the
/// pane's DOM repair is told.
pub const FOREGROUND_DOM_STALL_MS: u64 = 1_000;

/// The renderer reads and the one write presentation needs.
pub trait PresentationRendererView {
    /// How far canonical has advanced.
    fn canonical_epoch_seq(&self) -> RendererEpochSeq;
    /// How far the DOM has reconciled.
    fn reconciled_epoch_seq(&self) -> RendererEpochSeq;
    /// Why the reader is parked, or `None` while it is live.
    fn reader_reason(&self) -> Option<ReaderIntentReason>;
    /// Turn the cursor blink presentation on or off.
    fn set_cursor_blink_enabled(&mut self, enabled: bool);
}

/// A parked reader's explicit hold outranks the foreground stall: repairing
/// would rewrite the DOM under the reader. Every reason is named so a new one
/// has to decide.
pub fn preserves_foreground_reader_hold(reason: Option<ReaderIntentReason>) -> bool {
    match reason {
        Some(
            ReaderIntentReason::NativeScroll
            | ReaderIntentReason::Wheel
            | ReaderIntentReason::Touch
            | ReaderIntentReason::Selection
            | ReaderIntentReason::Find
            | ReaderIntentReason::PromptJump,
        ) => true,
        None => false,
    }
}
