//! The terminal-side rules: views, leases, carriers, and find paging.
//!
//! Everything here ends in an `Effect`, never in I/O. A view that needs
//! republishing asks for a command; a carrier that retires asks for a repair. The
//! host performs them. The deadlines live in `handle_sweep`, and the input path
//! in `handle_input` — because input is the one path where getting it wrong writes
//! to somebody's shell twice.
//!
//! Three rules live here, read for three different reasons: a pane attaching is
//! in this file, an acknowledgement arriving is in `views`, and the attempt that
//! moves a pane from one transport to another is in `staging`.
//!
//! Ported from `apps/web/src/store/terminal-stream-view.ts` (leases) and
//! `apps/web/src/store/terminal-stream-transport.ts` (carriers).

mod carriers;
mod staging;
mod views;

use crate::effect::Effect;
use crate::handle_sweep::{publish_view, send_intent, send_intent_for_wire};
use crate::search::RawMatch;
use crate::store::Store;
use crate::terminal::view::ViewIntent;
pub use carriers::{handle_carrier_authenticated, handle_carrier_lost, handle_worker_retired};
pub use staging::{
    MintedViewId, begin_sync_view_rotation, cancel_staged_candidate,
    cancel_staged_candidate_and_restart, handle_view_id_minted, sweep_candidate_deadlines,
};
pub use views::{handle_correlated_result, handle_view_state};

/// A pane asking to attach, as the front end stated it.
///
/// A record rather than eight positional arguments: a caller that passes `view_id`
/// where `worker_fp` belongs still compiles, and the mistake would be a view
/// registered against the wrong worker.
#[derive(Debug, Clone, Copy)]
pub struct ViewOpen<'a> {
    /// The session the pane is for.
    pub session_id: &'a str,
    /// The worker that owns the PTY.
    pub worker_fp: &'a str,
    /// The pane's identity, stable for its life.
    pub view_id: &'a str,
    /// The pane's effective columns.
    pub cols: u32,
    /// The pane's effective rows.
    pub rows: u32,
}

/// A pane attached: create the replica, record the demand, publish the view.
pub fn handle_view_opened(
    store: &mut Store,
    opened: ViewOpen<'_>,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let ViewOpen {
        session_id,
        worker_fp,
        view_id,
        cols,
        rows,
    } = opened;
    let replica = store.terminal_mut(session_id, worker_fp);
    replica.open_view(view_id, cols, rows, now_ms);
    if store
        .routes
        .set_view_demand(worker_fp, session_id, view_id, true)
    {
        store.note_change();
        tracing::info!(
            target: "terminal",
            session_id,
            view_id,
            worker_fp,
            cols,
            rows,
            "terminal view opened"
        );
    }
    // The election is told about the view, not just the route table. Without
    // this the machine's `active_views` stays zero, `start` returns on its
    // first gate, and no grant is ever requested — a session that stays on
    // Sync for the whole life of the document with nothing wrong to find.
    store
        .direct
        .demand(session_id, worker_fp, view_id, true, out);
    // A pane that appeared while a candidate was staging invalidates that
    // attempt's snapshot of this session: the candidate is preparing a view set
    // that is no longer the set this document wants.
    cancel_staged_candidate_and_restart(store, session_id, now_ms, out);
    publish_view(store, session_id, view_id, now_ms, out);
}

/// A pane changed size. The authority mints a NEW stream id for this, so the
/// replica must not keep folding into the old one.
pub fn handle_view_resized(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    cols: u32,
    rows: u32,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let changed = store
        .terminal_mut_if_present(session_id)
        .is_some_and(|replica| replica.resize_view(view_id, cols, rows));
    if !changed {
        return;
    }
    store.note_change();
    tracing::info!(
        target: "terminal",
        session_id,
        view_id,
        cols,
        rows,
        "terminal view resized; a new stream is expected"
    );
    // The attempt snapshotted this pane's intent and revision before the
    // resize, so publishing under either of them now would be a command the
    // authority reads as a different intent than the one the pane holds.
    cancel_staged_candidate_and_restart(store, session_id, now_ms, out);
    publish_view(store, session_id, view_id, now_ms, out);
}

/// A pane was hidden: it keeps its place but stops constraining geometry.
pub fn handle_view_hidden(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let worker_fp = worker_of(store, session_id);
    // Park the view FIRST, so the revision read below is the one the Park
    // publishes under. Reading it without parking leaves the intent as
    // `Publish`, so hiding a view reads as no change at all and the revision
    // never moves — the idempotence rule applied to a view that was never
    // hidden.
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.hide_view(view_id);
    }
    store
        .routes
        .set_view_demand(&worker_fp, session_id, view_id, false);
    store
        .direct
        .demand(session_id, &worker_fp, view_id, false, out);
    store.note_change();
    let revision = store
        .terminal(session_id)
        .and_then(|replica| replica.view(view_id))
        .map(|view| view.revision);
    cancel_staged_candidate_and_restart(store, session_id, now_ms, out);
    if let Some(revision) = revision {
        send_intent(store, session_id, view_id, ViewIntent::Park, revision, out);
    }
}

/// A pane closed, or its authorization was lost. The view goes at once, with no
/// lease wait.
pub fn handle_view_closed(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    // The wire id is captured BEFORE the record goes: after a promotion it is not
    // the pane's own id, and the removal the authority has to act on is the one
    // it actually holds a lease for.
    let wire_view_id = store
        .terminal(session_id)
        .and_then(|replica| replica.wire_view_id(view_id))
        .map(str::to_string);
    let revision = store
        .terminal_mut_if_present(session_id)
        .and_then(|replica| replica.close_view(view_id));
    store.note_change();
    let worker_fp = worker_of(store, session_id);
    store
        .routes
        .set_view_demand(&worker_fp, session_id, view_id, false);
    store
        .direct
        .demand(session_id, &worker_fp, view_id, false, out);
    cancel_staged_candidate_and_restart(store, session_id, now_ms, out);
    if let (Some(wire_view_id), Some(revision)) = (wire_view_id, revision) {
        send_intent_for_wire(
            store,
            session_id,
            &wire_view_id,
            ViewIntent::Unpublish,
            revision,
            out,
        );
    }
}

/// One coordinator search page: validate the window, then fence every match to
/// the grid epoch the replica is currently on.
pub fn handle_search_page(
    store: &mut Store,
    session_id: &str,
    page: &crate::search::SearchPage,
    matches: &[RawMatch],
    before_row: Option<u32>,
) {
    let epoch = store
        .terminal(session_id)
        .and_then(|replica| replica.canonical())
        .map(|frame| frame.grid_epoch.clone())
        .unwrap_or_default();
    match crate::search::fence_page(matches, page, before_row, &epoch) {
        Some(fenced) => {
            tracing::info!(
                target: "search",
                session_id,
                matches = fenced.len(),
                "search page fenced"
            );
            store.find_results.insert(session_id.to_string(), fenced);
            store.note_change();
        }
        None => tracing::warn!(
            target: "search",
            session_id,
            "search page refused: its window is not the one the reader asked for"
        ),
    }
}

/// The worker a session's replica belongs to, or empty when it has no replica.
pub(super) fn worker_of(store: &Store, session_id: &str) -> String {
    store
        .terminal(session_id)
        .map(|replica| replica.worker_fp.clone())
        .unwrap_or_default()
}
