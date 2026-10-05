//! The acknowledgement path: a view-state result, and the result that arrived
//! inside a Sync frame.
//!
//! Split from the pane lifecycle because a lease is a different question from a
//! pane. This is what you read when the authority answered; `handle_terminal`
//! is what you read when a pane opened.
//!
//! Ported from `apps/web/src/store/terminal-stream-view-commands.ts`; contract
//! `protocol/spec/terminal-stream.md:24-26`.

use crate::effect::Effect;
use crate::handle_sweep::request_repair_if_due;
use crate::store::Store;
use crate::sync::SyncFrame;
use crate::terminal::session::ViewStateAdmission;
use crate::terminal::view::ViewStateResult;
use roost_protocol::viewport::{TerminalGeometry, is_terminal_geometry, is_terminal_uuid};

/// A generation-matched view-state result. An accepted answer carrying a stream id
/// installs the new expectation, which drops the baseline and clears the latch.
pub fn handle_view_state(
    store: &mut Store,
    state: &ViewStateResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(replica) = store.terminal_mut_if_present(&state.session_id) else {
        return;
    };
    // The result names the id the AUTHORITY holds, which after a promotion is not
    // the pane's own. Correlating on it directly would never find the record that
    // has to acknowledge the answer, and the lease would expire against an
    // authority that is holding it perfectly well.
    let Some(logical_view_id) = replica
        .logical_view_for_wire(&state.view_id)
        .map(str::to_string)
    else {
        tracing::debug!(
            target: "terminal",
            session_id = %state.session_id,
            view_id = %state.view_id,
            "view-state result for an id this replica does not hold"
        );
        return;
    };
    let translated = ViewStateResult {
        view_id: logical_view_id,
        ..state.clone()
    };
    match replica.apply_view_state(&translated, now_ms) {
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
            // v2 `terminal-stream-view-commands.ts`: the stream is installed at
            // the AUTHORITY's effective geometry, and an answer without a valid
            // one installs nothing.
            let geometry = TerminalGeometry {
                cols: state.effective_cols,
                rows: state.effective_rows,
            };
            if !is_terminal_uuid(&stream_id) || !is_terminal_geometry(&geometry) {
                tracing::warn!(target: "terminal", session_id = %state.session_id,
                    "accepted view state without a valid stream or geometry");
                return;
            }
            let (cols, rows) = (geometry.cols, geometry.rows);
            let changed = store
                .terminal_mut_if_present(&state.session_id)
                .is_some_and(|replica| replica.install_expected_stream(&stream_id, cols, rows));
            if changed {
                store.note_change();
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
            revision,
            accepted,
            stream_id,
            effective_cols,
            effective_rows,
        } => handle_view_state(
            store,
            &ViewStateResult {
                session_id: session_id.clone(),
                view_id: view_id.clone(),
                generation: *generation,
                revision: *revision,
                accepted: *accepted,
                stream_id: (!stream_id.is_empty()).then(|| stream_id.clone()),
                effective_cols: *effective_cols,
                effective_rows: *effective_rows,
            },
            now_ms,
            out,
        ),
        SyncFrame::InputResult {
            session_id,
            input_seq,
            outcome,
            ..
        } => crate::handle_input::handle_input_result(
            store, session_id, *input_seq, outcome, now_ms, out,
        ),
        _ => {}
    }
}
