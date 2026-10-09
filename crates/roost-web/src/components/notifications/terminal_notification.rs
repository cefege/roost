//! Toasts for a program's own desktop notification (OSC 9 / OSC 777;notify),
//! shown while nobody in this tab is looking at the session that sent it.
//! Mounted by `NotificationDock`; drains `Store::terminal_signals`'
//! notification queue and honours the in-app notification preference. A tab
//! that is not open still learns through Web Push (`push/terminal_notification`).

use dioxus::prelude::*;
use roost_client_core::store::prefs::notify::NotifyPref;
use roost_client_core::store::selectors::session_by_id;
use roost_client_core::store::terminal_signals::TerminalNotificationRequest;
use roost_client_core::store::toasts::{ToastId, ToastKind, ToastOptions, ToastSource, add_toast};

use super::agent_notifications::agent_attention;
use super::store_write::write_store;
use crate::components::terminal::dom::now_ms;
use crate::platform::worker_paths::BrowserWorkerPaths;
use crate::pump::{Pump, use_store};
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;
use crate::session_naming::session_title;

/// The toast's text: the program's title (or the session's, which is what
/// OSC 9 leaves to the terminal) and the body.
pub fn notification_text(program_title: &str, session_title: &str, body: &str) -> String {
    let heading = if program_title.is_empty() {
        session_title
    } else {
        program_title
    };
    if body.is_empty() {
        heading.to_owned()
    } else {
        format!("{heading}: {body}")
    }
}

/// Drains program notifications and raises the toasts this tab should show.
#[component]
pub fn TerminalNotification() -> Element {
    let pump = use_store();
    let path = use_location();
    let attention = agent_attention::use_page_attention();
    use_effect(move || {
        let _ = pump.revision().read();
        let attended = *attention.read();
        let requests = {
            let core = pump.core();
            let mut core = core.borrow_mut();
            core.store_mut()
                .terminal_signals
                .drain_notifications()
                .collect::<Vec<_>>()
        };
        for request in requests {
            deliver(&pump, &path.peek(), attended, &request);
        }
    });
    rsx! {}
}

fn deliver(pump: &Pump, path: &str, attended: bool, request: &TerminalNotificationRequest) {
    let text = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let viewing = attended
            .then(|| active_session_for_path(store, &BrowserWorkerPaths, path))
            .flatten()
            .is_some_and(|session| session.id.as_str() == request.session_id);
        if viewing || !store.prefs.notify.get(NotifyPref::InApp) {
            None
        } else {
            session_by_id(store, &request.session_id).map(|session| {
                notification_text(
                    &request.notification.title,
                    &session_title(store, session),
                    &request.notification.body,
                )
            })
        }
    };
    let Some(text) = text else {
        return;
    };
    let id = ToastId::new(
        ToastSource::Host {
            name: "terminal-notification",
        },
        format!("{}:{}", request.session_id, request.delivery_seq),
    );
    let options = ToastOptions::with_ttl(Some(8_000)).with_action("View", &request.session_id);
    write_store(pump, |store| {
        add_toast(store, id, text, ToastKind::Ok, options, now_ms())
    });
    tracing::debug!(
        target: "notifications",
        session_id = %request.session_id,
        delivery_seq = request.delivery_seq,
        "terminal notification delivered"
    );
}

#[cfg(test)]
mod tests {
    use super::notification_text;

    #[test]
    fn an_untitled_notification_is_headed_by_the_session() {
        assert_eq!(notification_text("", "roost", "Done"), "roost: Done");
        assert_eq!(notification_text("Build", "roost", "ok"), "Build: ok");
        assert_eq!(notification_text("Build", "roost", ""), "Build");
    }
}
