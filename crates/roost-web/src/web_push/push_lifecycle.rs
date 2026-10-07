//! Turning Desktop notifications on and off, and repairing them on a page load:
//! the browser steps in `browser_push` joined to the coordinator's
//! `PushGetConfig`/`PushSubscribe`/`PushUnsubscribe`, in the order the plan in
//! `desktop_push_plan` decides. Called by the Settings notifications pane (a
//! toggle) and `DesktopPushBridge` (once per authorized page load).

use js_sys::Promise;
use roost_client_core::ClientEvent;
use roost_client_core::client::rpc::calls::settings::push::{
    GetPushConfig, PushKeyError, SubscribePush, UnsubscribePush,
};
use roost_client_core::client::rpc::unary::CallError;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::shell_intent::ShellIntent;
use web_sys::{PushSubscription, ServiceWorkerRegistration};

use super::browser_push::{
    browser_supports_push, current_permission, current_subscription, existing_registration,
    registered_worker, settle_permission, subscribe, subscription_json, subscription_uses_key,
    unsubscribe,
};
use super::desktop_push_plan::{
    DesktopPushError, PushReconcile, reconcile_desktop_push, require_granted,
};
use crate::platform::connect::CoordRpc;
use crate::pump::Pump;

/// Turn delivery on: the prompt's answer, the coordinator's key, the worker,
/// a subscription for that key, and the coordinator's record of it.
///
/// `permission_request` is the promise the click started; see
/// `browser_push::request_permission_in_gesture`.
pub async fn enable_desktop_push(
    rpc: &CoordRpc,
    permission_request: Option<Promise>,
) -> Result<(), DesktopPushError> {
    let request = permission_request.ok_or(DesktopPushError::Unsupported)?;
    require_granted(settle_permission(request).await)?;
    let key = coordinator_key(rpc).await?;
    let registration = registered_worker().await?;
    let subscription = subscription_for_key(&registration, &key).await?;
    record_subscription(rpc, &subscription).await?;
    tracing::info!(target: "notifications", "desktop push enabled");
    Ok(())
}

/// Turn delivery off: drop the subscription at the push service FIRST, which
/// stops delivery even when the coordinator cannot be reached, then tell the
/// coordinator to forget the endpoint.
pub async fn disable_desktop_push(rpc: &CoordRpc) -> Result<(), DesktopPushError> {
    let Some(registration) = existing_registration().await? else {
        return Ok(());
    };
    let Some(subscription) = current_subscription(&registration).await? else {
        return Ok(());
    };
    let endpoint = subscription.endpoint();
    unsubscribe(&subscription).await?;
    let forgotten = rpc
        .call(&UnsubscribePush { endpoint })
        .await
        .map_err(coordinator_error)?;
    if !forgotten {
        return Err(DesktopPushError::Coordinator(
            "the endpoint was not removed".to_owned(),
        ));
    }
    tracing::info!(target: "notifications", "desktop push disabled");
    Ok(())
}

/// Bring the browser, the coordinator and the preference back into agreement
/// once per page load. Never fails: a repair that cannot run now runs on the
/// next load, and the Settings row reports the state the browser is in.
pub async fn reconcile_on_load(pump: &Pump) {
    if !browser_supports_push() {
        return;
    }
    let rpc = pump.rpc();
    let config = match rpc.call(&GetPushConfig).await {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!(target: "notifications", %error, "push config read refused");
            return;
        }
    };
    let key = config.application_server_key();
    let enabled = pump
        .core()
        .borrow()
        .store()
        .prefs
        .notify
        .get(NotifyPref::Desktop);
    let outcome = match current_state(enabled).await {
        Ok((registration, subscription)) => {
            let plan = reconcile_desktop_push(
                enabled,
                current_permission(),
                subscription.is_some(),
                key.is_ok(),
            );
            tracing::info!(target: "notifications", plan = ?plan, "desktop push reconcile");
            apply_reconcile(pump, &rpc, plan, registration, key).await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = outcome {
        tracing::warn!(target: "notifications", %error, "desktop push reconcile failed");
    }
}

/// The registration and its subscription. An enabled browser registers the
/// worker, which is also how an updated `sw-push.js` reaches it; a disabled one
/// only looks, so a browser that never opted in never gets a worker.
async fn current_state(
    enabled: bool,
) -> Result<(Option<ServiceWorkerRegistration>, Option<PushSubscription>), DesktopPushError> {
    let registration = if enabled {
        Some(registered_worker().await?)
    } else {
        existing_registration().await?
    };
    let subscription = match &registration {
        Some(registration) => current_subscription(registration).await?,
        None => None,
    };
    Ok((registration, subscription))
}

async fn apply_reconcile(
    pump: &Pump,
    rpc: &CoordRpc,
    plan: PushReconcile,
    registration: Option<ServiceWorkerRegistration>,
    key: Result<Vec<u8>, PushKeyError>,
) -> Result<(), DesktopPushError> {
    match plan {
        PushReconcile::Settled => Ok(()),
        PushReconcile::Refresh | PushReconcile::Resubscribe => {
            let key = key?;
            let registration = registration.ok_or(DesktopPushError::Unsupported)?;
            let subscription = subscription_for_key(&registration, &key).await?;
            record_subscription(rpc, &subscription).await
        }
        PushReconcile::Revoke => {
            pump.dispatch(ClientEvent::Shell(ShellIntent::SetNotifyPref {
                pref: NotifyPref::Desktop,
                value: false,
            }));
            disable_desktop_push(rpc).await
        }
        PushReconcile::DropStale => disable_desktop_push(rpc).await,
    }
}

/// The coordinator's key as `PushManager.subscribe` takes it.
async fn coordinator_key(rpc: &CoordRpc) -> Result<Vec<u8>, DesktopPushError> {
    let config = rpc.call(&GetPushConfig).await.map_err(coordinator_error)?;
    Ok(config.application_server_key()?)
}

/// The registration's subscription for `key`: the existing one when it was
/// made for this key, otherwise a fresh one. A subscription made for another
/// key (a coordinator whose VAPID identity changed) is dropped first, because
/// the push service refuses every message the new key signs to it.
async fn subscription_for_key(
    registration: &ServiceWorkerRegistration,
    key: &[u8],
) -> Result<PushSubscription, DesktopPushError> {
    if let Some(existing) = current_subscription(registration).await? {
        if subscription_uses_key(&existing, key) {
            return Ok(existing);
        }
        tracing::info!(target: "notifications", "push subscription made for another key; replacing");
        unsubscribe(&existing).await?;
    }
    subscribe(registration, key).await
}

/// Store the subscription on the coordinator. An upsert, so a repeat refreshes.
async fn record_subscription(
    rpc: &CoordRpc,
    subscription: &PushSubscription,
) -> Result<(), DesktopPushError> {
    let request = SubscribePush::from_subscription_json(&subscription_json(subscription)?)?;
    let stored = rpc.call(&request).await.map_err(coordinator_error)?;
    if !stored {
        return Err(DesktopPushError::Coordinator(
            "the subscription was not stored".to_owned(),
        ));
    }
    tracing::info!(target: "notifications", "push subscription recorded");
    Ok(())
}

/// The coordinator's own sentence when it sent one: the subscribe refusals
/// name their cause (a full device, push off) and the reader can act on that.
fn coordinator_error(error: CallError) -> DesktopPushError {
    match error {
        CallError::Connect(refusal) => DesktopPushError::Coordinator(refusal.message),
        other => DesktopPushError::Coordinator(other.to_string()),
    }
}
