//! Pending closes: the undo window a session's close waits out.
//!
//! Clicking ✕ on a session row does not kill the PTY. The row hides at once, the
//! close waits five seconds, and a snackbar offers the way back. This queue owns
//! the WINDOW; the sweep turns each expired id into a `SessionsKill` effect
//! (`handle_close_kill.rs`), and the in-flight ledger here correlates the answer.
//!
//! Each close is independent: its own window, its own card, its own undo. Closing
//! a second tab does not touch the first tab's countdown, and re-clicking a row
//! that is already pending restarts only that row's window.
//!
//! This module is here rather than in the component layer because
//! `selectors.rs` filters pending closes out of the live lists. v2 keeps that
//! filter honest by reading a signal (`pendingClose.ts:60-65`); here the filter
//! reads the store, and a selector that could be handed a stale answer would put
//! a row back on screen that the user had just dismissed.
//!
//! Ported from `apps/web/src/lib/pendingClose.ts`, which no other slice owns.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::wire::SessionStatus;

use crate::store::Store;

/// How long a close waits before its kill is issued.
pub const UNDO_WINDOW_MS: u64 = 5_000;

/// What the undo card shows, snapshotted when the close was scheduled.
///
/// A SNAPSHOT, not a lookup: by the time the card renders the session may be
/// gone, and a card that reads "Terminal" where it said "vim api-gateway" is a
/// card the user cannot recognise.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloseLabels {
    /// The session's title at the moment of the close.
    pub terminal_name: String,
    /// The folder it ran in.
    pub folder: String,
    /// The machine it ran on.
    pub server: String,
}

/// One close waiting out its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingClose {
    /// The session being closed.
    pub session_id: String,
    /// What the undo card shows.
    pub labels: CloseLabels,
    /// The instant the window runs out.
    pub expires_at_ms: u64,
}

/// The queue, newest last.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PendingCloses {
    entries: BTreeMap<String, PendingClose>,
    /// Kills issued and not yet answered: call id → session id. A failed
    /// answer carries only its call id, and this is how it finds its session.
    kills_in_flight: BTreeMap<u64, String>,
    /// Sessions whose window ran out and whose kill was issued, still hidden
    /// until the Sync socket removes them. Without this the row and its tab
    /// came back for the round trip between the kill and the removal.
    closing: BTreeSet<String>,
}

impl PendingCloses {
    /// Nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a `SessionsKill` call awaiting its answer.
    pub fn begin_kill(&mut self, call_id: u64, session_id: String) {
        self.kills_in_flight.insert(call_id, session_id);
    }

    /// Settle a kill call; its session when `call_id` was one.
    pub fn take_kill(&mut self, call_id: u64) -> Option<String> {
        self.kills_in_flight.remove(&call_id)
    }

    /// How many closes are waiting.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is waiting.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The queue, by session id.
    pub fn entries(&self) -> impl Iterator<Item = &PendingClose> {
        self.entries.values()
    }

    /// Whether `session_id` is waiting out a close, or was closed and has not
    /// left the session plane yet.
    pub fn contains(&self, session_id: &str) -> bool {
        self.entries.contains_key(session_id) || self.closing.contains(session_id)
    }

    /// Show a closed session again: its kill failed, so it is still running.
    pub fn release_closing(&mut self, session_id: &str) -> bool {
        self.closing.remove(session_id)
    }

    /// Drop every entry, at a credential boundary.
    ///
    /// A pending close's kill targets a session the new credential may not own,
    /// and its undo restores a layout captured under the old one.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.closing.clear();
    }
}

/// Whether `session_id` is waiting out a close, and so must be hidden from the
/// live lists.
pub fn is_pending_close(store: &Store, session_id: &str) -> bool {
    store.pending_closes.contains(session_id)
}

/// Schedule a close: hide the row now, and owe a kill in `UNDO_WINDOW_MS`.
///
/// A second close for a session that is already pending REPLACES the first,
/// which restarts that row's window and keeps its card in place — the same shape
/// as v2's "re-click on hidden row" case, and the reason a row's countdown does
/// not jump backwards when a user clicks twice.
pub fn schedule_close(
    store: &mut Store,
    session_id: impl Into<String>,
    labels: CloseLabels,
    now_ms: u64,
) -> bool {
    let session_id = session_id.into();
    let entry = PendingClose {
        session_id: session_id.clone(),
        labels,
        expires_at_ms: now_ms.saturating_add(UNDO_WINDOW_MS),
    };
    if store.pending_closes.entries.get(&session_id) == Some(&entry) {
        return false;
    }
    store.pending_closes.entries.insert(session_id, entry);
    store.note_change();
    true
}

/// Restore one closed row, and hand back its labels.
///
/// The labels come back so the host can re-commit whatever the close undid — the
/// pane tiling — AFTER this write, which is the order
/// `pendingClose.ts:82-87` performs by hand and the reason it is not left to a
/// callback the store would have to keep alive.
pub fn undo_one(store: &mut Store, session_id: &str) -> Option<CloseLabels> {
    let entry = store.pending_closes.entries.remove(session_id)?;
    store.note_change();
    tracing::info!(target: "store", session_id, "close undone");
    Some(entry.labels)
}

/// Restore every closed row, oldest window first.
pub fn undo_all(store: &mut Store) -> Vec<CloseLabels> {
    if store.pending_closes.entries.is_empty() {
        return Vec::new();
    }
    let restored: Vec<CloseLabels> = store
        .pending_closes
        .entries
        .values()
        .map(|entry| entry.labels.clone())
        .collect();
    store.pending_closes.entries.clear();
    store.note_change();
    tracing::info!(target: "store", count = restored.len(), "every close undone");
    restored
}

/// Run the window deadline: take out every close whose time is up, and hand the
/// host the sessions it must kill.
///
/// Called by the sweep, and the return value is the whole point — the kill is a
/// Connect call, and a deadline that quietly dropped its ids on the floor would
/// leave a PTY the user believes they closed. The sweep issues one
/// `SessionsKill` per id, in the order returned.
pub fn sweep_pending_closes(store: &mut Store, now_ms: u64) -> Vec<String> {
    let gone: Vec<String> = store
        .pending_closes
        .closing
        .iter()
        .filter(|id| !session_is_open(store, id))
        .cloned()
        .collect();
    for session_id in &gone {
        store.pending_closes.closing.remove(session_id);
    }
    let due: Vec<String> = store
        .pending_closes
        .entries
        .values()
        .filter(|entry| now_ms >= entry.expires_at_ms)
        .map(|entry| entry.session_id.clone())
        .collect();
    if due.is_empty() {
        return Vec::new();
    }
    for session_id in &due {
        store.pending_closes.entries.remove(session_id);
        store.pending_closes.closing.insert(session_id.clone());
    }
    store.note_change();
    tracing::info!(target: "store", count = due.len(), "close windows expired");
    due
}

/// Whether the session plane still holds `session_id` as an open session.
fn session_is_open(store: &Store, session_id: &str) -> bool {
    crate::store::selectors::session_by_id(store, session_id)
        .is_some_and(|session| session.status == SessionStatus::Open)
}

/// Drop every pending close, at a credential boundary.
pub fn clear_pending_closes(store: &mut Store) {
    if store.pending_closes.entries.is_empty() && store.pending_closes.closing.is_empty() {
        return;
    }
    store.pending_closes.clear();
    store.note_change();
}
