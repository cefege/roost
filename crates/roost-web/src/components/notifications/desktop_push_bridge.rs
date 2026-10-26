//! The page-load half of Desktop notifications: once per authorized document,
//! repair this browser's Web Push subscription against the coordinator, and
//! route a clicked OS notification (posted by `sw-push.js`) to its session or
//! to the pairing approvals in this window. Renders nothing. Mounted once by
//! `NotificationDock`; the decisions are `crate::web_push`.

use dioxus::prelude::*;

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use crate::pump::use_store;
#[cfg(target_arch = "wasm32")]
use crate::router_state::{navigate_path, use_location};
#[cfg(target_arch = "wasm32")]
use crate::web_push::browser_push::ServiceWorkerClicks;
#[cfg(target_arch = "wasm32")]
use crate::web_push::desktop_push_plan::NotificationClick;

/// Renders nothing; exists for its two effects, each installed once.
#[component]
pub fn DesktopPushBridge() -> Element {
    #[cfg(target_arch = "wasm32")]
    {
        let reconcile_pump = use_store();
        let path = use_location();
        use_hook(move || {
            wasm_bindgen_futures::spawn_local(async move {
                crate::web_push::push_lifecycle::reconcile_on_load(&reconcile_pump).await;
            });
        });
        // The registration lives in the hook slot, so the handler comes off the
        // container when this scope ends rather than outliving the router it
        // writes to.
        let _clicks = use_hook(move || Rc::new(route_notification_clicks(path)));
    }
    rsx! {}
}

/// Open each clicked notification's exact session destination in this window,
/// or the pairing approvals.
#[cfg(target_arch = "wasm32")]
fn route_notification_clicks(path: Signal<String>) -> Option<ServiceWorkerClicks> {
    ServiceWorkerClicks::install(move |click: NotificationClick| match click {
        NotificationClick::Session(session_id) => {
            let href = crate::routes::session_href(session_id.as_str());
            tracing::info!(
                target: "notifications",
                session = %session_id,
                "push notification click opened its session"
            );
            navigate_path(path, href);
        }
        NotificationClick::PairApprovals => {
            tracing::info!(
                target: "notifications",
                "push notification click opened the pairing approvals"
            );
            navigate_path(path, crate::routes::Route::Pair.to_path());
        }
    })
}
