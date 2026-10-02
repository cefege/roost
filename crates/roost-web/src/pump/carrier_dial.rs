//! Spending a minted grant: resolve the door, open the socket, admit the
//! `Ready`, and drain what the carrier says for the rest of its life.
//!
//! Owned by `pump`, called by `pump::carriers::request_grant` with the answer
//! the coordinator gave, and by the socket's own notify once it is open. It owns
//! the browser half and nothing else: which door answers is
//! `client::local::discovery::DoorDiscovery`, whether a `Ready` is admissible is
//! `client::local::door::admit_ready`, what a frame becomes is
//! `client::carriers::inbound`, and what it touches is the core's
//! `handle_direct_frame`.
//!
//! Two properties are the reason the lifecycle is written out rather than left
//! implicit:
//!
//! - **A credential is spent or reported.** Every exit from this module — no
//!   door, a foreign door, a socket the browser refused, a `Ready` the admission
//!   rule rejected, a close — is a named `tracing` line carrying the worker, so
//!   a mint that produced nothing spendable is visible as exactly that.
//! - **A carrier is announced once, and lost once.** Admission dispatches
//!   `CarrierReady` under the connection id the socket was opened with, and the
//!   close path dispatches `CarrierLost` under that same id, so the core's route
//!   registry can never hold a route for a socket that is gone.
//!
//! NOTHING IS HELD ACROSS AN `await`. The door probe is a network round trip and
//! the pump's carriers live in a `RefCell`; a borrow taken before the probe and
//! released after it is a panic the first time a socket callback fires in between,
//! and a panic in a socket callback is a carrier this document never reports.
//!
//! Ported from `apps/web/src/store/transport/local-terminal.ts:126-270`
//! (`start`, `sendFrame`, `finish`).

use std::rc::Rc;

use roost_client_core::ClientEvent;
use roost_client_core::TerminalTransport;
use roost_client_core::client::carriers::DirectInbound;
use roost_client_core::client::local::LocalTerminalGrant;
use roost_client_core::client::local::bootstrap::{
    BootstrapOutcome, LOCAL_BOOTSTRAP_PATH, LocalBootstrap, read_serving_origin,
};
use roost_client_core::client::local::discovery::{
    BrowserEnvironment, DoorAdoption, DoorPlan, LocalWorkerDoor,
};
use roost_client_core::client::local::door::LoopbackReady;

use super::Pump;
use super::carriers::{Pending, report_mint_refusal};
use crate::platform::carriers::dial::{self, DialFault, DialPlan};
use crate::platform::carriers::{CarrierFault, LoopbackConnection};
use crate::platform::door_probe;
use crate::platform::loopback::{LoopbackMessage, open_loopback_socket};

mod grant_refresh;

/// Spend one minted grant on a loopback carrier, or say why it was not spent.
///
/// Takes the grant rather than the coordinator's answer: the election is
/// handed the same object from `pump::carriers::request_grant`, and two
/// constructions of one reply is a second copy of the deadline, the scope and
/// the worker epoch that could disagree with the one the election acted on.
///
/// One loopback connection per worker: `grant_refresh` decides whether this
/// grant needs a socket at all.
pub(super) fn dial(pump: &Pump, grant: LocalTerminalGrant) {
    if !grant_refresh::spends_new_socket(pump, &grant) {
        return;
    }
    let worker_fp = grant.worker_fp.clone();
    let session_ids = grant.session_ids.clone();
    let sessions = session_ids.iter().cloned().collect::<Vec<_>>().join(",");
    let worker = worker_fp.clone();
    let opening = pump.clone();
    wasm_bindgen_futures::spawn_local(async move {
        open(&opening, &grant, &worker, &sessions).await;
    });
}

/// Resolve the door, then open the socket on the grant.
async fn open(pump: &Pump, grant: &LocalTerminalGrant, worker_fp: &str, sessions: &str) {
    let door = resolve_door(pump).await;
    // The ANSWER, reported the moment discovery settles and not the socket:
    // `LoopbackProbe::permits_peer` refuses a peer until this arrives, so a
    // document that discovers its own worker and never says so allocates a
    // WebRTC peer beside a loopback carrier that was available all along.
    // Empty is the honest value for "no worker serves this machine", and it is
    // what releases a peer rather than what blocks one.
    pump.dispatch(ClientEvent::LocalDoorAnswered {
        worker_fp: worker_fp.to_owned(),
        serving_worker_fp: door
            .as_ref()
            .map(|door| door.worker_fingerprint.clone())
            .unwrap_or_default(),
    });
    let (origin, url, hello, door_worker_fp) = match dial::plan(grant, door.as_ref()) {
        Ok(DialPlan { origin, url, hello }) => {
            let door_worker_fp = door.map(|door| door.worker_fingerprint).unwrap_or_default();
            (origin, url, hello, door_worker_fp)
        }
        // No door for THIS worker is not a refusal of the grant: the same
        // credential is what a WebRTC peer authenticates on, and v2 never builds
        // a loopback controller without a matching door, so its grant survives
        // to the peer. Refusing it here drops the grant the peer is about to
        // spend its Hello on.
        Err(fault @ (DialFault::NoDoor { .. } | DialFault::ForeignDoor { .. })) => {
            tracing::info!(
                target: "carriers",
                worker_fp,
                detail = %fault,
                "no loopback carrier for this worker; the grant is left to a peer"
            );
            return;
        }
        Err(fault) => {
            report_mint_refusal(pump, worker_fp, sessions, &fault.to_string());
            return;
        }
    };
    let connection_id =
        crate::platform::carrier::CarrierIdentity::mint(worker_fp, TerminalTransport::Loopback)
            .connection_id;
    let notify: Rc<dyn Fn()> = {
        let pump = pump.clone();
        let id = connection_id.clone();
        Rc::new(move || schedule_drain(&pump, &id))
    };
    match open_loopback_socket(connection_id.clone(), &origin, &hello, notify) {
        Ok(handle) => {
            let mut held = pump.inner.carriers.borrow_mut();
            held.expect_ready(connection_id, handle, grant.clone(), door_worker_fp);
            drop(held);
            tracing::info!(
                target: "carriers",
                worker_fp,
                origin = %origin,
                url = %url,
                "loopback carrier dialling on a minted grant"
            );
        }
        Err(error) => report_mint_refusal(pump, worker_fp, sessions, &error.to_string()),
    }
}

/// The door this page can dial, running discovery at most once.
///
/// Memoization is the core's, not a flag here: `DoorDiscovery` refuses a second
/// `start` precisely so a page that mints for two panes probes once, and a
/// second memo in the host would be a second answer to "has this page asked".
async fn resolve_door(pump: &Pump) -> Option<LocalWorkerDoor> {
    if let Some(door) = pump.inner.carriers.borrow().door().cloned() {
        return Some(door);
    }
    let page_origin = door_probe::page_origin();
    let served_by_worker = served_by_this_page(&page_origin).await;
    let operator_origin = door_probe::stored_operator_origin();
    let plan = pump
        .inner
        .carriers
        .borrow_mut()
        .doors()
        .start(&BrowserEnvironment {
            page_origin: page_origin.clone(),
            served_by_worker,
            operator_origin,
        });
    match plan {
        DoorPlan::Adopting(door) => Some(door),
        DoorPlan::Probe { origin, url } => {
            let answer = door_probe::fetch_bootstrap(&url).await;
            let adoption = pump.inner.carriers.borrow_mut().doors().complete_probe(
                &origin,
                answer.status,
                &answer.body,
            );
            match adoption {
                DoorAdoption::Adopted(door) => Some(door),
                DoorAdoption::Absent(absence) => {
                    tracing::info!(
                        target: "carriers",
                        origin = %origin,
                        reason = absence.reason(),
                        "no worker door on this machine; the session stays on Sync"
                    );
                    None
                }
            }
        }
        DoorPlan::NotProbed(absence) => {
            tracing::info!(
                target: "carriers",
                reason = absence.reason(),
                "no worker door probe on this page; the session stays on Sync"
            );
            None
        }
        // `start` refuses a second call, and this page has just made its first
        // with no door behind it, so the adoption arrived on a path that already
        // recorded it. The table is the answer.
        DoorPlan::AlreadyAttempted => pump.inner.carriers.borrow().door().cloned(),
    }
}

/// The bootstrap this page's OWN origin serves, if it serves one.
///
/// Asked before discovery decides, because that is the fact `DoorPlan::Adopting`
/// turns on: a page a worker served never probes anything, and a page the
/// coordinator served has already spent the request that proves so.
async fn served_by_this_page(page_origin: &str) -> Option<LocalBootstrap> {
    if page_origin.is_empty() {
        return None;
    }
    let answer = door_probe::fetch_bootstrap(&format!("{page_origin}{LOCAL_BOOTSTRAP_PATH}")).await;
    match read_serving_origin(answer.status, &answer.body) {
        BootstrapOutcome::Served(bootstrap) => Some(bootstrap),
        BootstrapOutcome::NotWorkerServed(_) => None,
    }
}

/// Drain on the next task, never inside the callback that queued the message.
fn schedule_drain(pump: &Pump, connection_id: &str) {
    let pump = pump.clone();
    let connection_id = connection_id.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        drain(&pump, &connection_id);
    });
}

/// Hand one socket's observations to the core, in arrival order.
///
/// The socket's own token is looked up rather than carried here, because the id
/// a callback knows is not the generation a frame is folded on, and a guessed
/// token is a frame folded against the wrong route.
fn drain(pump: &Pump, connection_id: &str) {
    let Some((messages, admitted)) = pump.inner.carriers.borrow().observe(connection_id) else {
        return;
    };
    for message in messages {
        match message {
            LoopbackMessage::Open => {
                tracing::debug!(
                    target: "carriers",
                    connection_id,
                    admitted,
                    "loopback carrier socket open"
                );
            }
            LoopbackMessage::Binary(bytes) if admitted => deliver(pump, connection_id, &bytes),
            LoopbackMessage::Binary(bytes) => handshake(pump, connection_id, &bytes),
            LoopbackMessage::Closed { code, reason } => lose(pump, connection_id, code, &reason),
        }
    }
}

/// One frame on a socket that has not authenticated: the `Ready`, or a fault.
fn handshake(pump: &Pump, connection_id: &str, bytes: &[u8]) {
    let Some(pending) = pump.inner.carriers.borrow_mut().take_pending(connection_id) else {
        return;
    };
    match LoopbackConnection::receive_pre_hello(bytes) {
        Ok(ready) => admit(pump, connection_id, pending, ready),
        Err(fault) => refuse(pump, connection_id, &pending.door_worker_fp, &fault),
    }
}

/// Judge the worker's `Ready` and, if it holds, register the carrier.
fn admit(pump: &Pump, connection_id: &str, pending: Pending, ready: LoopbackReady) {
    let connection = match LoopbackConnection::admit(
        &pending.grant,
        &pending.door_worker_fp,
        connection_id,
        &ready,
    ) {
        Ok(connection) => connection,
        Err(fault) => {
            refuse(pump, connection_id, &pending.door_worker_fp, &fault);
            return;
        }
    };
    let carrier = connection.carrier().clone();
    let displaced = {
        let mut held = pump.inner.carriers.borrow_mut();
        held.admit(&connection, pending.handle)
    };
    if let Some(old) = displaced {
        tracing::info!(
            target: "carriers",
            connection_id,
            displaced = old.connection_id(),
            "a newer loopback carrier displaced this one"
        );
    }
    tracing::info!(
        target: "carriers",
        connection_id,
        worker_fp = %carrier.worker_fp,
        process_epoch = %carrier.token.process_epoch,
        sessions = carrier.granted_sessions.len(),
        "loopback carrier admitted"
    );
    pump.dispatch(ClientEvent::CarrierReady(carrier));
}

/// One frame on a carrier that has authenticated: a fold, a history page, or a
/// close.
fn deliver(pump: &Pump, connection_id: &str, bytes: &[u8]) {
    let Some(token) = pump
        .inner
        .carriers
        .borrow()
        .token_for(connection_id)
        .cloned()
    else {
        return;
    };
    let inbound = match LoopbackConnection::receive_from(bytes) {
        Ok(inbound) => inbound,
        Err(fault) => {
            tracing::warn!(
                target: "carriers",
                connection_id,
                worker_fp = token.worker_fp.as_deref().unwrap_or(""),
                process_epoch = %token.process_epoch,
                fault = %fault,
                "loopback frame refused"
            );
            return;
        }
    };
    let inbound = match inbound {
        DirectInbound::Scrollback(answer) => {
            super::direct_history::answered(pump, answer);
            return;
        }
        other => other,
    };
    if let DirectInbound::Closed { reason } = &inbound {
        tracing::info!(
            target: "carriers",
            connection_id,
            worker_fp = token.worker_fp.as_deref().unwrap_or(""),
            reason = %if reason.is_empty() { "the worker sent none" } else { reason.as_str() },
            "the worker closed the loopback carrier"
        );
    }
    let Some(frame) = inbound.as_sync_frame(token.socket_generation) else {
        return;
    };
    pump.dispatch(ClientEvent::DirectFrameReceived { token, frame });
}

/// A socket that will never carry: closed, and reported by name.
fn refuse(pump: &Pump, connection_id: &str, worker_fp: &str, fault: &CarrierFault) {
    if let Some(handle) = pump.inner.carriers.borrow_mut().drop_pending(connection_id) {
        handle.close(1000, "the loopback carrier was refused");
    }
    tracing::warn!(
        target: "carriers",
        connection_id,
        worker_fp,
        fault = %fault,
        "loopback carrier refused; the session stays on the Sync route"
    );
    let _ = pump;
}

/// The socket ended. Whatever it was carrying is retired exactly once.
fn lose(pump: &Pump, connection_id: &str, code: u16, reason: &str) {
    let (admitted, was_pending) = {
        let mut held = pump.inner.carriers.borrow_mut();
        let admitted = held.retire(connection_id);
        let was_pending = held.drop_pending(connection_id).is_some();
        (admitted, was_pending)
    };
    let Some((handle, token)) = admitted else {
        // A socket that never authenticated has no route to retire, and saying
        // so is the point: the credential it spent is the fact an operator needs.
        tracing::info!(
            target: "carriers",
            connection_id,
            code,
            reason,
            was_pending,
            "loopback socket closed before it was admitted"
        );
        return;
    };
    drop(handle);
    tracing::info!(
        target: "carriers",
        connection_id,
        code,
        reason,
        worker_fp = token.worker_fp.as_deref().unwrap_or(""),
        process_epoch = %token.process_epoch,
        "loopback carrier lost"
    );
    pump.dispatch(ClientEvent::CarrierLost {
        connection_id: connection_id.to_owned(),
    });
    super::direct_history::lose_reads_off_route(pump, reason);
}
