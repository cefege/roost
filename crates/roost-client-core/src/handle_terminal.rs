//! The terminal-side rules: views, leases, carriers, and find paging.
//!
//! Everything here ends in an `Effect`, never in I/O. A view that needs
//! republishing asks for a command; a carrier that retires asks for a repair. The
//! host performs them. The deadlines live in `handle_sweep`, and the input path
//! in `handle_input` — because input is the one path where getting it wrong writes
//! to somebody's shell twice.
//!
//! Ported from `apps/web/src/store/terminal-stream-view.ts` (leases) and
//! `apps/web/src/store/terminal-stream-transport.ts` (carriers).

use crate::effect::Effect;
use crate::handle_sweep::{publish_view, request_repair_if_due, send_intent};
use crate::search::RawMatch;
use crate::store::Store;
use crate::sync::SyncFrame;
use crate::terminal::ViewStateAdmission;
use crate::terminal::routes::DirectCarrier;
use crate::terminal::view::{ViewIntent, ViewStateResult};

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
    tracing::info!(
        target: "terminal",
        session_id,
        view_id,
        cols,
        rows,
        "terminal view resized; a new stream is expected"
    );
    publish_view(store, session_id, view_id, now_ms, out);
}

/// A pane was hidden: it keeps its place but stops constraining geometry.
pub fn handle_view_hidden(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    out: &mut Vec<Effect>,
) {
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.hide_view(view_id);
    }
    let worker_fp = worker_of(store, session_id);
    store
        .routes
        .set_view_demand(&worker_fp, session_id, view_id, false);
    send_intent(store, session_id, view_id, ViewIntent::Park, out);
}

/// A pane closed, or its authorization was lost. The view goes at once, with no
/// lease wait.
pub fn handle_view_closed(
    store: &mut Store,
    session_id: &str,
    view_id: &str,
    out: &mut Vec<Effect>,
) {
    if let Some(replica) = store.terminal_mut_if_present(session_id) {
        replica.close_view(view_id);
    }
    let worker_fp = worker_of(store, session_id);
    store
        .routes
        .set_view_demand(&worker_fp, session_id, view_id, false);
    send_intent(store, session_id, view_id, ViewIntent::Unpublish, out);
}

/// A generation-matched view-state result. An accepted answer carrying a stream id
/// installs the new expectation, which drops the baseline and clears the latch.
pub fn handle_view_state(
    store: &mut Store,
    state: &ViewStateResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(replica) = store.terminal(&state.session_id) else {
        return;
    };
    let geometry = replica
        .view(&state.view_id)
        .map(|view| (view.cols, view.rows));
    let Some(replica) = store.terminal_mut_if_present(&state.session_id) else {
        return;
    };
    match replica.apply_view_state(state, now_ms) {
        ViewStateAdmission::Stale => tracing::debug!(
            target: "terminal",
            session_id = %state.session_id,
            view_id = %state.view_id,
            "stale view-state result"
        ),
        ViewStateAdmission::Refused => tracing::warn!(
            target: "terminal",
            session_id = %state.session_id,
            view_id = %state.view_id,
            "the authority refused this view"
        ),
        ViewStateAdmission::Accepted {
            stream_id: Some(stream_id),
        } => {
            let (cols, rows) = geometry.unwrap_or((0, 0));
            let changed = store
                .terminal_mut_if_present(&state.session_id)
                .is_some_and(|replica| replica.install_expected_stream(&stream_id, cols, rows));
            if changed {
                tracing::info!(
                    target: "terminal",
                    session_id = %state.session_id,
                    stream_id = %stream_id,
                    "expecting a fresh baseline"
                );
            }
            request_repair_if_due(store, &state.session_id, now_ms, out);
        }
        ViewStateAdmission::Accepted { stream_id: None } => {}
    }
}

/// Apply a view-state or input result that arrived inside a Sync frame.
pub fn handle_correlated_result(
    store: &mut Store,
    frame: &SyncFrame,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    match frame {
        SyncFrame::ViewState {
            session_id,
            view_id,
            generation,
            accepted,
            ..
        } => handle_view_state(
            store,
            &ViewStateResult {
                session_id: session_id.clone(),
                view_id: view_id.clone(),
                generation: *generation,
                accepted: *accepted,
                stream_id: None,
            },
            now_ms,
            out,
        ),
        SyncFrame::InputResult {
            session_id,
            input_seq,
            outcome,
            ..
        } => crate::handle_input::handle_input_result(store, session_id, *input_seq, outcome),
        _ => {}
    }
}

/// A direct carrier authenticated.
pub fn handle_carrier_ready(store: &mut Store, carrier: &DirectCarrier) {
    if store.routes.register(carrier.clone()) {
        tracing::info!(
            target: "terminal",
            connection_id = %carrier.connection_id,
            worker_fp = %carrier.worker_fp,
            transport = carrier.transport.as_str(),
            "direct carrier registered"
        );
    } else {
        tracing::warn!(
            target: "terminal",
            connection_id = %carrier.connection_id,
            worker_fp = %carrier.worker_fp,
            "direct carrier refused"
        );
    }
}

/// A direct carrier closed or was displaced: retire exactly its routes, settle
/// exactly its batches, and never replay either.
pub fn handle_carrier_lost(store: &mut Store, connection_id: &str, out: &mut Vec<Effect>) {
    for lost in store.routes.unregister(connection_id) {
        crate::handle_input::retire_route(
            store,
            &lost.session_id,
            &lost.token,
            "direct carrier lost",
            out,
        );
    }
}

/// A worker is gone: its connections, routes, demand, and candidates all go.
pub fn handle_worker_retired(store: &mut Store, worker_fp: &str, out: &mut Vec<Effect>) {
    for lost in store.routes.retire_worker(worker_fp) {
        crate::handle_input::retire_route(
            store,
            &lost.session_id,
            &lost.token,
            "worker retired",
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
        }
        None => tracing::warn!(
            target: "search",
            session_id,
            "search page refused: its window is not the one the reader asked for"
        ),
    }
}

/// The worker a session's replica belongs to, or empty when it has no replica.
fn worker_of(store: &Store, session_id: &str) -> String {
    store
        .terminal(session_id)
        .map(|replica| replica.worker_fp.clone())
        .unwrap_or_default()
}
