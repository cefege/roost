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
use crate::terminal::token::TerminalToken;
use crate::terminal::view::{TerminalView, ViewAnswer, ViewIntent, ViewStateResult};

impl TerminalSession {
    /// A pane attached, or asked to be republished.
    ///
    /// A view already publishing this exact size is a renewal, not a new intent:
    /// it keeps its revision and lease state, as v2 `changeIntent` ignores an
    /// identical intent and `refresh` republishes the desired one unchanged.
    /// Re-opening a view id with any other intent continues its revision rather
    /// than restarting at 1: the authority remembers the old revision, and a
    /// lower one for the same handle is refused as stale.
    pub fn open_view(&mut self, view_id: impl Into<String>, cols: u32, rows: u32, now_ms: u64) {
        let mut view = TerminalView::opened(view_id, cols, rows, now_ms);
        if let Some(previous) = self.views.get(&view.view_id) {
            if previous.intent == view.intent {
                return;
            }
            view.revision = previous.revision + 1;
        }
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
    /// with no lease wait. Returns the revision its removal is published under,
    /// or `None` when this replica held no such view.
    pub fn close_view(&mut self, view_id: &str) -> Option<u64> {
        self.views.remove(view_id).map(|mut view| view.retire())
    }

    /// A view, for a host that repaints through it.
    pub fn view(&self, view_id: &str) -> Option<&TerminalView> {
        self.views.get(view_id)
    }

    /// A pane's record, for the caller that rewrites it in place.
    pub fn view_mut(&mut self, logical_view_id: &str) -> Option<&mut TerminalView> {
        self.views.get_mut(logical_view_id)
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

    /// Open a view for a STAGED candidate, under a wire id the candidate minted.
    ///
    /// The key stays the pane's own identity, because every local structure
    /// speaks that one; only the id the authority sees differs. Nothing here
    /// renews an existing view: a candidate's records are built once, at staging
    /// time, and a fresh attempt builds a fresh replica.
    pub fn open_prospective_view(
        &mut self,
        logical_view_id: &str,
        wire_view_id: String,
        intent: ViewIntent,
        revision: u64,
        now_ms: u64,
    ) {
        let view =
            TerminalView::published_as(logical_view_id, wire_view_id, intent, revision, now_ms);
        self.views.insert(view.view_id.clone(), view);
    }

    /// The pane whose authority-facing id is `wire_view_id`.
    ///
    /// The reverse of every outbound lookup, and needed because an inbound
    /// view-state names the id the authority holds: after a promotion the two
    /// differ, and a result correlated on the wire id alone would never find the
    /// record that has to acknowledge it.
    pub fn logical_view_for_wire(&self, wire_view_id: &str) -> Option<&str> {
        self.views
            .values()
            .find(|view| view.wire_view_id == wire_view_id)
            .map(|view| view.view_id.as_str())
    }

    /// The authority-facing id for one pane, or empty when this replica holds
    /// no such view.
    pub fn wire_view_id(&self, logical_view_id: &str) -> Option<&str> {
        self.view(logical_view_id)
            .map(|view| view.wire_view_id.as_str())
    }

    /// Record that a view's command was sent, and that it is now awaited.
    pub fn mark_view_published(&mut self, view_id: &str, generation: u64, now_ms: u64) {
        if let Some(view) = self.views.get_mut(view_id) {
            view.mark_published(generation, now_ms);
        }
    }

    /// Apply a generation-matched view-state result.
    ///
    /// Two kinds are admitted. The answer to an awaited command, matched on the
    /// generation the command went out on; and a state the authority broadcast
    /// to every live view when another viewer re-minted the stream, which
    /// answers no command and is matched instead on the view's CURRENT revision
    /// over this replica's own generation (v2 `dispatchTerminalViewState`: the
    /// frame's revision against `desired.revision`). Refusing the broadcast
    /// leaves the replica expecting the old stream and refusing every frame of
    /// the new one until the next heartbeat asks again.
    ///
    /// Anything else is `Stale`: the answer to a command from a socket that has
    /// since been replaced, and letting it satisfy the current lease would keep
    /// a view alive on the authority's memory of a socket that no longer exists.
    pub fn apply_view_state(
        &mut self,
        result: &ViewStateResult,
        now_ms: u64,
    ) -> ViewStateAdmission {
        if result.session_id != self.session_id {
            return ViewStateAdmission::Stale;
        }
        let own_generation = self.generation().map(TerminalToken::view_answer_generation);
        let Some(view) = self.views.get_mut(&result.view_id) else {
            return ViewStateAdmission::Stale;
        };
        let awaited = view.acknowledge(result.generation, now_ms);
        let broadcast = !awaited
            && result.revision == view.revision
            && own_generation == Some(result.generation);
        if !awaited && !broadcast {
            return ViewStateAdmission::Stale;
        }
        view.answer = Some(ViewAnswer {
            revision: view.revision,
            accepted: result.accepted,
        });
        if result.accepted {
            ViewStateAdmission::Accepted {
                stream_id: result.stream_id.clone(),
            }
        } else {
            ViewStateAdmission::Refused
        }
    }
}
