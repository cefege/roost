//! Web Push for a long shell command that finished (OSC 133 `D`, at least the
//! worker's 10 s threshold): the phone counterpart of the browser's "command
//! finished" toast.
//!
//! Subscribed to `Buses::command_finished_bus` once at boot by `serve`. Reuses
//! the agent push's target selection, so the same operator allowlist applies
//! and a device already viewing the session is not told.

use std::sync::Arc;

use roost_observability::LogFields;
use roost_protocol::wire::SessionId;
use serde::Serialize;
use sha2::Digest as _;
use sqlx::AnyPool;

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::SessionCommandFinished;
use crate::push::dispatch::{
    ActiveTerminalViewers, PushTransition, open_session, select_targets, session_title,
};
use crate::push::sender::{PushDeliveryOptions, send_push_to_subscriptions};
use crate::push::subscription_store::take_deliverable_subscriptions;
use crate::push::transport::PushNotificationTransport;

/// What one command-finished notification says. `kind` is `done` because the
/// service worker (`assets/sw-push.js`) shows `blocked`/`done` payloads and a
/// finished command is the "finished, look when you like" kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandFinishedPayload {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub kind: PushTransition,
    pub title: String,
    pub body: String,
    #[serde(rename = "deduplicationToken")]
    pub deduplication_token: String,
}

/// The delivery collaborators, shared with the agent push.
pub struct CommandFinishedPush {
    pool: AnyPool,
    allowed_origins: Vec<String>,
    viewers: Arc<dyn ActiveTerminalViewers>,
    transport: Arc<dyn PushNotificationTransport>,
}

impl std::fmt::Debug for CommandFinishedPush {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandFinishedPush")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

impl CommandFinishedPush {
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

    /// Push every completion published from now on. The returned subscription
    /// is the whole lifetime: dropping it stops the pushes.
    pub fn subscribe(self: Arc<Self>, buses: &Buses) -> Subscription<SessionCommandFinished> {
        buses.command_finished_bus.subscribe(move |finished| {
            // An empty allowlist switches push off; nothing is spawned for it.
            if self.allowed_origins.is_empty() {
                return;
            }
            let push = Arc::clone(&self);
            let finished = finished.clone();
            tokio::spawn(async move { push.deliver(&finished).await });
        })
    }

    async fn deliver(&self, finished: &SessionCommandFinished) {
        let session_id = finished.session_id.as_str();
        let Some((cwd, custom_title)) = open_session(&self.pool, session_id).await else {
            return;
        };
        let subscriptions = match take_deliverable_subscriptions(&self.pool).await {
            Ok(subscriptions) if !subscriptions.is_empty() => subscriptions,
            Ok(_) => return,
            Err(error) => {
                log_failed(session_id, &error.to_string());
                return;
            }
        };
        let Ok(session) = SessionId::try_from(session_id.to_owned()) else {
            return;
        };
        let viewing = self.viewers.active_viewer_fingerprints(&session);
        let (targets, suppressed) = select_targets(&subscriptions, &self.allowed_origins, &viewing);
        if targets.is_empty() {
            return;
        }
        let payload =
            command_finished_payload(finished, session_title(&cwd, custom_title.as_deref()));
        let body = match serde_json::to_string(&payload) {
            Ok(body) => body,
            Err(error) => {
                log_failed(session_id, &error.to_string());
                return;
            }
        };
        let result = send_push_to_subscriptions(
            &self.pool,
            &targets,
            &body,
            PushDeliveryOptions {
                deduplication_token: Some(payload.deduplication_token.clone()),
                is_current: None,
            },
            self.transport.as_ref(),
        )
        .await;
        roost_observability::log::info(
            "push",
            "command_finished_dispatched",
            LogFields::new()
                .set("session_id", session_id)
                .set("suppressed", suppressed)
                .set("targeted", targets.len())
                .set("delivered", result.delivered)
                .set("expired", result.expired)
                .set("failed", result.failed),
        );
    }
}

/// The notification: the session's title, and "Finished · 2m 13s" or
/// "Failed · exit 1 · 2m 13s". Never any terminal text.
#[must_use]
pub fn command_finished_payload(
    finished: &SessionCommandFinished,
    title: String,
) -> CommandFinishedPayload {
    let duration = format_duration(finished.duration_ms);
    let body = match finished.exit_code {
        Some(code) if code != 0 => format!("Failed · exit {code} · {duration}"),
        _ => format!("Finished · {duration}"),
    };
    let identity = format!(
        "{}:command:{}:{}",
        finished.session_id,
        finished.exit_code.unwrap_or_default(),
        finished.duration_ms
    );
    let digest = sha2::Sha256::digest(identity.as_bytes());
    CommandFinishedPayload {
        session_id: finished.session_id.clone(),
        kind: PushTransition::Done,
        title,
        body,
        deduplication_token: roost_host::b64url_encode(&digest)[..32].to_owned(),
    }
}

/// `45s`, `2m 13s`, `1h 4m`.
fn format_duration(duration_ms: u64) -> String {
    let seconds = duration_ms / 1_000;
    match (seconds / 3_600, seconds / 60 % 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m {seconds}s"),
        (hours, minutes, _) => format!("{hours}h {minutes}m"),
    }
}

fn log_failed(session_id: &str, error: &str) {
    roost_observability::log::warn(
        "push",
        "command_finished_failed",
        LogFields::new()
            .set("session_id", session_id)
            .set("error", error),
    );
}

#[cfg(test)]
mod tests {
    use super::command_finished_payload;
    use crate::events::bus_messages::SessionCommandFinished;

    #[test]
    fn the_payload_names_the_outcome_and_duration_but_no_terminal_text() {
        let failed = command_finished_payload(
            &SessionCommandFinished {
                session_id: "s".to_owned(),
                exit_code: Some(2),
                duration_ms: 133_000,
            },
            "roost".to_owned(),
        );
        assert_eq!(failed.body, "Failed · exit 2 · 2m 13s");
        let finished = command_finished_payload(
            &SessionCommandFinished {
                session_id: "s".to_owned(),
                exit_code: Some(0),
                duration_ms: 3_725_000,
            },
            "roost".to_owned(),
        );
        assert_eq!(finished.body, "Finished · 1h 2m");
        assert_ne!(failed.deduplication_token, finished.deduplication_token);
    }
}
