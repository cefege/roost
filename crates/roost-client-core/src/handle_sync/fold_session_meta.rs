//! The session-keyed metadata folds: OSC title, last activity, viewers and
//! opaque presence notices.
//!
//! Called by `apply_frame` only. Ported from `apps/web/src/store/sync-frame.ts`
//! (`sessionPresence` 231-262, `terminalTitle` 263-269, `lastActivity`
//! 270-276). The coordinator is the only party that parses a title or stamps
//! activity, so these are plain replacements keyed by session.

use crate::store::Store;
use crate::store::sync_feeds::{PRESENCE_NOTICE_QUEUE_MAX, PresenceNotice};
use crate::sync::inbound::SessionViewer;

/// The coordinator-parsed OSC title for one session.
pub(super) fn fold_terminal_title(store: &mut Store, session_id: &str, title: &str) {
    if store.terminal_titles.get(session_id).map(String::as_str) == Some(title) {
        return;
    }
    store
        .terminal_titles
        .insert(session_id.to_owned(), title.to_owned());
    store.note_change();
    tracing::debug!(target: "sync", session_id, "terminal title folded");
}

/// The coordinator-stamped last-activity time for one session.
pub(super) fn fold_last_activity(store: &mut Store, session_id: &str, ts_ms: i64) {
    if store.last_activity_ms.get(session_id) == Some(&ts_ms) {
        return;
    }
    store.last_activity_ms.insert(session_id.to_owned(), ts_ms);
    store.note_change();
    tracing::trace!(target: "sync", session_id, ts_ms, "last activity folded");
}

/// Replace one session's viewer list.
pub(super) fn fold_session_viewers(store: &mut Store, session_id: &str, viewers: &[SessionViewer]) {
    if store
        .session_viewers
        .get(session_id)
        .is_some_and(|held| held.as_slice() == viewers)
    {
        return;
    }
    store
        .session_viewers
        .insert(session_id.to_owned(), viewers.to_vec());
    store.note_change();
    tracing::debug!(target: "sync", session_id, viewers = viewers.len(), "session viewers folded");
}

/// Queue one opaque presence notice for its session's presence handler,
/// dropping the oldest past the bound.
pub(super) fn fold_session_presence(
    store: &mut Store,
    session_id: &str,
    payload: &serde_json::Value,
) {
    if store.presence_notices.len() >= PRESENCE_NOTICE_QUEUE_MAX {
        store.presence_notices.pop_front();
        tracing::debug!(target: "sync", session_id, "presence notice queue full; oldest dropped");
    }
    store.presence_notices.push_back(PresenceNotice {
        session_id: session_id.to_owned(),
        payload: payload.clone(),
    });
    store.note_change();
    tracing::trace!(target: "sync", session_id, "presence notice queued");
}

/// Queue one command completion for the browser, retaining a bounded recent set.
pub(super) fn fold_command_finished(
    store: &mut Store,
    session_id: &str,
    exit_code: Option<i32>,
    duration_ms: u64,
    delivery_seq: u64,
) {
    store.command_finished_requests.push(
        crate::store::command_finished_requests::CommandFinishedRequest {
            session_id: session_id.to_owned(),
            exit_code,
            duration_ms,
            delivery_seq,
        },
    );
    store.note_change();
    tracing::debug!(target: "sync", session_id, delivery_seq, "command completion queued");
}
