//! Keeping a granted, authenticated peer warm for every online worker with an
//! open session, before any pane asks for one, so a pane's direct route is one
//! staging round trip on a peer that already exists rather than a whole
//! negotiation. Owns the SELECTION — which workers, for which sessions, within
//! which budget — and nothing else: what a pre-warmed machine does is
//! `client::carriers::Signalling`'s. Called by `handle_sync` when the session
//! plane, the worker rows or the routable set change and when the document's
//! visibility does, and by the view handlers in `handle_terminal`.

use std::collections::{BTreeMap, BTreeSet};

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT, TERMINAL_PEER_MAX_SESSIONS_PER_GRANT,
};

use crate::effect::Effect;
use crate::store::{SessionStatus, Store};

/// Bring every worker's pre-warm in line with what the store says now.
///
/// A worker the selection stops naming is RELEASED while the document is
/// visible — a peer no view wants is closed, so its slot under the document's
/// cap goes to the worker that outranked it — and only CLEARED while the
/// document is hidden, which keeps the peer for when it is looked at again.
pub(crate) fn reconcile_prewarm(store: &mut Store, now_ms: u64, out: &mut Vec<Effect>) {
    let visible = store.sync.redial.visible();
    let selected = if visible {
        prewarm_selection(store)
    } else {
        BTreeMap::new()
    };
    let dropped: Vec<String> = store
        .direct
        .prewarmed_workers()
        .filter(|worker_fp| !selected.contains_key(*worker_fp))
        .map(str::to_owned)
        .collect();
    for worker_fp in dropped {
        if visible {
            store.direct.release_prewarm(&worker_fp, now_ms, out);
        } else {
            store.direct.clear_prewarm(&worker_fp, now_ms, out);
        }
    }
    for (worker_fp, session_ids) in selected {
        store
            .direct
            .set_prewarm(&worker_fp, session_ids, now_ms, out);
    }
}

/// The sessions each worker should be pre-warmed for.
///
/// Every routable worker with an open session is a candidate, most open
/// sessions first and then by fingerprint, and as many are taken as the
/// document's peer cap leaves once the workers views already hold are counted.
/// A worker with no open session is never one: the coordinator refuses a grant
/// that names none.
fn prewarm_selection(store: &Store) -> BTreeMap<String, BTreeSet<String>> {
    // The coordinator's own word on which workers it can route to. Before the
    // first routable set the only evidence is heartbeat freshness, which needs a
    // wall clock this core does not hold, so nothing is pre-warmed on a guess.
    let Some(routable) = store.routable_worker_fps.as_ref() else {
        return BTreeMap::new();
    };
    let mut open: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for session in store.sessions.sessions().values() {
        let worker_fp = session.worker_fp.as_str();
        if session.status == SessionStatus::Open
            && routable.contains(worker_fp)
            && store.workers.contains_key(worker_fp)
        {
            open.entry(worker_fp)
                .or_default()
                .insert(session.id.as_str());
        }
    }
    let viewed: BTreeSet<&str> = store.direct.workers_with_view_demand().collect();
    let mut selected = BTreeMap::new();
    // A worker some view already wants only ever SHEDS pre-warmed sessions
    // that closed. Every mint replaces the worker's whole scope, and the worker
    // closes each connection on a grant that lost a session — the view's own
    // route included — so a pre-warm that widened under a live view would cost
    // it its direct route. Shedding a closed session never mints.
    for &worker_fp in &viewed {
        let Some(held) = store.direct.prewarm_sessions(worker_fp) else {
            continue;
        };
        let still_open: BTreeSet<String> = held
            .iter()
            .filter(|id| {
                open.get(worker_fp)
                    .is_some_and(|ids| ids.contains(id.as_str()))
            })
            .cloned()
            .collect();
        if !still_open.is_empty() {
            selected.insert(worker_fp.to_owned(), still_open);
        }
    }
    let budget = TERMINAL_PEER_MAX_CONNECTIONS_PER_BROWSER_DOCUMENT.saturating_sub(viewed.len());
    let mut ranked: Vec<(&str, &BTreeSet<&str>)> = open
        .iter()
        .filter(|(worker_fp, _)| !viewed.contains(*worker_fp))
        .map(|(worker_fp, ids)| (*worker_fp, ids))
        .collect();
    ranked.sort_by(|(left_fp, left), (right_fp, right)| {
        right
            .len()
            .cmp(&left.len())
            .then_with(|| left_fp.cmp(right_fp))
    });
    for (worker_fp, ids) in ranked.into_iter().take(budget) {
        let session_ids = ids
            .iter()
            .take(TERMINAL_PEER_MAX_SESSIONS_PER_GRANT)
            .map(|id| (*id).to_owned())
            .collect();
        selected.insert(worker_fp.to_owned(), session_ids);
    }
    selected
}
