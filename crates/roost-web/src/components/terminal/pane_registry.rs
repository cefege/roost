//! The session-keyed registry of mounted terminal panes: the one place a
//! reader outside a pane (dictation context, a tab-grid preview, the clipboard
//! sheet's paste) resolves a session to the renderer that paints it. Provided
//! once in `App` via context; every `CellTerminal` registers on mount and
//! unregisters on drop, and the latest mount wins.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use dioxus::prelude::*;

pub use super::pane_surface::PaneSurface;

/// The registry, cheap to clone: every clone is the same registry.
#[derive(Clone, Default)]
pub struct PaneRegistry {
    inner: Rc<RefCell<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    next_mount_id: u64,
    panes: BTreeMap<String, MountedPane>,
}

struct MountedPane {
    mount_id: u64,
    surface: Rc<dyn PaneSurface>,
    /// The pane's own paste path, so a surface outside the pane (the clipboard
    /// history sheet) pastes through the same multiline guard a Mod+Shift+V
    /// paste does instead of writing raw bytes past it.
    paste: RefCell<Option<PasteTarget>>,
}

/// A mounted pane's paste door.
pub type PasteTarget = Rc<dyn Fn(&str)>;

impl PartialEq for PaneRegistry {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for PaneRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.borrow();
        formatter
            .debug_struct("PaneRegistry")
            .field("panes", &state.panes.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl PaneRegistry {
    /// Register a mounted pane's surface and answer its mount id. A second
    /// mount of the same session replaces the first: the newest pane is the
    /// one painting.
    pub fn register(&self, session_id: &str, surface: Rc<dyn PaneSurface>) -> u64 {
        let mut state = self.inner.borrow_mut();
        state.next_mount_id += 1;
        let mount_id = state.next_mount_id;
        let replaced = state
            .panes
            .insert(
                session_id.to_owned(),
                MountedPane {
                    mount_id,
                    surface,
                    paste: RefCell::new(None),
                },
            )
            .is_some();
        tracing::debug!(target: "terminal", session_id, mount_id, replaced, "pane registered");
        mount_id
    }

    /// Unregister one mount. A stale mount (already replaced) removes nothing,
    /// so a pane unmounting after its successor mounted cannot orphan it.
    pub fn unregister(&self, session_id: &str, mount_id: u64) -> bool {
        let mut state = self.inner.borrow_mut();
        let owns = state
            .panes
            .get(session_id)
            .is_some_and(|pane| pane.mount_id == mount_id);
        if owns {
            state.panes.remove(session_id);
            tracing::debug!(target: "terminal", session_id, mount_id, "pane unregistered");
        }
        owns
    }

    /// Install the paste door for `session_id`'s current mount. A door for a
    /// mount that has since been replaced is ignored.
    pub fn set_paste_target(&self, session_id: &str, mount_id: u64, target: PasteTarget) {
        if let Some(pane) = self
            .inner
            .borrow()
            .panes
            .get(session_id)
            .filter(|pane| pane.mount_id == mount_id)
        {
            *pane.paste.borrow_mut() = Some(target);
        }
    }

    /// Paste `text` into `session_id`'s mounted pane through its own paste
    /// path. `false` when no pane for that session is mounted.
    pub fn paste_text(&self, session_id: &str, text: &str) -> bool {
        let target = self
            .inner
            .borrow()
            .panes
            .get(session_id)
            .and_then(|pane| pane.paste.borrow().clone());
        // Called outside the borrow: the paste may re-enter the registry.
        target.is_some_and(|paste| {
            paste(text);
            true
        })
    }

    /// The canonical viewport as text.
    pub fn viewport_text(&self, session_id: &str) -> Option<String> {
        self.surface(session_id)?.viewport_text()
    }

    /// The newest `max_rows` history rows as text.
    pub fn scrollback_text(&self, session_id: &str, max_rows: usize) -> Option<String> {
        self.surface(session_id)?.scrollback_text(max_rows)
    }

    /// The newest non-blank painted rows, for a tab-grid preview. `None` when
    /// no pane for the session is mounted: a session never warmed has no
    /// preview.
    pub fn preview_rows(&self, session_id: &str) -> Option<Vec<roost_protocol::cell::CellRow>> {
        self.surface(session_id)?.preview_rows()
    }

    /// The surface, cloned out so the registry is not borrowed while it runs:
    /// a surface read may re-enter the registry.
    fn surface(&self, session_id: &str) -> Option<Rc<dyn PaneSurface>> {
        self.inner
            .borrow()
            .panes
            .get(session_id)
            .map(|pane| Rc::clone(&pane.surface))
    }
}

/// The registry from context.
pub fn use_pane_registry() -> PaneRegistry {
    use_context::<PaneRegistry>()
}
