//! The ids a session is re-registering on Sync after an elected direct route
//! was lost.
//!
//! A rotation is the honest answer to "the carrier that held this pane's view is
//! gone, and the pane's id belonged to that carrier's worker": the Sync authority
//! has never heard of that id, and publishing it there would either be refused as
//! unknown or — worse — accepted while a dead lease still names it. So the pane
//! keeps its own identity, the rotation mints a NEW wire id per pane, and until
//! those answers arrive the former direct id is suppressed everywhere Sync could
//! otherwise publish it.
//!
//! Holds no timer. The deadline that ends a rotation is the view heartbeat in
//! `handle_sweep`, and the retry is the next terminal-ready transition, because
//! minting again on every sweep would be a loop rather than a retry.

use std::collections::BTreeMap;

use crate::terminal::token::TerminalToken;
use crate::terminal::view::ViewIntent;

/// One pane inside a rotation, and what it was last saying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationView {
    /// The intent to publish under the new id.
    pub intent: ViewIntent,
    /// The revision it was published under, so a snapshot is not mistaken for a
    /// change the reader made after the route went.
    pub source_revision: u64,
}

/// A session's pending move back onto Sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncViewRotation {
    /// The attempt every answer must name, so an answer that arrives after a
    /// newer rotation or a newer route has taken over is recognisably stale.
    pub attempt_id: u64,
    /// The direct route that was lost, kept for the log and for the case where
    /// the loss is reported rather than resolved.
    pub old_route_token: TerminalToken,
    /// The panes still awaiting an id, by their own identity.
    pub views: BTreeMap<String, RotationView>,
}

/// Whether one session is mid-rotation. Every Sync publication asks: a view whose
/// id belongs to a dead carrier must not be published there at all.
pub fn is_pending(store: &crate::store::Store, session_id: &str) -> bool {
    store.pending_sync_view_rotation.contains_key(session_id)
}
