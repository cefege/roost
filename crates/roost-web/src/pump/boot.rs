//! Bringing the pump up: the device key, identity discovery, the captured
//! `#pair=` redeem, then the first dial and the timers.
//!
//! Called once by `App`. Ported from `apps/web/src/store/sync-bootstrap.ts`
//! (`_bootstrap`, `:162-199`) and `apps/web/src/store/sync-bootstrap.pair.ts`
//! (`dispatchCapturedFragmentCredential`); the redeem request is
//! `apps/web/src/store/auth/redeemPairToken.ts:30-55`.

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
pub fn start_pump(core: Rc<RefCell<ClientCore>>, revision: Signal<u64>, tab_id: &str) -> Pump {
    let rpc = Rc::new(CoordRpc::new(coordinator_origin(), tab_id));
    let pump = Pump::new(core, revision, rpc);
    let booting = pump.clone();
    wasm_bindgen_futures::spawn_local(async move { boot(booting).await });
    pump
}

async fn boot(pump: Pump) {
    match WebDeviceKey::load_or_generate().await {
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
    super::browser::install(&pump);
    pump.dispatch(ClientEvent::DialRequested);
}

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

/// The coordinator is the page's own origin (v2 `connect.ts:73-107` without the
/// local-bootstrap override, which the loopback slice adds).
fn coordinator_origin() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.location().origin().ok())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        String::new()
    }
}
