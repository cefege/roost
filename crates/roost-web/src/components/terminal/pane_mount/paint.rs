//! The pane's paint loop and presentation: on each pump revision it notes a
//! replica advance and re-reads the lease; on the next browser frame it paints
//! the store's canonical frame into the renderer, feeds the pager and the
//! presentation controller, and publishes what the component shows. Ports the
//! renderer subscriber of `apps/web/src/components/terminal/cell-terminal-renderer.ts`
//! and the presentation glue of `cell-terminal-presentation.ts`.

use roost_client_core::store::terminal_transport::session_terminal_transport_kind;
use roost_client_core::store::terminal_transport::transport_attribute;
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::terminal_presentation::{
    PresentationFrameMark, PresentationInputs, PresentationPane, TerminalPresentationState,
};
use roost_web_terminal::{BOTTOM_FOLLOW_SETTLE_MS, CellGridRenderer};
use serde_json::json;

use super::actions::{DomRepairCtx, PaneAction, perform, with_state};
use super::browser::{self, now_ms, request_frame};
use super::{FrameModes, PaneShared, PaneState};
use crate::components::terminal::pane_state::set_if_changed;
use crate::components::terminal::pane_status::{
    LoadingGate, PaneViewStatus, loading_notice, view_handle_status,
};
use crate::platform::browser::perf_counters::with_perf_counters;
use crate::platform::browser::phase_marks::{PhaseName, mark_phase_once};

/// What one store read found for this pane.
pub(super) struct StoreRead {
    frame_revision: u64,
    pub(super) status: Option<PaneViewStatus>,
    transport: Option<&'static str>,
    gestures_forwarded_pref: bool,
}

/// How a paint reached the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaintedAs {
    Deltas,
    Full,
}

pub(super) fn read_store(shared: &PaneShared) -> StoreRead {
    let core = shared.pump.core();
    let core = core.borrow();
    let store = core.store();
    StoreRead {
        frame_revision: store
            .terminal
            .get(&shared.session_id)
            .map_or(0, |replica| replica.frame_revision()),
        status: view_handle_status(store, &shared.session_id, &shared.view_id),
        transport: session_terminal_transport_kind(store, &shared.session_id)
            .map(transport_attribute),
        gestures_forwarded_pref: store.prefs.mouse_forward,
    }
}

/// The pump revision moved.
pub(super) fn sync_store(shared: &PaneShared) {
    if shared.disposed.get() {
        return;
    }
    let read = read_store(shared);
    let advanced = {
        let mut state = shared.state.borrow_mut();
        let advanced = state.feed.observe_revision(read.frame_revision);
        if advanced && !(state.flags.view_active() && state.page_visible) {
            state.feed.park();
        }
        advanced
    };
    set_if_changed(shared.ui.transport, read.transport);
    set_if_changed(
        shared.ui.gestures_forwarded,
        read.gestures_forwarded_pref
            && shared.modes.get().mouse_tracking != roost_protocol::cell::MouseTracking::None,
    );
    if advanced {
        request_owed_paint(shared);
    }
    super::echo::drain_outcomes(shared);
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
    let delta_base = shared.state.borrow().feed.delta_base();
    let read = {
        let core = shared.pump.core();
        let core = core.borrow();
        core.store()
            .terminal
            .get(&shared.session_id)
            .and_then(|replica| {
                let canonical = replica.canonical()?.clone();
                let deltas = delta_base
                    .and_then(|base| replica.deltas_since(base))
                    .filter(|deltas| !deltas.is_empty())
                    .map(<[CellGridFrame]>::to_vec);
                Some((replica.frame_revision(), canonical, deltas))
            })
    };
    let Some((revision, canonical, deltas)) = read else {
        shared.state.borrow_mut().feed.skip();
        return;
    };
    let Some(painted_as) = apply_owed_frames(shared, &canonical, deltas.as_deref()) else {
        tracing::warn!(target: "terminal", session_id = %shared.session_id, seq = canonical.seq,
            "render paint refused; repainting on the next revision");
        shared.state.borrow_mut().feed.owe_paint();
        return;
    };
    shared.modes.set(FrameModes {
        cursor_keys_app: canonical.cursor_keys_app,
        bracketed_paste: canonical.bracketed_paste,
        mouse_sgr: canonical.mouse_sgr,
        focus_events: canonical.focus_events,
        mouse_tracking: canonical.mouse_tracking,
    });
    set_if_changed(shared.ui.alt_screen, canonical.alt_screen);
    let delivery = shared.state.borrow_mut().feed.painted(&canonical, revision);
    super::echo::on_frame(shared, &canonical, delivery.scrollback_appended);
    mark_painted_frame(shared, &canonical, painted_as == PaintedAs::Full);
    // The view's real status: without it the controller reads the pane as
    // unready, and an unready refresh forgets the activity this frame just
    // recorded, so the pane never reads `receiving`.
    let status = read_store(shared).status;
    let work = {
        let mut state = shared.state.borrow_mut();
        state.cursor = Some((canonical.cursor_col, canonical.cursor_row));
        let mut renderer = shared.renderer.borrow_mut();
        let work = state.backfill.on_full_frame(&mut *renderer);
        let inputs = presentation_inputs(&state, status, &renderer, now);
        // Only a delta batch is output; a full is an attach or a repair, as
        // v2's renderer delivery marks it.
        let mark = PresentationFrameMark {
            full: painted_as == PaintedAs::Full,
            grid_epoch: &canonical.grid_epoch,
            seq: canonical.seq,
        };
        state.presentation.note_frame_activity(mark, &inputs);
        work
    };
    perform(shared, vec![PaneAction::Backfill(work)]);
    after_renderer_write(shared, now);
}

/// A painted frame closes the pane's input round trip, and the first one is
/// the session's first applied cell.
fn mark_painted_frame(shared: &PaneShared, canonical: &CellGridFrame, full: bool) {
    let now = now_ms() as f64;
    with_perf_counters(|counters| counters.note_frame_painted(&shared.session_id, now));
    mark_phase_once(
        PhaseName::FirstCellApply,
        &shared.session_id,
        &[
            ("sessionId", json!(shared.session_id)),
            ("sequence", json!(canonical.seq)),
            ("full", json!(full)),
        ],
    );
}

/// Fold the deltas the renderer has not seen, so the history they appended is
/// painted; the canonical full when there are none or the renderer refuses the
/// chain (a skipped delivery, a different grid). v2's scheduler makes the same
/// fallback (`terminal-render-scheduler.ts` `fallback_full`). `None` when the
/// renderer refused the full too.
fn apply_owed_frames(
    shared: &PaneShared,
    canonical: &CellGridFrame,
    deltas: Option<&[CellGridFrame]>,
) -> Option<PaintedAs> {
    let mut renderer = shared.renderer.borrow_mut();
    if let Some(deltas) = deltas {
        if renderer.apply_delta_frames(deltas) {
            return Some(PaintedAs::Deltas);
        }
        tracing::debug!(target: "terminal", session_id = %shared.session_id,
            deltas = deltas.len(), seq = canonical.seq,
            "delta batch refused; painting the canonical full");
    }
    renderer
        .apply_full_frame(canonical)
        .then_some(PaintedAs::Full)
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
        let host_actions = &mut Vec::new();
        let host = DomRepairCtx {
            shared,
            actively_viewed: true,
            foreground_view_ready: true,
            pointer_gesture_active: state.pointer_gestures > 0,
            catching_up: false,
            actions: host_actions,
        };
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

pub(super) fn backfill_after_anchor_change(shared: &PaneShared) {
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
        state
            .presentation
            .refresh_cursor_blink(pane, Some(&mut *renderer));
        let painted_recently = state.last_painted_at_ms.is_some_and(|at| {
            now < at + roost_web_terminal::terminal_presentation::FRAME_ACTIVITY_WINDOW_MS
        });
        let viewed = status.is_some() && pane.active && pane.page_visible;
        let detached = presentation == TerminalPresentationState::Detached;
        let offline_changed = state
            .offline
            .update(viewed, detached, painted_recently, now);
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
            state
                .dom_repair
                .on_catch_up_stalled(stalled.0, now, &mut host);
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
