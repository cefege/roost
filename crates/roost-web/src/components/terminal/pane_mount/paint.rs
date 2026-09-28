//! The pane's paint loop and presentation: on each pump revision it notes a
//! replica advance and re-reads the lease; on the next browser frame it paints
//! the store's canonical frame into the renderer, feeds the pager and the
//! presentation controller, and publishes what the component shows. Ports the
//! renderer subscriber of `apps/web/src/components/terminal/cell-terminal-renderer.ts`
//! and the presentation glue of `cell-terminal-presentation.ts`.

use roost_client_core::store::terminal_transport::session_terminal_transport_kind;
use roost_client_core::store::terminal_transport::transport_attribute;
use roost_web_terminal::terminal_presentation::{
    PresentationFrameMark, PresentationInputs, PresentationPane, TerminalPresentationState,
};
use roost_web_terminal::{BOTTOM_FOLLOW_SETTLE_MS, CellGridRenderer, ReaderIntent};

use super::actions::{DomRepairCtx, PaneAction, perform, with_state};
use super::browser::{self, now_ms, request_frame};
use super::{FrameModes, PaneShared, PaneState};
use crate::components::terminal::pane_state::set_if_changed;
use crate::components::terminal::pane_status::{
    LoadingGate, PaneViewStatus, loading_notice, view_handle_status,
};

/// What one store read found for this pane.
struct StoreRead {
    frame_revision: u64,
    status: Option<PaneViewStatus>,
    transport: Option<&'static str>,
    gestures_forwarded_pref: bool,
}

fn read_store(shared: &PaneShared) -> StoreRead {
    let core = shared.pump.core();
    let core = core.borrow();
    let store = core.store();
    StoreRead {
        frame_revision: store
            .terminal
            .get(&shared.session_id)
            .map_or(0, |replica| replica.frame_revision()),
        status: view_handle_status(store, &shared.session_id, &shared.view_id),
        transport: session_terminal_transport_kind(store, &shared.session_id).map(transport_attribute),
        gestures_forwarded_pref: store.prefs.mouse_forward,
    }
}

/// The pump revision moved.
pub(super) fn sync_store(shared: &PaneShared) {
    if shared.disposed.get() {
        return;
    }
    let read = read_store(shared);
    let advanced = shared.state.borrow_mut().feed.observe_revision(read.frame_revision);
    set_if_changed(shared.ui.transport, read.transport);
    set_if_changed(
        shared.ui.gestures_forwarded,
        read.gestures_forwarded_pref && shared.modes.get().mouse_tracking != roost_protocol::cell::MouseTracking::None,
    );
    if advanced {
        request_owed_paint(shared);
    }
    refresh_presentation_with(shared, read.status);
}

/// Ask for a frame when a paint is owed and the pane may paint.
pub(super) fn request_owed_paint(shared: &PaneShared) {
    let owed = {
        let state = shared.state.borrow();
        state.feed.paint_owed() && state.flags.view_active() && state.page_visible
    };
    if owed {
        request_frame(shared);
    }
}

/// The browser frame came round.
pub(super) fn on_animation_frame(shared: &PaneShared) {
    let now = now_ms();
    with_state(shared, |state, actions| {
        let mut host =
            super::actions::ViewportCtx::new(shared, super::actions::state_flags(state), actions);
        state.viewport.on_animation_frame(now, &mut host);
    });
    paint_owed(shared, now);
}

fn paint_owed(shared: &PaneShared, now: u64) {
    {
        let state = shared.state.borrow();
        if !state.feed.paint_owed() || !state.flags.view_active() || !state.page_visible {
            return;
        }
    }
    #[cfg(feature = "smoke")]
    {
        if shared.panes.dom_held(&shared.session_id) {
            return;
        }
        if shared.panes.take_frame_drop(&shared.session_id) {
            shared.state.borrow_mut().feed.skip();
            return;
        }
    }
    let canonical = {
        let core = shared.pump.core();
        let core = core.borrow();
        core.store()
            .terminal
            .get(&shared.session_id)
            .and_then(|replica| replica.canonical().cloned())
    };
    let Some(canonical) = canonical else {
        shared.state.borrow_mut().feed.skip();
        return;
    };
    let applied = shared.renderer.borrow_mut().apply_full_frame(&canonical);
    if !applied {
        tracing::warn!(target: "terminal", session_id = %shared.session_id, seq = canonical.seq,
            "render paint refused; repainting on the next revision");
        shared.state.borrow_mut().feed.owe_paint();
        return;
    }
    shared.modes.set(FrameModes {
        cursor_keys_app: canonical.cursor_keys_app,
        bracketed_paste: canonical.bracketed_paste,
        mouse_sgr: canonical.mouse_sgr,
        focus_events: canonical.focus_events,
        mouse_tracking: canonical.mouse_tracking,
    });
    set_if_changed(shared.ui.alt_screen, canonical.alt_screen);
    let delivery = shared.state.borrow_mut().feed.painted(&canonical);
    let work = {
        let mut state = shared.state.borrow_mut();
        state.cursor = Some((canonical.cursor_col, canonical.cursor_row));
        let mut renderer = shared.renderer.borrow_mut();
        let work = state.backfill.on_full_frame(&mut *renderer);
        let inputs = presentation_inputs(&state, None, &renderer, now);
        let mark = PresentationFrameMark {
            full: delivery.baseline,
            grid_epoch: &canonical.grid_epoch,
            seq: canonical.seq,
        };
        state.presentation.note_frame_activity(mark, &inputs);
        work
    };
    perform(shared, vec![PaneAction::Backfill(work)]);
    after_renderer_write(shared, now);
}

/// Run what the renderer's hooks flagged during a write: the first reconcile,
/// the reconcile watermark, and a follow-band settle request.
pub(super) fn after_renderer_write(shared: &PaneShared, now: u64) {
    if shared.first_reconciled.replace(false) {
        shared.state.borrow_mut().has_reconciled_frame = true;
    }
    if shared.reconciled.replace(false) {
        let mut state = shared.state.borrow_mut();
        state.last_painted_at_ms = Some(now);
        let renderer = shared.renderer.borrow();
        let host_actions = &mut Vec::new();
        let host = DomRepairCtx {
            shared,
            actively_viewed: true,
            foreground_view_ready: true,
            pointer_gesture_active: state.pointer_gestures > 0,
            catching_up: false,
            actions: host_actions,
        };
        drop(renderer);
        state.dom_repair.note_reconciled(&host);
    }
    if shared.follow_settle_requested.replace(false) {
        let mut state = shared.state.borrow_mut();
        if state.follow_settle_due_ms.is_none() {
            state.follow_settle_due_ms = Some(now + BOTTOM_FOLLOW_SETTLE_MS);
        }
    }
    let settle = shared.renderer.borrow().pending_bottom_park_settle();
    if let Some(epoch) = settle {
        let result = shared.renderer.borrow_mut().resume_bottom_park(epoch);
        if result.anchor_changed {
            backfill_after_anchor_change(shared);
        }
    }
    refresh_presentation(shared);
}

fn backfill_after_anchor_change(shared: &PaneShared) {
    let work = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        state.backfill.on_full_frame(&mut *renderer)
    };
    perform(shared, vec![PaneAction::Backfill(work)]);
}

fn presentation_inputs<'a>(
    state: &PaneState,
    status: Option<PaneViewStatus>,
    renderer: &'a CellGridRenderer,
    now: u64,
) -> PresentationInputs<'a, CellGridRenderer> {
    PresentationInputs {
        now_ms: now,
        pane: PresentationPane {
            active: state.flags.view_active(),
            focused: state.flags.focused,
            page_visible: state.page_visible,
        },
        status: status.map(|status| status.status),
        renderer: Some(renderer),
    }
}

/// Re-read the lease and re-decide presentation, blink, offline and the card.
pub(super) fn refresh_presentation(shared: &PaneShared) {
    let status = read_store(shared).status;
    refresh_presentation_with(shared, status);
}

fn refresh_presentation_with(shared: &PaneShared, status: Option<PaneViewStatus>) {
    if shared.disposed.get() {
        return;
    }
    let now = now_ms();
    let (presentation, offline_changed, gate) = {
        let mut state = shared.state.borrow_mut();
        let mut renderer = shared.renderer.borrow_mut();
        let inputs = presentation_inputs(&state, status, &renderer, now);
        let presentation = state.presentation.refresh_terminal_presentation(&inputs);
        let pane = inputs.pane;
        state.presentation.refresh_cursor_blink(pane, Some(&mut *renderer));
        let painted_recently = state
            .last_painted_at_ms
            .is_some_and(|at| now < at + roost_web_terminal::terminal_presentation::FRAME_ACTIVITY_WINDOW_MS);
        let viewed = status.is_some() && pane.active && pane.page_visible;
        let detached = presentation == TerminalPresentationState::Detached;
        let offline_changed = state.offline.update(viewed, detached, painted_recently, now);
        let gate = LoadingGate {
            view_active: pane.active,
            page_visible: pane.page_visible,
            offline: state.offline.offline(),
            has_reconciled_frame: state.has_reconciled_frame,
            pending: state.flags.pending,
        };
        (presentation, offline_changed, gate)
    };
    set_if_changed(shared.ui.presentation, presentation);
    if offline_changed {
        set_if_changed(shared.ui.offline, gate.offline);
    }
    let notice = loading_notice(gate, status).map(|mut notice| {
        notice.session_id = Some(shared.session_id.clone());
        notice
    });
    set_if_changed(shared.ui.notice, notice);
    browser::rearm(shared);
}

/// The one timer fired: run every due deadline other than the viewport's.
pub(super) fn on_deadlines(shared: &PaneShared, now: u64) {
    let status = read_store(shared).status;
    let mut actions = Vec::new();
    let offline_fire = {
        let mut state = shared.state.borrow_mut();
        let renderer = shared.renderer.borrow();
        let inputs = presentation_inputs(&state, status, &renderer, now);
        let stalled = state.presentation.fire_due_deadline(&inputs);
        drop(renderer);
        if let Some(stalled) = stalled {
            let catching_up = state.presentation.state() == TerminalPresentationState::CatchingUp;
            let mut host = DomRepairCtx {
                shared,
                actively_viewed: state.flags.view_active() && state.page_visible,
                foreground_view_ready: status.is_some_and(|status| status.foreground_ready()),
                pointer_gesture_active: state.pointer_gestures > 0,
                catching_up,
                actions: &mut actions,
            };
            state.dom_repair.on_catch_up_stalled(stalled.0, now, &mut host);
        }
        let mut host = DomRepairCtx {
            shared,
            actively_viewed: state.flags.view_active() && state.page_visible,
            foreground_view_ready: status.is_some_and(|status| status.foreground_ready()),
            pointer_gesture_active: state.pointer_gestures > 0,
            catching_up: state.presentation.state() == TerminalPresentationState::CatchingUp,
            actions: &mut actions,
        };
        state.dom_repair.on_deadline(now, &mut host);
        let settle_due = state.follow_settle_due_ms.is_some_and(|due| due <= now);
        if settle_due {
            state.follow_settle_due_ms = None;
        }
        (state.offline.on_deadline(now), settle_due)
    };
    perform(shared, actions);
    if offline_fire.0 == Some(crate::components::terminal::offline_watch::OfflineFire::Retry) {
        perform(shared, vec![PaneAction::RepublishView]);
    }
    if offline_fire.1 {
        let result = shared.renderer.borrow_mut().settle_follow_band();
        if result.anchor_changed {
            backfill_after_anchor_change(shared);
        }
    }
    refresh_presentation_with(shared, status);
    set_if_changed(shared.ui.offline, shared.state.borrow().offline.offline());
}

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
        (renderer.reader_intent() == ReaderIntent::Reading, renderer.follows_bottom())
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
        state.dom_repair.resume_after_pointer_gesture(now, &mut host);
    }
    perform(shared, actions);
}
