//! The pane's side effects, performed only after every state borrow is
//! released: a native state machine answers into a `Vec<PaneAction>` while
//! the pane state is borrowed, and `perform` runs them afterwards, so a view
//! dispatch or a renderer write can never meet a borrow it re-enters. Also the
//! two hosts those machines read (viewport publication, DOM repair). Ports the
//! glue of `apps/web/src/components/terminal/cell-terminal-viewport.ts`,
//! `cell-terminal-lifecycle.ts` and `cell-terminal-presentation.ts`.

use roost_client_core::terminal::token::TerminalTransport;
use roost_client_core::{ClientEvent, TransportControl};
use roost_web_terminal::RendererEpochSeq;
use roost_web_terminal::backfill::BackfillAction;
use roost_web_terminal::cell_geometry::{measure_terminal_cell_box, terminal_geometry_for_element};
use roost_web_terminal::terminal_presentation::preserves_foreground_reader_hold;

use super::{PaneShared, PaneState, browser};
use crate::components::terminal::dom_repair::DomRepairHost;
use crate::components::terminal::pane_state::PaneFlags;
use crate::components::terminal::viewport_publication::ViewportHost;

/// One deferred side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneAction {
    /// Open or resize the view at this geometry.
    PublishView { cols: u32, rows: u32 },
    /// Stop claiming: hide the view, suspend the pager, stop activity.
    WithdrawView,
    /// Release selection and armed holds before a park.
    ReleasePaintHolds,
    /// Drop predictions and live holds, then republish the view.
    RepairLocally,
    /// Recover the carrier generation that owns the stalled view.
    RecoverGeneration,
    /// Republish the current view lease.
    RepublishView,
    /// Work the pager handed back.
    Backfill(Vec<BackfillAction>),
}

/// Borrow the pane state, run `step` against it, then perform what it asked.
pub(super) fn with_state(
    shared: &PaneShared,
    step: impl FnOnce(&mut PaneState, &mut Vec<PaneAction>),
) {
    let mut actions = Vec::new();
    step(&mut shared.state.borrow_mut(), &mut actions);
    perform(shared, actions);
}

/// Perform deferred actions in order.
pub(super) fn perform(shared: &PaneShared, actions: Vec<PaneAction>) {
    for action in actions {
        match action {
            PaneAction::PublishView { cols, rows } => publish_view(shared, cols, rows),
            PaneAction::WithdrawView => withdraw_view(shared),
            PaneAction::ReleasePaintHolds => super::interactions::release_paint_holds(shared),
            PaneAction::RepairLocally => {
                super::echo::clear(shared);
                super::interactions::prepare_live_interaction(shared);
                republish(shared);
            }
            PaneAction::RecoverGeneration => recover_generation(shared),
            PaneAction::RepublishView => republish(shared),
            PaneAction::Backfill(work) => browser::perform_backfill(shared, work),
        }
    }
    browser::rearm(shared);
}

fn publish_view(shared: &PaneShared, cols: u32, rows: u32) {
    let (opened, previous) = {
        let mut state = shared.state.borrow_mut();
        let opened = std::mem::replace(&mut state.view_opened, true);
        (opened, state.published.replace((cols, rows)))
    };
    let event = if opened {
        if previous == Some((cols, rows)) {
            // A same-size claim after a withdraw re-opens the lease.
            open_event(shared, cols, rows)
        } else {
            ClientEvent::ViewResized {
                session_id: shared.session_id.clone(),
                view_id: shared.view_id.clone(),
                cols,
                rows,
            }
        }
    } else {
        open_event(shared, cols, rows)
    };
    shared.pump.dispatch(event);
    let mut state = shared.state.borrow_mut();
    let active = state.page_visible && state.flags.view_active();
    state.backfill.set_active(active);
}

fn open_event(shared: &PaneShared, cols: u32, rows: u32) -> ClientEvent {
    ClientEvent::ViewOpened {
        session_id: shared.session_id.clone(),
        worker_fp: shared.worker_fp.clone(),
        view_id: shared.view_id.clone(),
        cols,
        rows,
    }
}

fn republish(shared: &PaneShared) {
    let published = shared.state.borrow().published;
    if let Some((cols, rows)) = published {
        shared.pump.dispatch(open_event(shared, cols, rows));
    }
}

fn withdraw_view(shared: &PaneShared) {
    let (was_open, retired) = {
        let mut state = shared.state.borrow_mut();
        state.presentation.clear_frame_activity();
        state.backfill.set_active(false);
        let retired = state.backfill.suspend();
        // A withdrawn view re-opens on its next claim even at the same size.
        state.published = None;
        (state.view_opened, retired)
    };
    shared.renderer.borrow_mut().set_cursor_blink_enabled(false);
    if was_open {
        shared.pump.dispatch(ClientEvent::ViewHidden {
            session_id: shared.session_id.clone(),
            view_id: shared.view_id.clone(),
        });
    }
    browser::perform_backfill(shared, retired);
}

/// The generation that owns the stalled view: the Sync socket, or the
/// direct carrier elected for the session.
fn recover_generation(shared: &PaneShared) {
    let event = {
        let core = shared.pump.core();
        let core = core.borrow();
        let store = core.store();
        let transport = store
            .terminal
            .get(&shared.session_id)
            .and_then(|replica| replica.generation())
            .map(|token| token.transport);
        match transport {
            Some(TerminalTransport::Sync) => {
                Some(ClientEvent::SyncTransportControl(TransportControl::Reconnect))
            }
            Some(_) => store
                .routes
                .route(&shared.session_id)
                .map(|route| ClientEvent::CarrierLost {
                    connection_id: route.connection_id.clone(),
                }),
            None => None,
        }
    };
    if let Some(event) = event {
        tracing::warn!(target: "terminal", session_id = %shared.session_id,
            "terminal-dom-reconcile-timeout: recovering the owning generation");
        shared.pump.dispatch(event);
    }
}

/// The props changed: re-derive publication, focus and pager activity.
pub(super) fn flags_changed(shared: &PaneShared, previous: PaneFlags, flags: PaneFlags) {
    let now = browser::now_ms();
    let was_active = previous.view_active();
    let active = flags.view_active();
    with_state(shared, |state, actions| {
        state.backfill.set_active(active && state.page_visible);
        let mut host = ViewportCtx::new(shared, state_flags(state), actions);
        if was_active && !active {
            let transient = withdraw_is_transient_layout_gap(shared, flags, state.page_visible);
            state.viewport.withdraw(now, transient, &mut host);
        } else if active && (!was_active || previous.pending != flags.pending) {
            state.viewport.publish_now(now, &mut host);
        } else if active && previous.spotlit != flags.spotlit {
            state.viewport.schedule(now);
        }
    });
    super::paint::refresh_presentation(shared);
    super::interactions::sync_foreground(shared);
    super::input::focus_if_owner(shared, previous, flags);
}

/// Publish now, superseding a trailing publish.
pub(super) fn publish_viewport_now(shared: &PaneShared) {
    let now = browser::now_ms();
    with_state(shared, |state, actions| {
        let mut host = ViewportCtx::new(shared, state_flags(state), actions);
        state.viewport.publish_now(now, &mut host);
    });
}

/// The facts `ViewportCtx` reads, copied out so the state stays borrowable.
pub(super) fn state_flags(state: &PaneState) -> (PaneFlags, bool, bool) {
    (state.flags, state.page_visible, state.view_opened)
}

/// Every REAL withdraw is readable from the flags; what is left is `in_layout`
/// alone, and its transient form is a deck box that measures zero for a tick.
fn withdraw_is_transient_layout_gap(shared: &PaneShared, flags: PaneFlags, page_visible: bool) -> bool {
    !shared.disposed.get()
        && flags.surface_visible
        && flags.surface_active
        && page_visible
        && browser::deck_box_collapsed()
}

/// The viewport machine's view of the pane.
pub(super) struct ViewportCtx<'a> {
    shared: &'a PaneShared,
    flags: PaneFlags,
    page_visible: bool,
    actions: &'a mut Vec<PaneAction>,
}

impl<'a> ViewportCtx<'a> {
    pub(super) fn new(
        shared: &'a PaneShared,
        (flags, page_visible, _opened): (PaneFlags, bool, bool),
        actions: &'a mut Vec<PaneAction>,
    ) -> Self {
        Self {
            shared,
            flags,
            page_visible,
            actions,
        }
    }
}

impl ViewportHost for ViewportCtx<'_> {
    fn measure(&mut self) -> Option<(u32, u32)> {
        let cell = match self.shared.cell.get() {
            Some(cell) => cell,
            None => {
                let cell = measure_terminal_cell_box(self.shared.display.as_ref())?;
                self.shared.cell.set(Some(cell));
                cell
            }
        };
        let geometry = terminal_geometry_for_element(&self.shared.display, cell)?;
        Some((geometry.cols, geometry.rows))
    }
    fn should_publish_active(&self) -> bool {
        !self.shared.disposed.get() && !self.flags.pending && self.page_visible && self.flags.view_active()
    }
    fn has_view(&self) -> bool {
        !self.shared.disposed.get()
    }
    fn publish(&mut self, cols: u32, rows: u32) {
        self.actions.push(PaneAction::PublishView { cols, rows });
    }
    fn withdraw(&mut self) {
        self.actions.push(PaneAction::WithdrawView);
    }
    fn release_paint_holds(&mut self) {
        self.actions.push(PaneAction::ReleasePaintHolds);
    }
}

/// The DOM repair's view of the pane.
pub(super) struct DomRepairCtx<'a> {
    pub shared: &'a PaneShared,
    pub actively_viewed: bool,
    pub foreground_view_ready: bool,
    pub pointer_gesture_active: bool,
    pub catching_up: bool,
    pub actions: &'a mut Vec<PaneAction>,
}

impl DomRepairCtx<'_> {
    fn renderer_read<T>(&self, read: impl FnOnce(&roost_web_terminal::CellGridRenderer) -> T) -> Option<T> {
        self.shared.renderer.try_borrow().ok().map(|renderer| read(&renderer))
    }
}

impl DomRepairHost for DomRepairCtx<'_> {
    fn canonical(&self) -> Option<RendererEpochSeq> {
        self.renderer_read(|renderer| renderer.canonical_epoch_seq())
    }
    fn reconciled(&self) -> Option<RendererEpochSeq> {
        self.renderer_read(|renderer| renderer.reconciled_epoch_seq())
    }
    fn actively_viewed(&self) -> bool {
        self.actively_viewed
    }
    fn foreground_view_ready(&self) -> bool {
        self.foreground_view_ready
    }
    fn reader_hold_active(&self) -> bool {
        self.renderer_read(|renderer| preserves_foreground_reader_hold(renderer.reader_reason()))
            .unwrap_or(false)
    }
    fn pointer_gesture_active(&self) -> bool {
        self.pointer_gesture_active
    }
    fn catching_up(&self) -> bool {
        self.catching_up
    }
    fn repair_locally(&mut self) {
        self.actions.push(PaneAction::RepairLocally);
    }
    fn recover_unreconciled(&mut self, _watermark: &RendererEpochSeq) {
        self.actions.push(PaneAction::RecoverGeneration);
    }
}
