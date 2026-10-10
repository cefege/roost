//! Web Push for a new pairing request: every subscribed device hears that a
//! browser is waiting for approval, so a phone or a closed tab learns of it.
//!
//! Subscribed to `Buses::pair_bus` once at boot by
//! `push::session_push::subscribe_event_pushes`; only `Pending`, which is
//! published once per created request, is pushed. No device is suppressed as
//! "viewing": the approver is exactly who must hear it.

use std::collections::BTreeSet;
use std::sync::Arc;

use roost_observability::LogFields;
use serde::Serialize;
use sha2::Digest as _;
use sqlx::AnyPool;

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::PairRequestDelta;
use crate::push::dispatch::select_targets;
use crate::push::sender::{PushDeliveryOptions, send_push_to_subscriptions};
use crate::push::subscription_store::take_deliverable_subscriptions;
use crate::push::transport::PushNotificationTransport;

/// The `kind` the service worker routes a pairing notification by.
pub const PAIR_REQUEST_PUSH_KIND: &str = "pair_request";

/// What one pairing notification says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairRequestPayload {
    /// Always [`PAIR_REQUEST_PUSH_KIND`].
    pub kind: &'static str,
    /// The request's ephemeral id; the notification's tag.
    #[serde(rename = "ephemeralId")]
    pub ephemeral_id: String,
    /// The notification title.
    pub title: String,
    /// The notification body, naming the requester.
    pub body: String,
    /// The push service's collapse key for this request.
    #[serde(rename = "deduplicationToken")]
    pub deduplication_token: String,
}

/// The notification for request `ephemeral_id` from `requester`.
#[must_use]
pub fn pair_request_payload(ephemeral_id: &str, requester: &str) -> PairRequestPayload {
    let digest = sha2::Sha256::digest(format!("pair:{ephemeral_id}").as_bytes());
    PairRequestPayload {
        kind: PAIR_REQUEST_PUSH_KIND,
        ephemeral_id: ephemeral_id.to_owned(),
        title: "A browser wants to pair".to_owned(),
        body: format!("{requester} is waiting for approval. Open Roost to review."),
        deduplication_token: roost_host::b64url_encode(&digest)[..32].to_owned(),
    }
}

/// Push the pairing notification for `ephemeral_id` to every deliverable
/// subscription on an allowed origin, logging the counts.
pub async fn fire_pair_request_push(
    pool: &AnyPool,
    ephemeral_id: &str,
    requester: &str,
    allowed_origins: &[String],
    transport: &dyn PushNotificationTransport,
) {
    let subscriptions = match take_deliverable_subscriptions(pool).await {
        Ok(subscriptions) if !subscriptions.is_empty() => subscriptions,
        Ok(_) => return,
        Err(error) => {
            log_failed(ephemeral_id, &error.to_string());
            return;
        }
    };
    let (selected, _) = select_targets(&subscriptions, allowed_origins, &BTreeSet::new());
    if selected.is_empty() {
        return;
    }
    let payload = pair_request_payload(ephemeral_id, requester);
    let body = match serde_json::to_string(&payload) {
        Ok(body) => body,
        Err(error) => {
            log_failed(ephemeral_id, &error.to_string());
            return;
        }
    };
    let result = send_push_to_subscriptions(
        pool,
        &selected,
        &body,
        PushDeliveryOptions {
            deduplication_token: Some(payload.deduplication_token),
            is_current: None,
        },
        transport,
    )
    .await;
    roost_observability::log::info(
        "push",
        "pair_request_dispatched",
        LogFields::new()
            .set("ephemeral_id", ephemeral_id)
            .set("targeted", selected.len())
            .set("delivered", result.delivered)
            .set("expired", result.expired)
            .set("failed", result.failed),
    );
}

fn log_failed(ephemeral_id: &str, error: &str) {
    roost_observability::log::warn(
        "push",
        "pair_request_failed",
        LogFields::new()
            .set("ephemeral_id", ephemeral_id)
            .set("error", error),
    );
}

/// The delivery collaborators for the pairing push.
pub struct PairRequestPush {
    pool: AnyPool,
    allowed_origins: Vec<String>,
    transport: Arc<dyn PushNotificationTransport>,
}

impl std::fmt::Debug for PairRequestPush {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PairRequestPush")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

impl PairRequestPush {
    #[must_use]
    pub fn new(
        pool: AnyPool,
        allowed_origins: Vec<String>,
        transport: Arc<dyn PushNotificationTransport>,
    ) -> Self {
        Self {
            pool,
            allowed_origins,
            transport,
        }
    }

    /// Push every pairing request created from now on; dropping the
    /// subscription stops the pushes.
    pub fn subscribe(self: Arc<Self>, buses: &Buses) -> Subscription<PairRequestDelta> {
        buses.pair_bus.subscribe(move |delta| {
            if self.allowed_origins.is_empty() {
                return;
            }
            let PairRequestDelta::Pending {
                ephemeral_id,
                label,
                client_browser,
                client_os,
                city,
                region,
                country_code,
                ..
            } = delta
            else {
                return;
            };
            let requester = roost_protocol::wire::pairing::requester_label(
                label,
                client_browser,
                client_os,
                city,
                region,
                country_code,
            );
            let push = Arc::clone(&self);
            let ephemeral_id = ephemeral_id.clone();
            tokio::spawn(async move {
                fire_pair_request_push(
                    &push.pool,
                    &ephemeral_id,
                    &requester,
                    &push.allowed_origins,
                    push.transport.as_ref(),
                )
                .await;
            });
        })
    }
}
