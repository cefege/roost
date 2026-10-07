//! Desktop notifications: this browser's Web Push subscription, kept in step
//! with the Desktop switch in Settings → Notifications and with the coordinator.
//!
//! `desktop_push_plan` is pure: what the switch shows, what a boot-time check
//! repairs, and what a failure tells the reader. `browser_push` asks the
//! browser (service worker, permission, PushManager) and `push_lifecycle` joins
//! the two to the coordinator's `PushGetConfig`/`PushSubscribe`/
//! `PushUnsubscribe`. Called by the Settings pane and `DesktopPushBridge`.
//!
//! WHO IS NOT NOTIFIED IS THE COORDINATOR'S RULE. A device whose browser holds
//! a view of the session is dropped from the fan-out server-side
//! (`roost-coord` `push/viewers.rs`), so nothing here filters a push again.

#[cfg(target_arch = "wasm32")]
pub mod browser_push;
pub mod desktop_push_plan;
#[cfg(target_arch = "wasm32")]
pub mod push_lifecycle;

/// The service worker that shows a push and routes its click. Served from the
/// origin root (`Dioxus.toml` `public_dir`), so its default scope is `/` and it
/// can reach every window of the app.
pub const PUSH_SERVICE_WORKER_URL: &str = "/sw-push.js";

/// The message `sw-push.js` posts to a window when its notification is clicked.
pub const NAVIGATE_MESSAGE_TYPE: &str = "roost-navigate";
