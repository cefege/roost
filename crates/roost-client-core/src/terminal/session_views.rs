//! The view side of a session replica: the leases, their intents, and the
//! generation-matched acknowledgement that keeps one alive.
//!
//! Split out of `session` because the admission rules and the lease rules change
//! for different reasons and are read for different reasons: `session` is what
//! you read when a frame was refused, this is what you read when a pane opened.
//!
//! Ported from `apps/web/src/store/terminal-stream-view.ts` and
//! `apps/web/src/store/terminal-stream-view-commands.ts`. Contract:
//! `protocol/spec/terminal-stream.md:24-26`.

use crate::terminal::session::{TerminalSession, ViewStateAdmission};
use crate::terminal::view::{TerminalView, ViewIntent, ViewStateResult};

impl TerminalSession {
    /// A pane attached.
    pub fn open_view(&mut self, view_id: impl Into<String>, cols: u32, rows: u32, now_ms: u64) {
        let view = TerminalView::opened(view_id, cols, rows, now_ms);
        self.views.insert(view.view_id.clone(), view);
    }

    /// A pane changed size. Returns true when the size actually changed.
    ///
    /// A size change mints a NEW stream id at the authority, so the caller
    /// installs the new expectation and the replica waits for a fresh baseline.
    /// The pane does not get to keep folding into the old stream just because it
    /// already has a canonical for it.
    pub fn resize_view(&mut self, view_id: &str, cols: u32, rows: u32) -> bool {
        self.views
            .get_mut(view_id)
            .is_some_and(|view| view.resize(cols, rows))
    }

    /// A pane was hidden: it keeps its place in the authority's membership but
    /// stops constraining geometry.
    pub fn hide_view(&mut self, view_id: &str) {
        if let Some(view) = self.views.get_mut(view_id) {
            view.park();
        }
    }

    /// A pane closed, or authorization was lost. The view is removed at once,
    /// with no lease wait.
    pub fn close_view(&mut self, view_id: &str) {
        self.views.remove(view_id);
    }

    /// A view, for a host that repaints through it.
    pub fn view(&self, view_id: &str) -> Option<&TerminalView> {
        self.views.get(view_id)
    }

    /// Every view, for the heartbeat sweep.
    pub fn views(&self) -> &std::collections::BTreeMap<String, TerminalView> {
        &self.views
    }

    /// The view a resync should name: the first still counted, or the first at
    /// all. A resync is a view-scoped command — it names the geometry the
    /// baseline must match — so a session with no views has nobody to ask.
    pub fn repair_view(&self) -> Option<&TerminalView> {
        self.views
            .values()
            .find(|view| view.counted)
            .or_else(|| self.views.values().next())
    }

    /// The intent one view currently wants sent.
    pub fn view_intent(&self, view_id: &str) -> Option<ViewIntent> {
        self.views.get(view_id).map(|view| view.intent)
    }

    /// Record that a view's command was sent, and that it is now awaited.
    pub fn mark_view_published(&mut self, view_id: &str, generation: u64, now_ms: u64) {
        if let Some(view) = self.views.get_mut(view_id) {
            view.mark_published(generation, now_ms);
        }
    }

    /// Apply a generation-matched view-state result.
    ///
    /// A result for a generation this view is not awaiting is `Stale`: it is the
    /// answer to a command from a socket that has since been replaced, and
    /// letting it satisfy the current lease would keep a view alive on the
    /// authority's memory of a socket that no longer exists.
    pub fn apply_view_state(
        &mut self,
        result: &ViewStateResult,
        now_ms: u64,
    ) -> ViewStateAdmission {
        if result.session_id != self.session_id {
            return ViewStateAdmission::Stale;
        }
        let acknowledged = self
            .views
            .get_mut(&result.view_id)
            .is_some_and(|view| view.acknowledge(result.generation, now_ms));
        if !acknowledged {
            return ViewStateAdmission::Stale;
        }
        if result.accepted {
            ViewStateAdmission::Accepted {
                stream_id: result.stream_id.clone(),
            }
        } else {
            ViewStateAdmission::Refused
        }
    }
}
