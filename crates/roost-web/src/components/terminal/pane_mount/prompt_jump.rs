//! Mod+Shift+Up / Mod+Shift+Down: move the reader between the prompt rows the
//! shell marked with OSC 133.
//!
//! Called by `input::run_reserved_chord`. Prompts are found among the rows the
//! pane holds — painted history and the live screen. When the next prompt in
//! the asked direction is in history the pane has not painted, the shared pager
//! pulls the adjacent page (`ScrollbackBackfill::ensure_row_painted`) and the
//! jump runs again once it settles, page by page, up to the whole retained
//! history.

use std::collections::BTreeMap;

use roost_protocol::cell::row_mark;
use roost_web_terminal::{ReaderIntent, ReaderIntentReason};

use super::PaneShared;
use super::browser::now_ms;
use super::paint::after_renderer_write;

/// Pages one jump may pull before it gives up: 40 pages of the pager's 250
/// rows covers the worker's 10 000 retained lines.
const PROMPT_SEEK_MAX_PAGES: u8 = 40;

/// Which way the reader asked to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptDirection {
    Previous,
    Next,
}

/// A pane's prompt-jump state.
#[derive(Debug, Default)]
pub(in crate::components::terminal) struct PromptSeek {
    /// History rows pulled for a jump and not yet settled, with the direction
    /// that jump was going and how many more pages it may pull.
    pending: BTreeMap<u32, (PromptDirection, u8)>,
    /// The prompt row the last jump landed on. The reader's own anchor is the
    /// row at the TOP of the screen, and the jump places its target a third of
    /// the way down, so the anchor alone would skip the prompts in between.
    landed: Option<u32>,
}

/// The rows the pane holds, numbered absolutely, ascending, and where the
/// reader is.
struct PromptView {
    prompts: Vec<u32>,
    oldest_painted: Option<u32>,
    newest_painted: Option<u32>,
    scrollback_total: u32,
    alt_screen: bool,
    reading: bool,
    parked_by_jump: bool,
    anchor: Option<u32>,
}

impl PromptView {
    fn read(shared: &PaneShared) -> Option<Self> {
        let renderer = shared.renderer.borrow();
        let frame = renderer.current_frame()?;
        let scrollback_total = u32::try_from(frame.scrollback_total).unwrap_or(u32::MAX);
        let projection = renderer.renderer_projection();
        let history = projection
            .painted_history
            .iter()
            .map(|row| (row.index, row.mark));
        let screen = frame
            .viewport_rows
            .iter()
            .map(|row| (scrollback_total.saturating_add(row.index), row.mark));
        let prompts = history
            .chain(screen)
            .filter(|(_, mark)| mark & row_mark::PROMPT != 0)
            .map(|(index, _)| index)
            .collect();
        Some(Self {
            prompts,
            oldest_painted: projection.painted_history.first().map(|row| row.index),
            newest_painted: projection.painted_history.last().map(|row| row.index),
            scrollback_total,
            alt_screen: frame.alt_screen,
            reading: projection.reader_intent == ReaderIntent::Reading,
            parked_by_jump: projection.reader_reason == Some(ReaderIntentReason::PromptJump),
            anchor: projection.reader_anchor.map(|anchor| anchor.row),
        })
    }

    /// The row a jump moves away from: the prompt the last jump landed on
    /// while the reader is still parked there, the reader's position while it
    /// reads history, else the newest prompt on the live screen, so the first
    /// Previous from the tail lands on the last command's prompt.
    fn origin(&self, landed: Option<u32>) -> u32 {
        if self.parked_by_jump
            && let Some(landed) = landed
        {
            return landed;
        }
        if self.reading
            && let Some(anchor) = self.anchor
        {
            return anchor;
        }
        self.prompts.last().copied().unwrap_or(u32::MAX)
    }
}

pub(super) fn jump_prompt(shared: &PaneShared, direction: PromptDirection) {
    jump(shared, direction, PROMPT_SEEK_MAX_PAGES);
}

fn jump(shared: &PaneShared, direction: PromptDirection, pages_left: u8) {
    let Some(view) = PromptView::read(shared) else {
        return;
    };
    // A full-screen program's canvas has no prompts, and scrolling it would
    // only scroll the shell history hidden behind it.
    if view.alt_screen {
        return;
    }
    let origin = view.origin(shared.state.borrow().prompt_seek.landed);
    let target = match direction {
        PromptDirection::Previous => view.prompts.iter().rev().find(|row| **row < origin),
        PromptDirection::Next => view.prompts.iter().find(|row| **row > origin),
    };
    if let Some(&row) = target {
        reveal(shared, &view, row);
        return;
    }
    match direction {
        PromptDirection::Previous => match view.oldest_painted {
            Some(0) => {}
            Some(oldest) => seek(shared, oldest - 1, direction, pages_left),
            None if view.scrollback_total > 0 => {
                seek(shared, view.scrollback_total - 1, direction, pages_left);
            }
            None => {}
        },
        PromptDirection::Next => match view.newest_painted {
            Some(newest) if view.reading && newest.saturating_add(1) < view.scrollback_total => {
                seek(shared, newest + 1, direction, pages_left);
            }
            _ => {
                shared.state.borrow_mut().prompt_seek.landed = None;
                super::scroll::jump_to_live(shared);
            }
        },
    }
}

/// Scroll `row` into view, parked as a prompt jump rather than a find.
fn reveal(shared: &PaneShared, view: &PromptView, row: u32) {
    let moved = {
        let mut renderer = shared.renderer.borrow_mut();
        if row >= view.scrollback_total {
            renderer.scroll_to_viewport_row_for(
                row - view.scrollback_total,
                ReaderIntentReason::PromptJump,
            )
        } else {
            renderer.scroll_to_scrollback_row_for(row, ReaderIntentReason::PromptJump)
        }
    };
    if moved {
        shared.state.borrow_mut().prompt_seek.landed = Some(row);
        after_renderer_write(shared, now_ms());
    }
}

/// Pull the page holding `row` and jump again once it is painted.
fn seek(shared: &PaneShared, row: u32, direction: PromptDirection, pages_left: u8) {
    let Some(pages_left) = pages_left.checked_sub(1) else {
        tracing::debug!(session_id = %shared.session_id, "prompt jump gave up: no prompt within the page budget");
        return;
    };
    let actions = {
        let mut state = shared.state.borrow_mut();
        state
            .prompt_seek
            .pending
            .insert(row, (direction, pages_left));
        let mut renderer = shared.renderer.borrow_mut();
        state.backfill.ensure_row_painted(row, &mut *renderer)
    };
    super::backfill_io::perform_backfill(shared, actions);
}

/// A pager pull settled; if a prompt jump asked for it, carry the jump on.
pub(super) fn on_row_settled(shared: &PaneShared, row: u32, painted: bool) {
    let pending = shared.state.borrow_mut().prompt_seek.pending.remove(&row);
    if let Some((direction, pages_left)) = pending
        && painted
    {
        jump(shared, direction, pages_left);
    }
}
