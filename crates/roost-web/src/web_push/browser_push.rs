//! The browser's half of Web Push: feature detection, the notification
//! permission, the `/sw-push.js` registration, the PushManager subscription,
//! and the service worker's click message. Called by `push_lifecycle` and
//! `DesktopPushBridge`; every answer is a plain value `desktop_push_plan` reads.
//!
//! EVERY GLOBAL IS PROBED BEFORE IT IS TOUCHED. An iOS Safari tab that is not a
//! Home Screen app has no `Notification` and no `PushManager`, and web-sys reads
//! those globals unchecked, so calling through an absent one throws out of wasm.

use js_sys::{Promise, Reflect, Uint8Array};
use roost_protocol::wire::SessionId;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    MessageEvent, Notification, NotificationPermission, PushSubscription,
    PushSubscriptionOptionsInit, ServiceWorkerContainer, ServiceWorkerRegistration,
};

use super::PUSH_SERVICE_WORKER_URL;
use super::desktop_push_plan::{DesktopPushError, PushPermission, clicked_session};
use crate::platform::device_key::describe_js;

/// Whether `owner` has a member called `name`. A failed reflection is "no".
fn has_member(owner: &JsValue, name: &str) -> bool {
    Reflect::has(owner, &JsValue::from_str(name)).unwrap_or(false)
}

/// The service worker container, when this document has one: absent on an
/// insecure origin and in some embedded browsers.
fn service_worker_container() -> Option<ServiceWorkerContainer> {
    let navigator = web_sys::window()?.navigator();
    if !has_member(&navigator, "serviceWorker") {
        return None;
    }
    Some(navigator.service_worker())
}

/// Whether this browser can hold a push subscription at all.
pub fn browser_supports_push() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    has_member(&window, "PushManager")
        && has_member(&window, "Notification")
        && service_worker_container().is_some()
}

/// `Notification.permission`, or `Default` where there is no `Notification`.
pub fn current_permission() -> PushPermission {
    let has_notification =
        web_sys::window().is_some_and(|window| has_member(&window, "Notification"));
    if !has_notification {
        return PushPermission::Default;
    }
    match Notification::permission() {
        NotificationPermission::Granted => PushPermission::Granted,
        NotificationPermission::Denied => PushPermission::Denied,
        _ => PushPermission::Default,
    }
}

/// Start `Notification.requestPermission()`. MUST be called synchronously
/// inside the click that asked: Safari and Firefox show the prompt only to a
/// user gesture, and an `await` before this call spends the gesture.
pub fn request_permission_in_gesture() -> Option<Promise> {
    if !browser_supports_push() {
        return None;
    }
    Notification::request_permission().ok()
}

/// The prompt's answer.
pub async fn settle_permission(request: Promise) -> PushPermission {
    match JsFuture::from(request).await {
        Ok(answer) => PushPermission::from_browser(&answer.as_string().unwrap_or_default()),
        Err(error) => {
            tracing::warn!(target: "notifications", error = %describe_js(&error), "permission prompt failed");
            current_permission()
        }
    }
}

/// Register `/sw-push.js` and wait for it to be active.
///
/// Registering again is how an updated worker script reaches a browser that
/// registered an older one; the call is idempotent otherwise.
pub async fn registered_worker() -> Result<ServiceWorkerRegistration, DesktopPushError> {
    let container = service_worker_container().ok_or(DesktopPushError::Unsupported)?;
    JsFuture::from(container.register(PUSH_SERVICE_WORKER_URL))
        .await
        .map_err(browser_error)?;
    let ready = container.ready().map_err(browser_error)?;
    let registration = JsFuture::from(ready).await.map_err(browser_error)?;
    tracing::info!(target: "notifications", script = PUSH_SERVICE_WORKER_URL, "push service worker ready");
    Ok(registration.unchecked_into())
}

/// The registration controlling the app root, without registering one.
pub async fn existing_registration() -> Result<Option<ServiceWorkerRegistration>, DesktopPushError>
{
    let Some(container) = service_worker_container() else {
        return Ok(None);
    };
    let found = JsFuture::from(container.get_registration_with_document_url("/"))
        .await
        .map_err(browser_error)?;
    Ok(found.dyn_into::<ServiceWorkerRegistration>().ok())
}

/// The registration's current subscription, if it holds one.
pub async fn current_subscription(
    registration: &ServiceWorkerRegistration,
) -> Result<Option<PushSubscription>, DesktopPushError> {
    let manager = registration.push_manager().map_err(browser_error)?;
    let found = JsFuture::from(manager.get_subscription().map_err(browser_error)?)
        .await
        .map_err(browser_error)?;
    Ok(found.dyn_into::<PushSubscription>().ok())
}

/// Subscribe the registration to the coordinator's VAPID key.
///
/// `userVisibleOnly` is the only mode browsers grant: every push must show a
/// notification, which `sw-push.js` does for every payload it accepts.
pub async fn subscribe(
    registration: &ServiceWorkerRegistration,
    application_server_key: &[u8],
) -> Result<PushSubscription, DesktopPushError> {
    let manager = registration.push_manager().map_err(browser_error)?;
    let options = PushSubscriptionOptionsInit::new();
    options.set_user_visible_only(true);
    options.set_application_server_key(&Uint8Array::from(application_server_key).into());
    let created = JsFuture::from(
        manager
            .subscribe_with_options(&options)
            .map_err(browser_error)?,
    )
    .await
    .map_err(browser_error)?;
    created
        .dyn_into::<PushSubscription>()
        .map_err(|value| DesktopPushError::Browser(describe_js(&value)))
}

/// The subscription as `JSON.stringify` writes it, keys base64url-encoded.
pub fn subscription_json(subscription: &PushSubscription) -> Result<String, DesktopPushError> {
    js_sys::JSON::stringify(subscription)
        .map_err(browser_error)?
        .as_string()
        .ok_or_else(|| DesktopPushError::Browser("the subscription has no JSON form".to_owned()))
}

/// Drop the subscription at the push service.
pub async fn unsubscribe(subscription: &PushSubscription) -> Result<(), DesktopPushError> {
    JsFuture::from(subscription.unsubscribe().map_err(browser_error)?)
        .await
        .map_err(browser_error)?;
    Ok(())
}

/// Whether `subscription` was made for `application_server_key`. A browser
/// that does not report the key is taken at its word: there is nothing to
/// compare, and resubscribing on every load would rotate the endpoint forever.
pub fn subscription_uses_key(
    subscription: &PushSubscription,
    application_server_key: &[u8],
) -> bool {
    match subscription.options().application_server_key() {
        Ok(Some(buffer)) => Uint8Array::new(&buffer).to_vec() == application_server_key,
        Ok(None) | Err(_) => true,
    }
}

fn browser_error(error: JsValue) -> DesktopPushError {
    DesktopPushError::Browser(describe_js(&error))
}

/// The `onmessage` handler for `sw-push.js`'s notification click, taken back
/// off when this value drops.
///
/// The `onmessage` SLOT rather than `addEventListener`, because assigning it is
/// what enables the client's message queue: a click posted while the page was
/// still booting is delivered then, instead of waiting on a `startMessages()`
/// this build's bindings do not expose. This value is the slot's only owner.
pub struct ServiceWorkerClicks {
    container: ServiceWorkerContainer,
    listener: Closure<dyn FnMut(MessageEvent)>,
}

impl std::fmt::Debug for ServiceWorkerClicks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceWorkerClicks")
            .finish_non_exhaustive()
    }
}

impl ServiceWorkerClicks {
    /// Call `on_session` with each session a clicked notification names.
    pub fn install(mut on_session: impl FnMut(SessionId) + 'static) -> Option<Self> {
        let container = service_worker_container()?;
        let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = event.data();
            let member = |name: &str| {
                Reflect::get(&data, &JsValue::from_str(name))
                    .ok()
                    .and_then(|value| value.as_string())
            };
            if let Some(session_id) =
                clicked_session(member("type").as_deref(), member("sessionId").as_deref())
            {
                on_session(session_id);
            }
        });
        container.set_onmessage(Some(listener.as_ref().unchecked_ref()));
        Some(Self {
            container,
            listener,
        })
    }
}

impl Drop for ServiceWorkerClicks {
    fn drop(&mut self) {
        // A remount installs its own handler before this one drops; clearing a
        // slot that no longer holds this closure would silence the new one.
        let still_ours = self.container.onmessage().is_some_and(|current| {
            let current: &JsValue = current.as_ref();
            current == self.listener.as_ref()
        });
        if still_ours {
            self.container.set_onmessage(None);
        }
    }
}
