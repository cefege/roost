//! Bringing the pump up: the tab id claim, the device key, identity discovery,
//! the captured `#pair=` redeem, then the first dial and the timers.
//!
//! Called once by `App`. Ported from `apps/web/src/store/sync-bootstrap.ts`
//! (`_bootstrap`, `:162-199`) and `apps/web/src/store/sync-bootstrap.pair.ts`
//! (`dispatchCapturedFragmentCredential`); the redeem request is
//! `apps/web/src/store/auth/redeemPairToken.ts:30-55`; the claim is
//! `apps/web/src/client/auth/tab-id.ts` (`claimTabIdentity`).

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::rpc::{CallError, ConnectCode};
use roost_client_core::{ClientCore, ClientEvent, RpcCall, RpcResult};

use super::Pump;
use crate::platform::connect::CoordRpc;
use crate::platform::device_key::WebDeviceKey;
use crate::platform::fragment_credential::{captured_credential, clear_captured_credential};
use crate::platform::self_label::current_browser_self_label;

/// Build the pump for `core` and start it. The returned pump is the one the
/// app provides in context.
///
/// The core and the coordinator client start with no tab id: the document has
/// none until `boot` claims one, and nothing transports before that.
pub fn start_pump(core: Rc<RefCell<ClientCore>>, revision: Signal<u64>) -> Pump {
    let rpc = Rc::new(CoordRpc::new(coordinator_origin(), ""));
    let pump = Pump::new(core, revision, rpc);
    let booting = pump.clone();
    wasm_bindgen_futures::spawn_local(async move { boot(booting).await });
    pump
}

async fn boot(pump: Pump) {
    // Independent waits: the claim answers over a BroadcastChannel and the key
    // comes out of IndexedDB, and neither reads the other.
    let ((), key) = futures_util::join!(claim_tab_id(&pump), WebDeviceKey::load_or_generate());
    match key {
        Ok(key) => pump.inner.rpc.install_key(Rc::new(key)),
        Err(reason) => {
            tracing::error!(target: "auth", %reason, "device key unavailable; calls go out unauthenticated");
        }
    }
    pump.dispatch(ClientEvent::BootstrapRequested);
    if redeem_captured_pair_token(&pump).await {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    {
        super::browser::install(&pump);
        wasm_bindgen_futures::spawn_local({
            let pump = pump.clone();
            async move {
                let _ = super::carrier_dial::resolve_door(&pump).await;
            }
        });
    }
    pump.dispatch(ClientEvent::DialRequested);
}

/// Claim this document's tab id and present it on the store (the Sync dial)
/// and on every Connect call. v2 awaits the claim before any transport
/// (`sync-bootstrap.ts:163`, `main.tsx:154`); the claim is kept with the
/// pump's listeners because its channel answers later duplicated documents.
#[cfg(target_arch = "wasm32")]
async fn claim_tab_id(pump: &Pump) {
    let claim = crate::platform::tab_id::claim_document_tab_id().await;
    claim
        .id()
        .clone_into(&mut pump.inner.core.borrow_mut().store_mut().tab_id);
    pump.inner.rpc.present_tab_id(claim.id());
    pump.inner.listeners.borrow_mut().push(Box::new(claim));
}

/// No document, so no tab id to claim.
#[cfg(not(target_arch = "wasm32"))]
async fn claim_tab_id(_pump: &Pump) {}

/// Spend a scrubbed `#pair=` token on this device's key before anything
/// protected is asked for. `true` when it succeeded and the page is being
/// reloaded as a paired key.
async fn redeem_captured_pair_token(pump: &Pump) -> bool {
    let Some(token) = captured_credential() else {
        return false;
    };
    let ssh_pubkey_b64 = pump
        .inner
        .rpc
        .device_key()
        .map(|key| key.public_key_b64().to_owned())
        .unwrap_or_default();
    let call = RpcCall::RedeemPairToken {
        call_id: 0,
        token,
        ssh_pubkey_b64,
        label: current_browser_self_label(),
    };
    match pump.inner.rpc.call_core(&call).await {
        RpcResult::PairTokenRedeemed { .. } => {
            clear_captured_credential();
            tracing::info!(target: "auth", "pair token redeemed; reloading as a paired key");
            crate::platform::location::replace_location("/");
            true
        }
        RpcResult::Failed { error, .. } => {
            if redeem_failure_is_authoritative(&error) {
                clear_captured_credential();
            }
            tracing::warn!(target: "auth", %error, "#pair redeem failed");
            false
        }
        other => {
            tracing::error!(target: "auth", answer = other.kind_name(), "redeem answered with the wrong message");
            false
        }
    }
}

/// A refusal the coordinator will repeat for the same token, so keeping the
/// token would only fail again (`redeemPairToken.ts:45-50`).
fn redeem_failure_is_authoritative(error: &CallError) -> bool {
    matches!(
        error.code(),
        Some(
            ConnectCode::InvalidArgument
                | ConnectCode::AlreadyExists
                | ConnectCode::PermissionDenied
                | ConnectCode::Unauthenticated
        )
    )
}

/// The coordinator this document dials, as `connect.ts:69-93` resolves it.
///
/// The serving-origin answer was primed before the graph loaded, so a page a
/// WORKER served dials the coordinator that worker advertised instead of itself.
/// The decision and its storage keys live in `client::local`; this only names
/// the host inputs.
#[cfg(target_arch = "wasm32")]
fn coordinator_origin() -> String {
    crate::platform::door_probe::coordinator_base_url_for_page()
}

/// A build with no document has no origin and no coordinator to dial.
#[cfg(not(target_arch = "wasm32"))]
fn coordinator_origin() -> String {
    String::new()
}
