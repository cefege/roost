//! The session-keyed registry of mounted terminal panes: the one place a
//! reader outside a pane (the smoke backdoor, a preview) resolves a session to
//! the renderer that paints it. Provided once in `App` via context; every
//! `CellTerminal` registers on mount and unregisters on drop, and the latest
//! mount wins. Ports the renderer registry of
//! `apps/web/src/renderer/terminalPreview.ts` (`registerRenderer`,
//! `rendererRegistryEntry`).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_web_terminal::{
    BackfillAnchor, PaintedRowText, ReaderIntent, ReconcileBlockReason, RendererEpochSeq,
    RendererPaintPresentation, RendererPresentationSnapshot,
};

pub use super::pane_surface::{PaintedLine, PaintedMarkerHit, PaneSurface, find_marker};

/// Per-session counters that outlive any one mount, as v2's module maps did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaneCounters {
    /// Epoch-addressed history reads the session's scrollback pagers issued.
    pub backfill_requests: u64,
    /// History pages an elected direct carrier served to those pagers.
    pub direct_history_responses: u64,
}

/// One render probe: watermarks and reader state of the mounted renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRenderProbe {
    /// How far canonical has advanced.
    pub canonical: RendererEpochSeq,
    /// How far the painted DOM has reconciled.
    pub reconciled: RendererEpochSeq,
    /// Live tail or parked reader.
    pub reader_intent: ReaderIntent,
    /// Whether the scroll box sits at its exact bottom.
    pub at_bottom: bool,
    /// Immutable history rows painted.
    pub painted_scrollback_rows: usize,
    /// Viewport row elements in the DOM.
    pub dom_rows: usize,
    /// Why the DOM is behind canonical, or `None` when it is not.
    pub reconcile_block_reason: ReconcileBlockReason,
    /// The painted history range and the epoch a history page must match.
    pub backfill_anchor: Option<BackfillAnchor>,
}

/// The registry, cheap to clone: every clone is the same registry.
#[derive(Clone, Default)]
pub struct PaneRegistry {
    inner: Rc<RefCell<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    next_mount_id: u64,
    panes: BTreeMap<String, MountedPane>,
    counters: BTreeMap<String, PaneCounters>,
}

struct MountedPane {
    mount_id: u64,
    surface: Rc<dyn PaneSurface>,
    /// The deck's last word about this pane. The browser snapshot reads it
    /// from here rather than parsing display CSS: "is this pane in the
    /// layout" and "is its surface the active one" are DECK facts, and a
    /// computed style is a report about the renderer, not the authority.
    flags: Cell<super::pane_state::PaneFlags>,
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
                    flags: Cell::new(super::pane_state::PaneFlags::default()),
                    paste: RefCell::new(None),
                },
            )
            .is_some();
        tracing::debug!(target: "terminal", session_id, mount_id, replaced, "pane registered");
        mount_id
    }

    /// Record the deck's current flags for this pane.
    ///
    /// Written from `PaneMount::set_flags`, which is the ONE place the deck's
    /// decision arrives: a mount that is not registered yet has no reader, and
    /// the first `set_flags` after registration seeds it.
    pub fn set_pane_flags(&self, session_id: &str, flags: super::pane_state::PaneFlags) {
        if let Some(pane) = self.inner.borrow_mut().panes.get_mut(session_id) {
            pane.flags.set(flags);
        }
    }

    /// The deck's last recorded flags for this pane, or `None` with no mount.
    #[must_use]
    pub fn pane_flags(&self, session_id: &str) -> Option<super::pane_state::PaneFlags> {
        self.inner
            .borrow()
            .panes
            .get(session_id)
            .map(|pane| pane.flags.get())
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

    /// The current mount's id, for a caller detecting a remount.
    pub fn mount_id(&self, session_id: &str) -> Option<u64> {
        self.inner
            .borrow()
            .panes
            .get(session_id)
            .map(|pane| pane.mount_id)
    }

    /// Every session with a mounted pane.
    pub fn sessions(&self) -> Vec<String> {
        self.inner.borrow().panes.keys().cloned().collect()
    }

    /// The canonical viewport as text.
    pub fn viewport_text(&self, session_id: &str) -> Option<String> {
        self.surface(session_id)?.viewport_text()
    }

    /// The newest `max_rows` history rows as text.
    pub fn scrollback_text(&self, session_id: &str, max_rows: usize) -> Option<String> {
        self.surface(session_id)?.scrollback_text(max_rows)
    }

    /// The first painted row containing `marker`.
    pub fn find_painted_marker(&self, session_id: &str, marker: &str) -> Option<PaintedMarkerHit> {
        find_marker(&self.surface(session_id)?.painted_lines()?, marker)
    }

    /// Watermarks and reader state of the mounted renderer.
    pub fn render_probe(&self, session_id: &str) -> Option<PaneRenderProbe> {
        let probe = self.surface(session_id)?.probe()?;
        Some(PaneRenderProbe {
            canonical: probe.canonical,
            reconciled: probe.reconciled,
            reader_intent: probe.reader_intent,
            at_bottom: probe.at_bottom,
            painted_scrollback_rows: probe.painted_scrollback_rows,
            dom_rows: probe.dom_rows,
            reconcile_block_reason: probe.reconcile_block_reason,
            backfill_anchor: probe.backfill_anchor,
        })
    }

    /// The painted window around the reader.
    pub fn paint_presentation(
        &self,
        session_id: &str,
        row_limit: Option<usize>,
    ) -> Option<RendererPaintPresentation> {
        self.surface(session_id)?.paint_presentation(row_limit)
    }

    /// Whether every row of `[start, end)` is painted.
    pub fn has_painted_scrollback_range(&self, session_id: &str, start: u32, end: u32) -> bool {
        self.surface(session_id)
            .is_some_and(|surface| surface.has_painted_scrollback_range(start, end))
    }

    /// The painted text of `[start, end)`.
    pub fn painted_scrollback_range(
        &self,
        session_id: &str,
        start: u32,
        end: u32,
    ) -> Option<Vec<PaintedRowText>> {
        self.surface(session_id)?
            .painted_scrollback_range(start, end)
    }

    /// The presentation snapshot.
    pub fn presentation_snapshot(&self, session_id: &str) -> Option<RendererPresentationSnapshot> {
        self.surface(session_id)?.presentation_snapshot()
    }

    /// The newest non-blank painted rows, for a tab-grid preview. `None` when
    /// no pane for the session is mounted: a session never warmed has no
    /// preview.
    pub fn preview_rows(&self, session_id: &str) -> Option<Vec<roost_protocol::cell::CellRow>> {
        self.surface(session_id)?.preview_rows()
    }

    /// The session's counters, zero when it never had a pane.
    pub fn counters(&self, session_id: &str) -> PaneCounters {
        self.inner
            .borrow()
            .counters
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    /// One history read issued by the session's pager.
    pub fn note_backfill_request(&self, session_id: &str) {
        self.inner
            .borrow_mut()
            .counters
            .entry(session_id.to_owned())
            .or_default()
            .backfill_requests += 1;
    }

    /// One history page served by the session's elected direct carrier.
    pub fn note_direct_history_response(&self, session_id: &str) {
        self.inner
            .borrow_mut()
            .counters
            .entry(session_id.to_owned())
            .or_default()
            .direct_history_responses += 1;
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
