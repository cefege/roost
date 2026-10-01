//! Minting a direct credential: ask the coordinator, build the one grant the
//! answer installed, hand it to the election and to the dial, and name every way
//! the mint can produce nothing spendable.
//!
//! Owned by `pump::carriers`, called by `pump::effects` for
//! `Effect::RequestDirectGrant` and by `pump::carrier_dial` whenever a dial ends
//! without a live carrier. It decides nothing about whether a carrier MAY open —
//! that is `client::carriers::Signalling`'s — and it holds no connection.

#[cfg(target_arch = "wasm32")]
use std::collections::BTreeSet;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::DirectGrant;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::carriers::grant_rpc::MintLocalTerminalGrant;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::GrantMintAnswer;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::local::{GrantMintRequest, LocalTerminalGrant};
use roost_client_core::event::ClientEvent;

use super::super::Pump;
#[cfg(target_arch = "wasm32")]
use crate::platform::clock::WallClock;

/// Perform one `Effect::RequestDirectGrant`: mint the credential a carrier will
/// authenticate on, and hand it to the dial.
///
/// Two properties are the reason this is not one line:
///
/// - **A mint that RETURNS is a refusal, not a pending state.** Only a worker's
///   acknowledgement reveals the secret, so an answer without one installed
///   nothing, and reporting nothing back would leave the core's lifecycle in
///   `Requested` — the one phase with no path forward.
/// - **Nothing here decides whether a carrier MAY open.** That is
///   `client::carriers::Signalling`'s, driven by demand and by the loopback
///   probe. This performs the mint the lifecycle asked for and hands the
///   credential to `pump::carrier_dial`, which spends it or reports that it
///   could not.
#[cfg(target_arch = "wasm32")]
pub(in crate::pump) fn request_grant(pump: &Pump, session_id: &str, worker_fp: &str) {
    let request = GrantMintRequest {
        worker_fp: worker_fp.to_owned(),
        session_ids: vec![session_id.to_owned()],
        tab_id: pump.inner.core.borrow().store().tab_id.clone(),
    };
    let rpc = pump.rpc();
    // The pump is cheap to clone and is the only way the answer gets back:
    // the core is reached by dispatch, never by a task holding the store.
    let reply_to = pump.clone();
    let worker = worker_fp.to_owned();
    let session = session_id.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        let answer = match rpc.call(&MintLocalTerminalGrant { request }).await {
            Ok(answer) => answer,
            Err(error) => {
                report_mint_refusal(&reply_to, &worker, &session, &error.to_string());
                return;
            }
        };
        let Some(grant) = local_grant(&reply_to, &worker, &session, answer) else {
            report_mint_refusal(
                &reply_to,
                &worker,
                &session,
                "the coordinator installed no credential on the worker",
            );
            return;
        };
        tracing::info!(
            target: "carriers",
            worker_fp = %worker,
            session_id = %session,
            peer_supported = grant.peer_supported,
            "direct grant minted; the election decides what it opens"
        );
        // Reported BEFORE it is spent, and by the place that BUILT it rather
        // than by the dial that spends it: the loopback carrier and the peer
        // machine are two answers to one credential, so the election has to
        // learn it exists from one construction of one reply.
        reply_to.dispatch(ClientEvent::DirectGrantMinted {
            grant: DirectGrant::from_local(&grant),
        });
        super::super::carrier_dial::dial(&reply_to, grant);
    });
}

/// The credential the coordinator's answer actually installed, or `None` with
/// the refusal already reported by name.
///
/// Built ONCE and handed to both consumers, because the loopback dial and the
/// election must agree about the deadline, the scope and the worker epoch of a
/// single reply — and two constructions of one answer is the drift that produces
/// a carrier authenticating on a grant the election believes is another's.
#[cfg(target_arch = "wasm32")]
fn local_grant(
    pump: &Pump,
    worker_fp: &str,
    session_id: &str,
    answer: GrantMintAnswer,
) -> Option<LocalTerminalGrant> {
    let (tab_id, device_fp) = {
        let tab_id = pump.inner.core.borrow().store().tab_id.clone();
        let device_fp = pump
            .inner
            .rpc
            .device_key()
            .map(|key| key.fingerprint().to_owned())
            .unwrap_or_default();
        (tab_id, device_fp)
    };
    LocalTerminalGrant::from_answer(
        answer,
        worker_fp,
        BTreeSet::from_iter([session_id.to_owned()]),
        tab_id,
        device_fp,
        WallClock.now_ms(),
    )
}

/// Perform one `Effect::RequestDirectGrant` on a build with no coordinator
/// transport to ask.
#[cfg(not(target_arch = "wasm32"))]
pub(in crate::pump) fn request_grant(pump: &Pump, session_id: &str, worker_fp: &str) {
    report_mint_refusal(
        pump,
        worker_fp,
        session_id,
        "this build has no coordinator transport",
    );
}

/// A mint that did not produce a spendable credential, or whose credential
/// nothing spent.
///
/// `warn` and not `error`: a worker that cannot peer, or a coordinator that is
/// merely down, leaves the session on the Sync route, which is a working state.
/// It is still a report — the thing being avoided is the SILENCE, not the level.
pub(in crate::pump) fn report_mint_refusal(
    pump: &Pump,
    worker_fp: &str,
    session_id: &str,
    detail: &str,
) {
    tracing::warn!(
        target: "carriers",
        worker_fp,
        session_id,
        detail,
        "direct grant refused; the session stays on the Sync route"
    );
    // Reported into the core as well as logged: a refusal the election never
    // hears about leaves its grant lifecycle in `Requested`, which is the one
    // phase with no path forward, so the retry is never armed.
    pump.dispatch(ClientEvent::DirectGrantRefused {
        worker_fp: worker_fp.to_owned(),
        reason: detail.to_owned(),
    });
}
