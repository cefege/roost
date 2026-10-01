//! The screen the dictation recogniser is biased with, read out of the pane.
//!
//! Split out of `cell_terminal.rs` because it is dictation's concern and not the
//! terminal's: the cell owns the grid, and the pane registry is what knows what
//! the operator can currently SEE.

use crate::voice::keyterms::{ContextReader, TerminalContext};

/// How many history rows the recognizer is biased with.
///
/// The viewport plus a screen of history is what an operator has actually seen:
/// further back is the same words as further up, and the scoring decays what is
/// further back anyway.
const DICTATION_SCROLLBACK_ROWS: usize = 250;

/// The live terminal context one dictation is scored against.
///
/// The composer's own draft is added by `super::terminal_chrome::composer`; this
/// supplies the two halves the renderer owns. Ports v2's `readContext` at
/// `cell-terminal-input.ts:190`.
pub(super) fn dictation_context(
    panes: super::pane_registry::PaneRegistry,
    session_id: &str,
) -> ContextReader {
    let session_id = session_id.to_owned();
    ContextReader::new(move || TerminalContext {
        grid: panes.viewport_text(&session_id).unwrap_or_default(),
        scrollback: panes
            .scrollback_text(&session_id, DICTATION_SCROLLBACK_ROWS)
            .unwrap_or_default(),
        ..TerminalContext::default()
    })
}
