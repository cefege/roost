//! The delivery skeleton every session-scoped Web Push shares: resolve the open
//! session's display title, take the deliverable subscriptions, skip devices
//! already viewing the session, send, and log the counts. `command_finished`
//! and `terminal_notification` build only their payloads; `serve` subscribes
//! them, and the `pair_request` push, through [`subscribe_event_pushes`].

use std::sync::Arc;

use roost_observability::LogFields;
use roost_protocol::wire::SessionId;
use serde::Serialize;
use sqlx::AnyPool;

use crate::push::dispatch::{ActiveTerminalViewers, open_session, select_targets, session_title};
use crate::push::sender::{PushDeliveryOptions, send_push_to_subscriptions};
use crate::push::subscription_store::take_deliverable_subscriptions;
use crate::push::transport::PushNotificationTransport;

/// What a session push needs from its owner.
pub struct SessionPushTargets<'a> {
    pub pool: &'a AnyPool,
    pub allowed_origins: &'a [String],
    pub viewers: &'a Arc<dyn ActiveTerminalViewers>,
    pub transport: &'a Arc<dyn PushNotificationTransport>,
}

impl std::fmt::Debug for SessionPushTargets<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionPushTargets")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

/// Deliver the payload `build` makes from the session's display title.
/// `event` names the log lines (`<event>_dispatched` / `<event>_failed`).
pub async fn deliver_session_push<P: Serialize>(
    targets: SessionPushTargets<'_>,
    session_id: &str,
    event: &str,
    deduplication_token: impl FnOnce(&P) -> String,
    build: impl FnOnce(String) -> P,
) {
    let Some((cwd, custom_title)) = open_session(targets.pool, session_id).await else {
        return;
    };
    let subscriptions = match take_deliverable_subscriptions(targets.pool).await {
        Ok(subscriptions) if !subscriptions.is_empty() => subscriptions,
        Ok(_) => return,
        Err(error) => {
            log_failed(event, session_id, &error.to_string());
            return;
        }
    };
    let Ok(session) = SessionId::try_from(session_id.to_owned()) else {
        return;
    };
    let viewing = targets.viewers.active_viewer_fingerprints(&session);
    let (selected, suppressed) = select_targets(&subscriptions, targets.allowed_origins, &viewing);
    if selected.is_empty() {
        return;
    }
    let payload = build(session_title(&cwd, custom_title.as_deref()));
    let token = deduplication_token(&payload);
    let body = match serde_json::to_string(&payload) {
        Ok(body) => body,
        Err(error) => {
            log_failed(event, session_id, &error.to_string());
            return;
        }
    };
    let result = send_push_to_subscriptions(
        targets.pool,
        &selected,
        &body,
        PushDeliveryOptions {
            deduplication_token: Some(token),
            is_current: None,
        },
        targets.transport.as_ref(),
    )
    .await;
    roost_observability::log::info(
        "push",
        &format!("{event}_dispatched"),
        LogFields::new()
            .set("session_id", session_id)
            .set("suppressed", suppressed)
            .set("targeted", selected.len())
            .set("delivered", result.delivered)
            .set("expired", result.expired)
            .set("failed", result.failed),
    );
}

fn log_failed(event: &str, session_id: &str, error: &str) {
    roost_observability::log::warn(
        "push",
        &format!("{event}_failed"),
        LogFields::new()
            .set("session_id", session_id)
            .set("error", error),
    );
}

/// Subscribe the command-finished, terminal-notification and pair-request
/// pushes; the returned handles are their whole lifetime.
pub fn subscribe_event_pushes(
    services: &crate::services::CoordServices,
    allowed_origins: &[String],
    transport: Arc<dyn PushNotificationTransport>,
) -> EventPushSubscriptions {
    let viewers = || Arc::clone(&services.views) as Arc<dyn ActiveTerminalViewers>;
    EventPushSubscriptions {
        _command_finished: Arc::new(crate::push::command_finished::CommandFinishedPush::new(
            services.db.pool().clone(),
            allowed_origins.to_vec(),
            viewers(),
            Arc::clone(&transport),
        ))
        .subscribe(&services.buses),
        _pair_request: Arc::new(crate::push::pair_request::PairRequestPush::new(
            services.db.pool().clone(),
            allowed_origins.to_vec(),
            Arc::clone(&transport),
        ))
        .subscribe(&services.buses),
        _terminal_notification: Arc::new(
            crate::push::terminal_notification::TerminalNotificationPush::new(
                services.db.pool().clone(),
                allowed_origins.to_vec(),
                viewers(),
                transport,
            ),
        )
        .subscribe(&services.buses),
    }
}

/// The event pushes' bus subscriptions; dropping this stops them.
#[derive(Debug)]
pub struct EventPushSubscriptions {
    _command_finished:
        crate::events::bus::Subscription<crate::events::bus_messages::SessionCommandFinished>,
    _pair_request: crate::events::bus::Subscription<crate::events::bus_messages::PairRequestDelta>,
    _terminal_notification:
        crate::events::bus::Subscription<crate::events::bus_messages::SessionTerminalSignals>,
}
