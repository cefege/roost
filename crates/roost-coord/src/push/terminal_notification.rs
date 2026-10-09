//! Web Push for a program's own desktop notification (OSC 9 / OSC 777;notify):
//! the phone counterpart of the browser's notification toast.
//!
//! Subscribed to `Buses::terminal_signal_bus` once at boot by `serve`; only a
//! message carrying a notification is pushed. Shares the delivery skeleton
//! (`session_push`), so the operator allowlist applies and a device already
//! viewing the session is not told.

use std::sync::Arc;

use serde::Serialize;
use sha2::Digest as _;
use sqlx::AnyPool;

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::SessionTerminalSignals;
use crate::push::dispatch::{ActiveTerminalViewers, PushTransition};
use crate::push::session_push::{SessionPushTargets, deliver_session_push};
use crate::push::transport::PushNotificationTransport;

/// What one program notification says. `kind` is `done`: the service worker
/// shows `blocked`/`done` payloads, and a program's notice is informational.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminalNotificationPayload {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub kind: PushTransition,
    pub title: String,
    pub body: String,
    #[serde(rename = "deduplicationToken")]
    pub deduplication_token: String,
}

/// The delivery collaborators, shared with the other session pushes.
pub struct TerminalNotificationPush {
    pool: AnyPool,
    allowed_origins: Vec<String>,
    viewers: Arc<dyn ActiveTerminalViewers>,
    transport: Arc<dyn PushNotificationTransport>,
}

impl std::fmt::Debug for TerminalNotificationPush {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalNotificationPush")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

impl TerminalNotificationPush {
    #[must_use]
    pub fn new(
        pool: AnyPool,
        allowed_origins: Vec<String>,
        viewers: Arc<dyn ActiveTerminalViewers>,
        transport: Arc<dyn PushNotificationTransport>,
    ) -> Self {
        Self {
            pool,
            allowed_origins,
            viewers,
            transport,
        }
    }

    /// Push every program notification published from now on; dropping the
    /// subscription stops the pushes.
    pub fn subscribe(self: Arc<Self>, buses: &Buses) -> Subscription<SessionTerminalSignals> {
        buses.terminal_signal_bus.subscribe(move |signals| {
            if self.allowed_origins.is_empty() || signals.notification.is_none() {
                return;
            }
            let push = Arc::clone(&self);
            let signals = signals.clone();
            tokio::spawn(async move { push.deliver(&signals).await });
        })
    }

    async fn deliver(&self, signals: &SessionTerminalSignals) {
        let Some(notification) = signals.notification.as_ref() else {
            return;
        };
        deliver_session_push(
            SessionPushTargets {
                pool: &self.pool,
                allowed_origins: &self.allowed_origins,
                viewers: &self.viewers,
                transport: &self.transport,
            },
            &signals.session_id,
            "terminal_notification",
            |payload: &TerminalNotificationPayload| payload.deduplication_token.clone(),
            |session_title| {
                terminal_notification_payload(
                    &signals.session_id,
                    &notification.title,
                    &notification.body,
                    session_title,
                )
            },
        )
        .await;
    }
}

/// The notification: the program's title (or the session's title when it gave
/// none, as OSC 9 does) and its body.
#[must_use]
pub fn terminal_notification_payload(
    session_id: &str,
    title: &str,
    body: &str,
    session_title: String,
) -> TerminalNotificationPayload {
    let identity = format!("{session_id}:notify:{title}:{body}");
    let digest = sha2::Sha256::digest(identity.as_bytes());
    TerminalNotificationPayload {
        session_id: session_id.to_owned(),
        kind: PushTransition::Done,
        title: if title.is_empty() {
            session_title
        } else {
            title.to_owned()
        },
        body: body.to_owned(),
        deduplication_token: roost_host::b64url_encode(&digest)[..32].to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::terminal_notification_payload;

    #[test]
    fn an_untitled_notification_takes_the_session_title() {
        let untitled = terminal_notification_payload("s", "", "Done", "roost".to_owned());
        assert_eq!(
            (untitled.title.as_str(), untitled.body.as_str()),
            ("roost", "Done")
        );
        let titled = terminal_notification_payload("s", "Build", "ok", "roost".to_owned());
        assert_eq!(titled.title, "Build");
        assert_ne!(untitled.deduplication_token, titled.deduplication_token);
    }
}
