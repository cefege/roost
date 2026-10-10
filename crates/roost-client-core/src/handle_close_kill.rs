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

/// Issue one delete or graceful terminal kill per expired close.
pub(crate) fn issue_due_kills(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    for tab_id in sweep_pending_closes(store, now_ms) {
        issue_kill(store, tab_id, false, out);
    }
}

fn issue_kill(store: &mut Store, tab_id: String, force: bool, out: &mut Vec<Effect>) {
    let call_id = store.next_call_id();
    store.pending_closes.begin_kill(call_id, tab_id.clone());
    if let Some(conversation_id) = tab_id.strip_prefix("agent:") {
        tracing::info!(target: "close", call_id, conversation_id, "deleting closed agent conversation");
        out.push(Effect::Rpc(RpcCall::AgentChatDelete {
            call_id,
            conversation_id: conversation_id.to_owned(),
        }));
    } else {
        tracing::info!(target: "close", call_id, session_id = %tab_id, force, "killing closed session");
        out.push(Effect::Rpc(RpcCall::SessionsKill {
            call_id,
            session_id: tab_id,
            force,
        }));
    }
}

/// Fold a close answer. Returns whether `result` answered a pending close.
pub(crate) fn settle_kill(
    store: &mut Store,
    result: &RpcResult,
    now_ms: u64,
    out: &mut Vec<Effect>,
) -> bool {
    let Some(tab_id) = store.pending_closes.take_kill(result.call_id()) else {
        return false;
    };
    if let RpcResult::SessionKillAnswered {
        accepted: false,
        force: false,
        ..
    } = result
        && !tab_id.starts_with("agent:")
    {
        tracing::info!(target: "close", session_id = %tab_id, "graceful kill refused; forcing");
        issue_kill(store, tab_id, true, out);
        return true;
    }
    if let RpcResult::Failed { call_id, error } = result {
        tracing::warn!(target: "close", tab_id = %tab_id, %error, "close failed");
        if store.pending_closes.release_closing(&tab_id) {
            store.note_change();
        }
        add_toast(
            store,
            ToastId::new(ToastSource::Rpc { call_id: *call_id }, tab_id),
            format!("Close failed: {error}"),
            ToastKind::Err,
            ToastOptions::plain(),
            now_ms,
        );
    } else if matches!(
        result,
        RpcResult::SessionKillAnswered {
            accepted: false,
            force: true,
            ..
        }
    ) {
        tracing::warn!(target: "close", tab_id = %tab_id, "forced kill refused");
        if store.pending_closes.release_closing(&tab_id) {
            store.note_change();
        }
    }
    true
}
