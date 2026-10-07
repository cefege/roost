//! The imperative half of one terminal pane: the `CellGridRenderer` mounted in
//! the pane's display element, fed the store's canonical frame on the browser
//! frame after each replica revision, plus the pane's view lease, pager,
//! presentation and input. Constructed by `cell_terminal` on mount, dropped on
//! unmount; depends on `roost-web-terminal`, the pump and the pane registry.
//! Ports `apps/web/src/components/terminal/cell-terminal-renderer.ts` and the
//! runtime of `cell-terminal-runtime.ts`.

mod actions;
mod backfill_io;
mod browser;
mod clipboard;
mod cursor_report;
mod echo;
mod find_io;
mod input;
mod interactions;
mod link_targets;
mod paint;
mod paste_files;
mod prompt_jump;
mod scroll;

use crate::components::terminal_chrome::attachment_picker::ChosenFile;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use dioxus::prelude::EventHandler;
use dioxus::prelude::Signal;
use roost_client_core::ClientEvent;
use roost_web_terminal::CellGridRenderer;
use roost_web_terminal::backfill::ScrollbackBackfill;
use roost_web_terminal::cell_geometry::TerminalCellBox;
use roost_web_terminal::terminal_presentation::TerminalPresentationController;
use wasm_bindgen::JsCast as _;
use web_sys::HtmlElement;

use super::dom_repair::DomRepair;
use super::frame_feed::FrameFeed;
use super::offline_watch::OfflineWatch;
use super::pane_registry::PaneRegistry;
use super::pane_state::{PaneFlags, PaneUi};
use super::viewport_publication::ViewportPublication;
use crate::platform::browser::phase_marks::{PhaseName, mark_session_phase};
use crate::pump::Pump;

pub use actions::PaneAction;

/// What `cell_terminal` hands the mount.
pub struct PaneMountInit {
    pub session_id: String,
    pub worker_fp: String,
    pub title: String,
    pub flags: PaneFlags,
    pub ui: PaneUi,
    pub pump: Pump,
    pub panes: PaneRegistry,
    /// The router's handler. The pane is imperative and mounted from an event
    /// handler, where no Dioxus context is readable, and a terminal file link
    /// opens a route — so the handle travels in rather than being looked up.
    pub navigate: EventHandler<String>,
    pub staged_files: Signal<Vec<ChosenFile>>,
}

impl std::fmt::Debug for PaneMountInit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneMountInit")
            .field("session_id", &self.session_id)
            .field("worker_fp", &self.worker_fp)
            .finish_non_exhaustive()
    }
}

/// The terminal modes the last painted frame carried.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct FrameModes {
    pub cursor_keys_app: bool,
    pub bracketed_paste: bool,
    pub mouse_sgr: bool,
    pub focus_events: bool,
    pub mouse_tracking: roost_protocol::cell::MouseTracking,
}

/// Every native state machine the pane drives, behind one borrow.
pub(super) struct PaneState {
    pub flags: PaneFlags,
    pub page_visible: bool,
    pub feed: FrameFeed,
    pub viewport: ViewportPublication,
    pub presentation: TerminalPresentationController,
    pub dom_repair: DomRepair,
    pub offline: OfflineWatch,
    pub backfill: ScrollbackBackfill,
    pub find: find_io::PaneFind,
    pub prompt_seek: prompt_jump::PromptSeek,
    pub view_opened: bool,
    pub published: Option<(u32, u32)>,
    pub has_reconciled_frame: bool,
    pub last_painted_at_ms: Option<u64>,
    pub pointer_gestures: u32,
    pub follow_settle_due_ms: Option<u64>,
    pub cursor: Option<(u32, u32)>,
    pub cursor_poll: roost_web_terminal::scheduler::CursorPollTicker,
}

/// Everything one mounted pane owns.
pub(super) struct PaneShared {
    pub session_id: String,
    pub worker_fp: String,
    pub view_id: String,
    pub title: RefCell<String>,
    pub pump: Pump,
    pub panes: PaneRegistry,
    /// The router's handler, for the links this pane's terminal paints.
    pub navigate: EventHandler<String>,
    pub staged_files: Signal<Vec<ChosenFile>>,
    pub ui: PaneUi,
    pub display: HtmlElement,
    pub renderer: Rc<RefCell<CellGridRenderer>>,
    pub mount_id: Cell<u64>,
    /// This pane's slot in the pump's find-intent registry, so a global-search
    /// result clicked for this session reaches THIS pane and a pane that has
    /// unmounted is not called into.
    pub find_registration: Cell<u64>,
    pub cell: Cell<Option<TerminalCellBox>>,
    pub modes: Cell<FrameModes>,
    /// Set by the renderer's reconcile hooks, which run inside a paint and so
    /// may not borrow the pane; consumed after the paint returns.
    pub reconciled: Rc<Cell<bool>>,
    pub first_reconciled: Rc<Cell<bool>>,
    pub follow_settle_requested: Rc<Cell<bool>>,
    pub state: RefCell<PaneState>,
    pub input: RefCell<Option<roost_web_terminal::input::TerminalInputController>>,
    pub browser: RefCell<browser::PaneBrowser>,
    pub interactions: interactions::PaneInteractions,
    pub echo: RefCell<Option<echo::PaneEcho>>,
    pub disposed: Cell<bool>,
    /// This pane, for the async callbacks that must not keep it alive.
    me: Weak<PaneShared>,
}

impl PaneShared {
    /// A handle a deferred callback upgrades, finding nothing once dropped.
    pub fn weak_self(&self) -> Weak<PaneShared> {
        self.me.clone()
    }
}

/// One mounted pane. Dropping it disposes it.
pub struct PaneMount {
    shared: Rc<PaneShared>,
}

impl std::fmt::Debug for PaneMount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneMount")
            .field("session_id", &self.shared.session_id)
            .field("view_id", &self.shared.view_id)
            .finish_non_exhaustive()
    }
}

impl PaneMount {
    /// Mount the renderer inside `display` and start the pane.
    pub fn mount(display: &web_sys::Element, init: PaneMountInit) -> Option<Self> {
        let display: HtmlElement = display.clone().dyn_into().ok()?;
        let reconciled = Rc::new(Cell::new(false));
        let first_reconciled = Rc::new(Cell::new(false));
        let follow_settle_requested = Rc::new(Cell::new(false));
        let renderer = {
            let (first, every, settle) = (
                Rc::clone(&first_reconciled),
                Rc::clone(&reconciled),
                Rc::clone(&follow_settle_requested),
            );
            CellGridRenderer::with_callbacks(
                display.as_ref(),
                Some(Box::new(move || first.set(true))),
                Some(Box::new(move || every.set(true))),
                Some(Box::new(move || settle.set(true))),
            )
        };
        let renderer = match renderer {
            Ok(renderer) => Rc::new(RefCell::new(renderer)),
            Err(error) => {
                tracing::error!(target: "terminal", session_id = %init.session_id, ?error,
                    "diag.corruption_signal cell_mount_failed");
                return None;
            }
        };
        let Some(view_id) = crate::platform::terminal_view_id::mint_view_id() else {
            tracing::error!(target: "terminal", session_id = %init.session_id,
                "no crypto.randomUUID: a terminal view cannot be minted");
            return None;
        };
        let page_visible = browser::page_visible();
        let shared = Rc::new_cyclic(|me| PaneShared {
            session_id: init.session_id.clone(),
            worker_fp: init.worker_fp,
            view_id,
            title: RefCell::new(init.title),
            pump: init.pump,
            panes: init.panes,
            ui: init.ui,
            navigate: init.navigate,
            staged_files: init.staged_files,
            display,
            renderer: Rc::clone(&renderer),
            mount_id: Cell::new(0),
            cell: Cell::new(None),
            modes: Cell::new(FrameModes::default()),
            reconciled,
            first_reconciled,
            follow_settle_requested,
            state: RefCell::new(PaneState {
                // Default, so the first `set_flags` below is an activation edge.
                flags: PaneFlags::default(),
                page_visible,
                feed: FrameFeed::new(),
                viewport: ViewportPublication::new(),
                presentation: TerminalPresentationController::new(),
                dom_repair: DomRepair::new(),
                offline: OfflineWatch::new(),
                backfill: ScrollbackBackfill::new(&init.session_id),
                find: find_io::PaneFind::new(
                    &init.session_id,
                    &crate::platform::terminal_view_id::mint_view_id().unwrap_or_default(),
                ),
                prompt_seek: prompt_jump::PromptSeek::default(),
                view_opened: false,
                published: None,
                has_reconciled_frame: false,
                last_painted_at_ms: None,
                pointer_gestures: 0,
                follow_settle_due_ms: None,
                cursor: None,
                cursor_poll: roost_web_terminal::scheduler::CursorPollTicker::new(),
            }),
            input: RefCell::new(None),
            browser: RefCell::new(browser::PaneBrowser::default()),
            interactions: interactions::PaneInteractions::default(),
            echo: RefCell::new(None),
            disposed: Cell::new(false),
            me: me.clone(),
            find_registration: Cell::new(0),
        });
        let surface: Rc<dyn super::pane_surface::PaneSurface> = renderer;
        shared
            .mount_id
            .set(shared.panes.register(&shared.session_id, surface));
        let paste_into = shared.weak_self();
        shared.panes.set_paste_target(
            &shared.session_id,
            shared.mount_id.get(),
            Rc::new(move |text: &str| {
                if let Some(shared) = paste_into.upgrade().filter(|pane| !pane.disposed.get()) {
                    input::paste_text(&shared, text);
                }
            }),
        );
        browser::attach(&shared);
        input::attach(&shared);
        interactions::attach(&shared);
        echo::attach(&shared);
        shared
            .find_registration
            .set(shared.pump.register_terminal_find(
                &shared.session_id,
                Box::new(find_io::PaneFindSink::new(&shared)),
            ));
        let poll_due = shared
            .state
            .borrow_mut()
            .cursor_poll
            .register(cursor_report::CURSOR_POLL_PANE, browser::now_ms());
        tracing::debug!(target: "terminal", session_id = %shared.session_id, ?poll_due, "cursor poll registered");
        tracing::info!(target: "terminal", session_id = %shared.session_id,
            view_id = %shared.view_id, "terminal_mount");
        mark_session_phase(PhaseName::TerminalMount, &shared.session_id);
        let mount = Self { shared };
        mount.set_flags(init.flags);
        mount.sync_store();
        Some(mount)
    }

    /// The props changed.
    pub fn set_flags(&self, flags: PaneFlags) {
        let shared = &self.shared;
        let previous = {
            let mut state = shared.state.borrow_mut();
            std::mem::replace(&mut state.flags, flags)
        };
        shared.panes.set_pane_flags(&shared.session_id, flags);
        actions::flags_changed(shared, previous, flags);
    }

    /// The session's title changed: rename the grid and the textarea.
    pub fn set_title(&self, title: &str) {
        if *self.shared.title.borrow() == title {
            return;
        }
        *self.shared.title.borrow_mut() = title.to_owned();
        self.shared
            .renderer
            .borrow_mut()
            .set_accessible_label(&format!("Terminal — {title}"));
        if let Some(controller) = self.shared.input.borrow().as_ref() {
            controller.set_accessible_label(&format!("Terminal input — {title}"));
        }
    }

    /// The pump revision moved: note a replica advance and re-read the lease.
    pub fn sync_store(&self) {
        paint::sync_store(&self.shared);
    }

    /// Send one named key through the textarea encoder (the key sheet).
    pub fn dispatch_key(&self, key: &str) {
        input::dispatch_named_key(&self.shared, key);
    }

    /// Send composed text, framed like a paste, optionally submitted.
    pub fn send_text(&self, text: &str, submit: bool) {
        input::send_text(&self.shared, text, submit);
    }

    /// Send text as typed bytes, never framed as a paste.
    pub fn send_raw_text(&self, text: &str) {
        input::send_bytes(&self.shared, text.as_bytes().to_vec(), false);
    }

    /// Type text as the keyboard would, spending a latched Ctrl on it.
    pub fn type_text(&self, text: &str) {
        input::on_controller_data(&self.shared, text);
    }

    /// Paste text through the multiline guard.
    pub fn paste_text(&self, text: &str) {
        input::paste_text(&self.shared, text);
    }

    /// Give the keyboard to the pane's textarea.
    pub fn force_focus(&self) {
        if let Some(controller) = self.shared.input.borrow().as_ref() {
            controller.force_focus();
        }
    }

    /// Re-claim the view: the offline notice's retry.
    pub fn retry_view(&self) {
        actions::perform(&self.shared, vec![PaneAction::RepublishView]);
    }

    /// Publish the viewport now, e.g. after the find bar changed the rows.
    pub fn publish_viewport_now(&self) {
        actions::publish_viewport_now(&self.shared);
    }

    /// Leave history and follow the live tail: the jump-to-bottom button.
    pub fn jump_to_live(&self) {
        scroll::jump_to_live(&self.shared);
    }
}

impl Drop for PaneMount {
    fn drop(&mut self) {
        let shared = &self.shared;
        shared.disposed.set(true);
        echo::detach(shared);
        interactions::detach(shared);
        input::detach(shared);
        browser::detach(shared);
        find_io::dispose(shared);
        let backfill_actions = shared.state.borrow_mut().backfill.dispose();
        drop(backfill_actions);
        {
            let mut state = shared.state.borrow_mut();
            state.offline.dispose();
            state.dom_repair.clear_dom_stall_recovery();
            let mut renderer = shared.renderer.borrow_mut();
            state.presentation.dispose(Some(&mut *renderer));
            renderer.dispose();
        }
        shared
            .panes
            .unregister(&shared.session_id, shared.mount_id.get());
        shared
            .pump
            .unregister_terminal_find(&shared.session_id, shared.find_registration.get());
        shared.pump.dispatch(ClientEvent::ViewClosed {
            session_id: shared.session_id.clone(),
            view_id: shared.view_id.clone(),
        });
        tracing::info!(target: "terminal", session_id = %shared.session_id,
            view_id = %shared.view_id, "terminal pane unmounted");
    }
}
