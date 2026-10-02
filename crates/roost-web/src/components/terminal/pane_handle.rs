//! The component's handle on its imperative pane: `pane_mount::PaneMount` in a
//! browser, and nothing on the native target, where there is no document to
//! mount a renderer in. `cell_terminal` holds one per mounted `CellTerminal`
//! and forwards every prop change and pump revision through it. The seam
//! `apps/web/src/components/terminal/CellTerminal.tsx` crossed with `runtime`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use super::pane_mount::{PaneMount, PaneMountInit};
use super::pane_registry::PaneRegistry;
use super::pane_state::{PaneFlags, PaneUi};
use crate::pump::Pump;

/// What a mount needs besides the element.
#[derive(Debug, Clone)]
pub struct PaneMountRequest {
    pub session_id: String,
    pub worker_fp: String,
    pub title: String,
    pub flags: PaneFlags,
    pub ui: PaneUi,
    pub pump: Pump,
    pub panes: PaneRegistry,
    /// The router's handler, for the links this pane's terminal paints.
    pub navigate: EventHandler<String>,
}

/// The mounted pane, if any. Cheap to clone; every clone is the same handle.
#[derive(Clone, Default)]
pub struct PaneHandle {
    mount: Rc<RefCell<Option<PaneMount>>>,
}

impl PartialEq for PaneHandle {
    /// Two handles are the same handle when they share one mount cell, which is
    /// what `Rc::ptr_eq` asks. A component's props are diffed on this, and the
    /// only question the diff can answer about a handle is whether the pane
    /// behind it is the same pane: there is no value to compare and no
    /// generation, so two distinct cells are two panes however alike they
    /// print.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.mount, &other.mount)
    }
}

impl std::fmt::Debug for PaneHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneHandle")
            .field("mounted", &self.mount.borrow().is_some())
            .finish()
    }
}

impl PaneHandle {
    /// Mount the renderer in the display element. A second mount replaces the
    /// first, which drops (and so disposes) it.
    #[cfg(target_arch = "wasm32")]
    pub fn mount(&self, display: &MountedData, request: PaneMountRequest) {
        use dioxus::web::WebEventExt as _;

        let Some(element) = display.try_as_web_event() else {
            return;
        };
        let mounted = PaneMount::mount(
            &element,
            PaneMountInit {
                session_id: request.session_id,
                worker_fp: request.worker_fp,
                title: request.title,
                flags: request.flags,
                ui: request.ui,
                pump: request.pump,
                panes: request.panes,
                navigate: request.navigate,
            },
        );
        let previous = self.mount.replace(mounted);
        drop(previous);
    }

    /// Mount the renderer in the display element.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn mount(&self, _display: &MountedData, _request: PaneMountRequest) {}

    /// Unmount: the pane disposes its renderer, view and listeners.
    pub fn unmount(&self) {
        // Taken out of the cell first, so the mount's teardown runs with the
        // cell unborrowed.
        let _previous = self.mount.borrow_mut().take();
    }

    fn with_mount(&self, run: impl FnOnce(&PaneMount)) {
        if let Ok(mount) = self.mount.try_borrow()
            && let Some(mount) = mount.as_ref()
        {
            run(mount);
        }
    }

    /// The props changed.
    pub fn set_flags(&self, flags: PaneFlags) {
        self.with_mount(|mount| mount.set_flags(flags));
    }

    /// The session's title changed.
    pub fn set_title(&self, title: &str) {
        self.with_mount(|mount| mount.set_title(title));
    }

    /// The pump revision moved.
    pub fn sync_store(&self) {
        self.with_mount(|mount| mount.sync_store());
    }

    /// One named key through the encoder.
    pub fn dispatch_key(&self, key: &str) {
        self.with_mount(|mount| mount.dispatch_key(key));
    }

    /// Composed text, optionally submitted.
    pub fn send_text(&self, text: &str, submit: bool) {
        self.with_mount(|mount| mount.send_text(text, submit));
    }

    /// Text typed as-is, never framed as a paste.
    pub fn send_raw_text(&self, text: &str) {
        self.with_mount(|mount| mount.send_raw_text(text));
    }

    /// Give the keyboard to the pane.
    pub fn force_focus(&self) {
        self.with_mount(|mount| mount.force_focus());
    }

    /// Re-claim the view.
    pub fn retry_view(&self) {
        self.with_mount(|mount| mount.retry_view());
    }

    /// Publish the viewport now.
    pub fn publish_viewport_now(&self) {
        self.with_mount(|mount| mount.publish_viewport_now());
    }

    /// Show the find bar for this pane.
    pub fn open_find(&self) {
        self.with_mount(|mount| mount.open_find());
    }

    /// Hide the find bar and hand the keyboard back to the PTY.
    pub fn close_find(&self) {
        self.with_mount(|mount| mount.close_find());
    }

    /// Replace the find query.
    pub fn set_find_query(&self, query: &str) {
        self.with_mount(|mount| mount.set_find_query(query));
    }

    /// Move the active match, wrapping at both ends.
    pub fn step_find(&self, delta: i64) {
        self.with_mount(|mount| mount.step_find(delta));
    }

    /// Flip case sensitivity and re-search.
    pub fn toggle_find_case(&self) {
        self.with_mount(|mount| mount.toggle_find_case());
    }

    /// Flip regex mode and re-search.
    pub fn toggle_find_regex(&self) {
        self.with_mount(|mount| mount.toggle_find_regex());
    }
}

/// Natively there is no document, so no pane is ever mounted: the type is
/// uninhabited and every call on it is unreachable by construction.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub enum PaneMount {}

#[cfg(not(target_arch = "wasm32"))]
impl PaneMount {
    fn set_flags(&self, _flags: PaneFlags) {
        match *self {}
    }
    fn set_title(&self, _title: &str) {
        match *self {}
    }
    fn sync_store(&self) {
        match *self {}
    }
    fn dispatch_key(&self, _key: &str) {
        match *self {}
    }
    fn send_text(&self, _text: &str, _submit: bool) {
        match *self {}
    }
    fn send_raw_text(&self, _text: &str) {
        match *self {}
    }
    fn force_focus(&self) {
        match *self {}
    }
    fn retry_view(&self) {
        match *self {}
    }
    fn publish_viewport_now(&self) {
        match *self {}
    }
    fn open_find(&self) {
        match *self {}
    }
    fn close_find(&self) {
        match *self {}
    }
    fn set_find_query(&self, _query: &str) {
        match *self {}
    }
    fn step_find(&self, _delta: i64) {
        match *self {}
    }
    fn toggle_find_case(&self) {
        match *self {}
    }
    fn toggle_find_regex(&self) {
        match *self {}
    }
}
