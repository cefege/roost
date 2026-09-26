//! Encrypt and deliver one payload to a bounded set of stored subscriptions.
//!
//! Owned by the push domain. The two rules that live here and nowhere else are
//! the concurrency ceiling and the failure isolation; `push-sender.ts:64-141`
//! decides them, and both are easy to lose in a tidier port.
//!
//! EVERY FAILURE IS ISOLATED. A delivery that fails for any reason other than a
//! dead subscription is counted and the batch continues
//! (`push-sender.ts:130-134`). One push provider having a bad afternoon must not
//! cost a fleet every other notification, and must not unsubscribe every device
//! either -- which is why only 404 and 410 prune.
//!
//! NO REDIRECT REPLAY. `web-push` issues one request and rejects every non-2xx,
//! and so does this: a 3xx is a failure, counted, and never retried
//! (`push-sender.ts:97-98`).

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use sha2::Digest;
use sqlx::SqlitePool;

use crate::push::subscription_store::StoredSubscription;
use crate::push::transport::{
    MAX_CONCURRENT_SENDS, PushDeliveryRequest, PushNotificationTransport, PushTransportError,
    REQUEST_TIMEOUT, TTL_SECONDS,
};

/// What one dispatch did, in the three numbers the log line reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PushDeliveryResult {
    /// Subscriptions the service accepted.
    pub delivered: usize,
    /// Subscriptions pruned because the service said 404 or 410.
    pub expired: usize,
    /// Subscriptions whose delivery failed for any other reason.
    pub failed: usize,
}

/// The optional fences one dispatch runs under.
#[derive(Clone, Default)]
pub struct PushDeliveryOptions<'a> {
    /// The RFC 8030 topic: a later delivery with the same token replaces an
    /// undelivered one instead of stacking a second notification behind it.
    pub deduplication_token: Option<&'a str>,
    /// Whether the transition that triggered this dispatch is still the current
    /// one.
    ///
    /// Checked before the batch and again before each individual send, so a
    /// transition superseded while the fleet's first four deliveries were in
    /// flight does not go on to notify the other sixteen. That is
    /// `push-dispatch.ts:132-141` reaching into the sender, and it is the
    /// difference between one notification and two for one event.
    ///
    /// An owned `Arc` rather than a borrowed `&dyn Fn` so a caller can hand
    /// the same fence to a spawned task; a borrowed trait object is not
    /// `Send`-able past the frame that owns it, and the dispatch runs inside
    /// one.
    pub is_current: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

/// Written by hand because the fence is a closure: a derived rendering would
/// have to print it, and what a reader needs is whether one is installed, not
/// what it captures.
impl std::fmt::Debug for PushDeliveryOptions<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushDeliveryOptions")
            .field("deduplication_token", &self.deduplication_token)
            .field("fenced", &self.is_current.is_some())
            .finish()
    }
}

impl PushDeliveryOptions<'_> {
    /// Whether the transition is still current. Absent means yes.
    fn current(&self) -> bool {
        self.is_current
            .as_ref()
            .is_none_or(|is_current| is_current())
    }
}

/// Deliver `payload` to every subscription in `subscriptions`, at most
/// [`MAX_CONCURRENT_SENDS`] at a time.
/// The pool is v2's shape (`push-sender.ts:87-139`): a fixed number of workers
/// each taking the next index, so the ceiling holds however many subscriptions
/// there are. `buffer_unordered` is what that shape is in Rust -- a stream that
/// runs at most N futures concurrently and yields each as it settles, which is
/// also why the results are summed rather than collected in order.
pub async fn send_push_to_subscriptions(
    pool: &SqlitePool,
    subscriptions: &[StoredSubscription],
    payload: &str,
    options: PushDeliveryOptions<'_>,
    transport: &dyn PushNotificationTransport,
) -> PushDeliveryResult {
    if subscriptions.is_empty() || !options.current() {
        return PushDeliveryResult::default();
    }
    // The options are cloned per attempt rather than captured by reference: the
    // stream's closure is `FnMut`, and it must be able to hand each attempt its
    // own copy of the fence. Cloning an `Arc` is a refcount bump, and the
    // token slice is a `Copy` pointer.
    let attempts = subscriptions
        .iter()
        .map(|subscription| deliver_one(pool, subscription, payload, options.clone(), transport));
    futures_util::stream::iter(attempts)
        .buffer_unordered(MAX_CONCURRENT_SENDS)
        .fold(PushDeliveryResult::default(), |total, outcome| async move {
            total.plus(outcome)
        })
        .await
}

/// Deliver to one subscription: deliver it, prune it, or record the failure.
///
/// The `is_current` re-check is here rather than in the stream because this is
/// the only place a single send is about to leave the process: a superseded
/// transition must not reach the network at all.
async fn deliver_one(
    pool: &SqlitePool,
    subscription: &StoredSubscription,
    payload: &str,
    options: PushDeliveryOptions<'_>,
    transport: &dyn PushNotificationTransport,
) -> PushDeliveryResult {
    if !options.current() {
        return PushDeliveryResult::default();
    }
    let request = PushDeliveryRequest {
        endpoint: subscription.endpoint.clone(),
        p256dh: subscription.p256dh.clone(),
        auth: subscription.auth.clone(),
        body: payload.to_owned(),
        ttl: Duration::from_secs(u64::from(TTL_SECONDS)),
        topic: options.deduplication_token.map(str::to_owned),
        timeout: REQUEST_TIMEOUT,
    };

    match transport.send(&request).await {
        Ok(()) => PushDeliveryResult {
            delivered: 1,
            ..PushDeliveryResult::default()
        },
        Err(error) if error.is_dead_subscription() => {
            prune_dead_subscription(pool, subscription, &error).await;
            PushDeliveryResult {
                expired: 1,
                ..PushDeliveryResult::default()
            }
        }
        Err(error) => {
            log_send_failure(subscription, &error);
            PushDeliveryResult {
                failed: 1,
                ..PushDeliveryResult::default()
            }
        }
    }
}

/// Delete a subscription the push service reported as gone.
///
/// The endpoint is named by a short digest and never in full: an endpoint is a
/// bearer-ish credential, and a log line is the wrong place for one
/// (`push-sender.ts:18-20` does the same).
async fn prune_dead_subscription(
    pool: &SqlitePool,
    subscription: &StoredSubscription,
    error: &PushTransportError,
) {
    let pruned =
        sqlx::query("DELETE FROM push_subscriptions WHERE viewer_fp = ?1 AND endpoint = ?2")
            .bind(&subscription.viewer_fp)
            .bind(&subscription.endpoint)
            .execute(pool)
            .await;
    match pruned {
        Ok(_) => roost_observability::log::info(
            "push",
            "subscription_expired",
            roost_observability::LogFields::new()
                .set("endpoint_id", endpoint_id(&subscription.endpoint))
                .set("status", error.status.unwrap_or_default()),
        ),
        Err(delete_error) => roost_observability::log::warn(
            "push",
            "prune_failed",
            roost_observability::LogFields::new()
                .set("endpoint_id", endpoint_id(&subscription.endpoint))
                .set("error", delete_error.to_string()),
        ),
    }
}

/// Record a delivery failure without pruning and without failing the batch.
fn log_send_failure(subscription: &StoredSubscription, error: &PushTransportError) {
    roost_observability::log::warn(
        "push",
        "send_failed",
        roost_observability::LogFields::new()
            .set("endpoint_id", endpoint_id(&subscription.endpoint))
            .set("status", error.status)
            .set("error", &error.reason),
    );
}

/// Twelve hex characters of the endpoint's SHA-256, for a log line.
///
/// v2 takes the first twelve of the hex digest (`push-sender.ts:18-20`), which
/// is long enough to correlate one endpoint's failures across a log and short
/// enough that the line cannot be used to reconstruct the endpoint.
#[must_use]
pub fn endpoint_id(endpoint: &str) -> String {
    let digest = sha2::Sha256::digest(endpoint.as_bytes());
    hex::encode(digest)[..12].to_owned()
}

impl PushDeliveryResult {
    /// The sum of two partial results, for folding stream outcomes together.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            delivered: self.delivered + other.delivered,
            expired: self.expired + other.expired,
            failed: self.failed + other.failed,
        }
    }
}
