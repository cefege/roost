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
mod input;
mod paint;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

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
    pub view_opened: bool,
    pub published: Option<(u32, u32)>,
    pub has_reconciled_frame: bool,
    pub last_painted_at_ms: Option<u64>,
    pub pointer_gestures: u32,
    pub follow_settle_due_ms: Option<u64>,
    pub cursor: Option<(u32, u32)>,
    pub cursor_sent: Option<(u32, u32)>,
}

/// Everything one mounted pane owns.
pub(super) struct PaneShared {
    pub session_id: String,
    pub worker_fp: String,
    pub view_id: String,
    pub title: RefCell<String>,
    pub pump: Pump,
    pub panes: PaneRegistry,
    pub ui: PaneUi,
    pub display: HtmlElement,
    pub renderer: Rc<RefCell<CellGridRenderer>>,
    pub mount_id: Cell<u64>,
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
        let view_id = format!("view-{:016x}", js_sys::Math::random().to_bits());
        let page_visible = browser::page_visible();
        let shared = Rc::new_cyclic(|me| PaneShared {
            session_id: init.session_id.clone(),
            worker_fp: init.worker_fp,
            view_id,
            title: RefCell::new(init.title),
            pump: init.pump,
            panes: init.panes,
            ui: init.ui,
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
                view_opened: false,
                published: None,
                has_reconciled_frame: false,
                last_painted_at_ms: None,
                pointer_gestures: 0,
                follow_settle_due_ms: None,
                cursor: None,
                cursor_sent: None,
            }),
            input: RefCell::new(None),
            browser: RefCell::new(browser::PaneBrowser::default()),
            disposed: Cell::new(false),
            me: me.clone(),
        });
        let surface: Rc<dyn super::pane_surface::PaneSurface> = renderer;
        shared
            .mount_id
            .set(shared.panes.register(&shared.session_id, surface));
        browser::attach(&shared);
        input::attach(&shared);
        tracing::info!(target: "terminal", session_id = %shared.session_id,
            view_id = %shared.view_id, "terminal_mount");
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
}

impl Drop for PaneMount {
    fn drop(&mut self) {
        let shared = &self.shared;
        shared.disposed.set(true);
        input::detach(shared);
        browser::detach(shared);
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
        shared.pump.dispatch(ClientEvent::ViewClosed {
            session_id: shared.session_id.clone(),
            view_id: shared.view_id.clone(),
        });
        tracing::info!(target: "terminal", session_id = %shared.session_id,
            view_id = %shared.view_id, "terminal pane unmounted");
    }
}
