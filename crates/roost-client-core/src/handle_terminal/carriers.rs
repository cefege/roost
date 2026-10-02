//! A carrier's whole life: it authenticated, it was displaced, it is gone.
//!
//! Split from the pane lifecycle because the two are read at different moments
//! and fail differently. This is what you read when a socket's state changed;
//! `handle_terminal` is what you read when a pane opened.
//!
//! Ported from `apps/web/src/store/terminal-stream-transport.ts` and
//! `apps/web/src/store/transport/local-terminal.ts`. Contract:
//! `protocol/spec/direct-terminal.md`.

use crate::client::carriers::DirectGrant;
use crate::effect::Effect;
use crate::store::Store;
use crate::terminal::routes::DirectCarrier;
use crate::terminal::token::TerminalTransport;

use super::staging::{release_cancelled, stage_admitted_sessions, stage_viewed_sessions};

/// A direct carrier authenticated, and the election is told.
///
/// Separate from the registration below because the two know different things:
/// the registry learns which token this connection presents, and the machine
/// only needs to know that a loopback carrier EXISTS — it is the fallback a
/// faulted peer hands the session to, and it was opened from the discovered door
/// with no negotiation a fault could have interrupted.
pub fn handle_carrier_authenticated(
    store: &mut Store,
    carrier: &DirectCarrier,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    register_carrier(store, carrier, out);
    stage_viewed_sessions(store, carrier, now_ms, out);
    if carrier.transport == TerminalTransport::Loopback {
        store.direct.loopback_staged(&carrier.worker_fp, true, out);
    }
}

/// A fresh grant arrived: the election learns it, and every live carrier to that
/// worker is widened to it and stages the sessions it newly admits.
///
/// v2 hands each new grant to the live connection (`presentGrant` →
/// `updateGrant`) and stages every demanded session on it, which is how a
/// second session on a worker joins the peer the first one already opened
/// instead of waiting on a connection that will never be negotiated again.
pub fn handle_grant_minted(
    store: &mut Store,
    grant: &DirectGrant,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    store.direct.grant_minted(grant.clone(), out);
    let widened =
        store
            .routes
            .widen_grant(&grant.worker_fp, &grant.worker_epoch, &grant.session_ids);
    for (carrier, added) in widened {
        store.note_change();
        tracing::info!(
            target: "route",
            connection_id = %carrier.connection_id,
            worker_fp = %carrier.worker_fp,
            added = added.len(),
            granted = carrier.granted_sessions.len(),
            "a refreshed grant widened a live direct carrier"
        );
        stage_admitted_sessions(store, &carrier, &added, now_ms, out);
    }
}

/// A pane opened: stage its session on the live carrier whose grant already
/// admits it, unless that carrier already holds the session's route.
///
/// v2 `TerminalPeerOwner.handleDemand` → `stageCurrentConnection`. The grant
/// names every session the document has demanded, so a session whose pane left
/// and came back — a navigation, then a layout that splits it in again — is
/// already admitted: the refreshed grant adds nothing, no carrier widens, and
/// without this the session stays on Sync beside a carrier that could serve it.
/// A loopback route is never traded for a peer, as in v2.
pub fn stage_opened_session(
    store: &mut Store,
    session_id: &str,
    now_ms: u64,
    out: &mut Vec<Effect>,
) {
    let Some(carrier) = store.routes.admitted_carrier(session_id) else {
        return;
    };
    let keeps_its_route = store.routes.route(session_id).is_some_and(|route| {
        route.connection_id == carrier.connection_id
            || (route.token.transport == TerminalTransport::Loopback
                && carrier.transport == TerminalTransport::Peer)
    });
    if keeps_its_route {
        return;
    }
    stage_admitted_sessions(store, &carrier, &[session_id.to_string()], now_ms, out);
}

/// A direct carrier authenticated. Returns whether it was admitted.
fn register_carrier(store: &mut Store, carrier: &DirectCarrier, out: &mut Vec<Effect>) -> bool {
    let registration = store.routes.register(carrier.clone());
    if !registration.accepted {
        tracing::warn!(
            target: "terminal",
            connection_id = %carrier.connection_id,
            worker_fp = %carrier.worker_fp,
            "direct carrier refused"
        );
        return false;
    }
    store.note_change();
    tracing::info!(
        target: "terminal",
        connection_id = %carrier.connection_id,
        worker_fp = %carrier.worker_fp,
        transport = carrier.transport.as_str(),
        displaced = registration.cancelled.len(),
        "direct carrier registered"
    );
    // A newcomer may take the candidate slot from an attempt that has published
    // nothing the other worker can keep: those ids are still leased over there
    // until they are released, and the newcomer is not the one who published them.
    release_cancelled(&registration.cancelled, out);
    true
}

/// A direct carrier closed or was displaced: retire exactly its routes, settle
/// exactly its batches, abandon exactly its attempts, and never replay either.
pub fn handle_carrier_lost(store: &mut Store, connection_id: &str, out: &mut Vec<Effect>) {
    let lost = store.routes.unregister(connection_id);
    // A connection that only ever STAGED holds no route, and `unregister` only
    // reports the active one. Its attempts still hold views on its worker, so they
    // are cancelled by connection id rather than by route.
    let abandoned = store.routes.cancel_candidates_for_connection(connection_id);
    if !abandoned.is_empty() {
        store.note_change();
    }
    release_cancelled(&abandoned, out);
    for lost in lost {
        crate::handle_input::retire_route(
            store,
            &lost.session_id,
            &lost.token,
            "direct carrier lost",
            out,
        );
        // The machine's whole view of a loopback carrier is "one is staged", so
        // the connection going is what clears it. Nothing else can: the socket
        // belongs to the host, and a faulted peer's fallback decision reads
        // this flag rather than the registry.
        if lost.token.transport == TerminalTransport::Loopback
            && let Some(worker_fp) = lost.token.worker_fp.clone()
        {
            store.direct.loopback_staged(&worker_fp, false, out);
        }
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
    let abandoned = store.routes.cancel_candidates_for_worker(worker_fp);
    if !abandoned.is_empty() {
        store.note_change();
    }
    release_cancelled(&abandoned, out);
    // A removed worker's machine must not outlive it: `retire` is terminal in
    // the grant lifecycle, and a machine that kept asking would turn one delete
    // into a request the coordinator can only refuse.
    store.direct.retire(worker_fp, out);
    // The sockets outlive the routes unless the host is told: a carrier is a
    // live object the host owns, and an operator who deleted the worker must
    // not find its terminal still accepting input.
    out.push(Effect::CloseDirectCarriers {
        worker_fp: worker_fp.to_owned(),
    });
}
