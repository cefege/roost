//! The kill a close owes once its undo window runs out, and what its answer does.
//!
//! Ported from v2 `apps/web/src/lib/closeSession.ts` `killAfterUndo`: after the
//! window, `SessionsKill`; a refused graceful kill is retried once with
//! `force`; a failed call logs and raises a `Close failed:` card. An accepted
//! kill needs nothing — the session's removal arrives on the Sync socket.

use crate::effect::{Effect, RpcCall, RpcResult};
use crate::store::Store;
use crate::store::pending_close::sweep_pending_closes;
use crate::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

/// Issue one graceful `SessionsKill` per close whose window has run out, in
/// the order the queue returns them.
pub(crate) fn issue_due_kills(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    for session_id in sweep_pending_closes(store, now_ms) {
        issue_kill(store, session_id, false, out);
    }
}

fn issue_kill(store: &mut Store, session_id: String, force: bool, out: &mut Vec<Effect>) {
    let call_id = store.next_call_id();
    store.pending_closes.begin_kill(call_id, session_id.clone());
    tracing::info!(target: "close", call_id, session_id = %session_id, force, "killing closed session");
    out.push(Effect::Rpc(RpcCall::SessionsKill {
        call_id,
        session_id,
        force,
    }));
}

/// Fold a `SessionsKill` answer. Returns whether `result` answered a kill.
pub(crate) fn settle_kill(
    store: &mut Store,
    result: &RpcResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) -> bool {
    let Some(session_id) = store.pending_closes.take_kill(result.call_id()) else {
        return false;
    };
    match result {
        RpcResult::SessionKillAnswered {
            accepted: false,
            force: false,
            ..
        } => {
            tracing::info!(target: "close", session_id = %session_id, "graceful kill refused; forcing");
            issue_kill(store, session_id, true, out);
        }
        RpcResult::Failed { call_id, error } => {
            tracing::warn!(target: "close", session_id = %session_id, %error, "close failed");
            // The session is still running, so it comes back on screen beside
            // the card that says why.
            if store.pending_closes.release_closing(&session_id) {
                store.note_change();
            }
            add_toast(
                store,
                ToastId::new(ToastSource::Rpc { call_id: *call_id }, session_id),
                format!("Close failed: {error}"),
                ToastKind::Err,
                ToastOptions::plain(),
                now_ms,
            );
        }
        RpcResult::SessionKillAnswered {
            accepted: false,
            force: true,
            ..
        } => {
            tracing::warn!(target: "close", session_id = %session_id, "forced kill refused");
            if store.pending_closes.release_closing(&session_id) {
                store.note_change();
            }
        }
        _ => {}
    }
    true
}
