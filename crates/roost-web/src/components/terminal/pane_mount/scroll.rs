//! The display's scroll, box-resize and pointer-settle reactions: a user
//! scroll parks or follows the reader and pages history, the jump-to-bottom
//! button resumes it, a box resize re-pins the reader, and a lifted pointer
//! resumes a DOM repair a gesture deferred.
//! Called from the pane's listeners in `browser`. Ports the scroll arms of
//! `apps/web/src/components/terminal/cell-terminal-lifecycle.ts` and
//! `cell-terminal-dom-repair.ts`.

use roost_web_terminal::terminal_presentation::TerminalPresentationState;
use roost_web_terminal::{BOTTOM_FOLLOW_SETTLE_MS, ReaderIntent};

use super::PaneShared;
use super::actions::{DomRepairCtx, PaneAction, perform};
use super::browser::now_ms;
use super::paint::{after_renderer_write, backfill_after_anchor_change, read_store};

/// The display scrolled.
pub(super) fn on_scroll(shared: &PaneShared) {
    let now = now_ms();
    let (active, visible) = {
        let state = shared.state.borrow();
        (state.flags.view_active(), state.page_visible)
    };
    if !active || !visible {
        shared.renderer.borrow_mut().prepare_live_interaction();
        return;
    }
    shared.renderer.borrow_mut().handle_scroll();
    let (reading, follows) = {
        let renderer = shared.renderer.borrow();
        (
            renderer.reader_intent() == ReaderIntent::Reading,
            renderer.follows_bottom(),
        )
    };
    if reading {
        shared.state.borrow_mut().follow_settle_due_ms = Some(now + BOTTOM_FOLLOW_SETTLE_MS);
    }
    let work = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        if follows {
            if !reading {
                state.backfill.suspend()
            } else {
                Vec::new()
            }
        } else {
            state.backfill.on_user_scroll(&mut *renderer)
        }
    };
    perform(shared, vec![PaneAction::Backfill(work)]);
    after_renderer_write(shared, now);
}

/// The jump-to-bottom button: end every reader hold and pin to the live tail,
/// the same explicit resume a keystroke makes, without sending a byte.
pub(super) fn jump_to_live(shared: &PaneShared) {
    super::interactions::prepare_live_interaction(shared);
    shared.state.borrow_mut().follow_settle_due_ms = None;
    tracing::info!(target: "terminal", session_id = %shared.session_id,
        "the reader jumped to the live tail");
    after_renderer_write(shared, now_ms());
}

/// The display's box changed size.
pub(super) fn on_box_resize(shared: &PaneShared) {
    let result = shared.renderer.borrow_mut().note_box_resize();
    if result.anchor_changed {
        backfill_after_anchor_change(shared);
    }
}

/// A pointer lifted; resume a repair a gesture deferred.
pub(super) fn on_pointer_settled(shared: &PaneShared) {
    let now = now_ms();
    let status = read_store(shared).status;
    let mut actions = Vec::new();
    {
        let mut state = shared.state.borrow_mut();
        if state.pointer_gestures == 0 {
            return;
        }
        state.pointer_gestures = 0;
        let mut host = DomRepairCtx {
            shared,
            actively_viewed: state.flags.view_active() && state.page_visible,
            foreground_view_ready: status.is_some_and(|status| status.foreground_ready()),
            pointer_gesture_active: false,
            catching_up: state.presentation.state() == TerminalPresentationState::CatchingUp,
            actions: &mut actions,
        };
        state
            .dom_repair
            .resume_after_pointer_gesture(now, &mut host);
    }
    perform(shared, actions);
}
