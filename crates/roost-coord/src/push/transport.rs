//! The one call Web Push delivery makes, as the sender sees it.
//!
//! Owned by the push domain. v2's `PushNotificationTransport` is
//! `Pick<typeof webpush, "sendNotification">` and the tests substitute a fake;
//! here that substitution is a trait, which is the same seam with a name a
//! reader can grep for.
//!
//! WHY THE ERROR CARRIES A STATUS AND NOT AN ENUM. `push-sender.ts:52-61`
//! reaches for `statusCode` on whatever the transport threw, because the
//! decision that follows is a status comparison and nothing else. An enum would
//! move that comparison to the transport and make every new status a change in
//! two files.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// The `web-push` `WebPushClient` this trait's production half delegates to.
pub use web_push::WebPushClient;

/// How long a push service may hold a message for an offline device.
///
/// 60 seconds (`push-sender.ts:12`). Long enough for a phone on a slow link to
/// surface it, short enough that an agent that finished ten minutes ago does
/// not wake a device to say so.
pub const TTL_SECONDS: u32 = 60;

/// How long one delivery attempt may take before it is abandoned.
///
/// 10 seconds (`push-sender.ts:13`). This is the per-attempt ceiling inside
/// [`MAX_CONCURRENT_SENDS`], so the whole batch's worst case is
/// `ceil(n / 4) * 10s`, and a hung push service cannot hold the dispatch open
/// for the fleet's whole session list.
pub const REQUEST_TIMEOUT: Duration = Duration::from_millis(10_000);

/// How many deliveries may be in flight at once.
///
/// 4 (`push-sender.ts:14`). A push service rate-limits by endpoint, and a
/// fleet-wide fan-out that opened one connection per subscription would turn a
/// busy transition into a self-inflicted outage. Four is also the width that
/// keeps a 20-subscription fleet inside one timeout window.
pub const MAX_CONCURRENT_SENDS: usize = 4;

/// Why one delivery attempt failed, and what a caller may conclude from it.
///
/// `status` is the HTTP status when the transport saw one. Only 404 and 410
/// mean the subscription is dead; everything else is a failure to record and
/// move past, and treating a 500 or a timeout as "gone" would unsubscribe every
/// device the first time a push provider had a bad afternoon.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("push delivery failed: {reason}")]
pub struct PushTransportError {
    /// The status the push service answered with, when it answered.
    pub status: Option<u16>,
    /// What went wrong, for the log line.
    pub reason: String,
}

impl PushTransportError {
    /// A failure with an HTTP status.
    #[must_use]
    pub fn with_status(status: u16, reason: impl Into<String>) -> Self {
        Self {
            status: Some(status),
            reason: reason.into(),
        }
    }

    /// A failure with no status: a timeout, a DNS failure, a refused socket.
    #[must_use]
    pub fn without_status(reason: impl Into<String>) -> Self {
        Self {
            status: None,
            reason: reason.into(),
        }
    }

    /// Whether the push service said this subscription no longer exists.
    ///
    /// 404 and 410 only. A 410 is the RFC 8030 "Gone" and is the provider
    /// retiring a token; a 404 is the same answer from providers that do not
    /// distinguish it. Nothing else prunes.
    #[must_use]
    pub fn is_dead_subscription(&self) -> bool {
        matches!(self.status, Some(404 | 410))
    }
}

/// One encrypted delivery of one payload to one subscription.
///
/// Options rather than arguments because the three of them are the RFC 8291
/// request's own knobs -- TTL, topic, and the per-attempt ceiling -- and they
/// travel together to every transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDeliveryRequest {
    /// The browser's push endpoint, exactly as it was subscribed.
    pub endpoint: String,
    /// The base64url P-256 public key from `subscribe`.
    pub p256dh: String,
    /// The base64url auth secret from `subscribe`.
    pub auth: String,
    /// The serialized payload.
    pub body: String,
    /// How long the service may hold the message.
    pub ttl: Duration,
    /// The RFC 8030 topic, which is the deduplication token when there is one.
    pub topic: Option<String>,
    /// The ceiling on this one attempt.
    pub timeout: Duration,
}

/// Encrypt and deliver one payload to one subscription.
///
/// `Send + Sync` because the sender holds one transport behind `&dyn` and runs
/// four attempts at a time.
///
/// The method returns a BOXED future rather than being an `async fn`. An
/// `async fn` in a trait desugars to a higher-ranked future over every input
/// lifetime, and the sender holds the transport as `&dyn`, so composing them
/// makes the whole call non-`'static` and unusable from a spawned task. The box
/// is the price of the trait object, paid once per delivery -- and one
/// `web-push` HTTPS round trip costs orders of magnitude more than the
/// allocation.
pub trait PushNotificationTransport: Send + Sync {
    /// Deliver `request`, or say why not.
    fn send<'a>(
        &'a self,
        request: &'a PushDeliveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushTransportError>> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::{PushTransportError, REQUEST_TIMEOUT, TTL_SECONDS};

    #[test]
    fn only_404_and_410_prune_a_subscription() {
        for dead in [404_u16, 410] {
            assert!(PushTransportError::with_status(dead, "gone").is_dead_subscription());
        }
        for alive in [400_u16, 401, 403, 413, 429, 500, 502, 503] {
            assert!(
                !PushTransportError::with_status(alive, "refused").is_dead_subscription(),
                "{alive} must not prune"
            );
        }
        assert!(!PushTransportError::without_status("timed out").is_dead_subscription());
    }

    #[test]
    fn the_ttl_and_the_attempt_ceiling_are_the_ones_v2_sent() {
        assert_eq!(TTL_SECONDS, 60);
        assert_eq!(REQUEST_TIMEOUT.as_millis(), 10_000);
    }
}
