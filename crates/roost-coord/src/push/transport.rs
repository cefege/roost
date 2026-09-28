//! The one call Web Push delivery makes, as the sender sees it, and the
//! production transport that makes it.
//!
//! Owned by the push domain. v2's `PushNotificationTransport` is
//! `Pick<typeof webpush, "sendNotification">` and the tests substitute a fake;
//! here that substitution is a trait, which is the same seam with a name a
//! reader can grep for. [`WebPushTransport`] is the production half, ported
//! from `ensureConfigured` and the `webpush.sendNotification` call in
//! `push-sender.ts:22-37,99-112`; `serve.rs` builds it once at boot and hands
//! it to `PushTransitions`.
//!
//! WHY THE ERROR CARRIES A STATUS AND NOT AN ENUM. `push-sender.ts:52-61`
//! reaches for `statusCode` on whatever the transport threw, because the
//! decision that follows is a status comparison and nothing else. An enum would
//! move that comparison to the transport and make every new status a change in
//! two files.
//!
//! THE STATUS IS THE ONE THE PUSH SERVICE ANSWERED, NOT A GUESS. `web-push`
//! encrypts (RFC 8291) and signs (RFC 8292) the message, and `reqwest` sends
//! it, so the status a 404/410 prune decision reads is the response line
//! itself. `web-push`'s bundled client folds most statuses into an error enum
//! whose code can come from the response BODY, which would let a provider's
//! JSON decide that a subscription is dead.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use web_push::{
    ContentEncoding, SubscriptionInfo, VapidSignatureBuilder, WebPushMessage, WebPushMessageBuilder,
};

use crate::db::CoordDb;
use crate::push::vapid::{VapidKeyStore, VapidKeys};

/// The VAPID `sub` claim: who a push service contacts about this sender.
///
/// `mailto:roost@local` (`push-sender.ts:27`). A self-hosted coordinator has no
/// operator address to publish, and the claim is required, so this is the same
/// placeholder v2 signs with.
pub const VAPID_SUBJECT: &str = "mailto:roost@local";

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
    ///
    /// The two lifetimes are SEPARATE on purpose. Sharing one `'a` says the
    /// request must live exactly as long as the transport borrow, and every
    /// caller that holds a request from a different scope -- a row read from a
    /// pool, an owned request built from it -- stops compiling, with the
    /// complaint that an `Fn` closure is "not general enough". The returned
    /// future is tied to `&self` because that is what a transport may borrow
    /// (its client, its pool); the request is read during the call and need not
    /// outlive it.
    fn send<'a>(
        &'a self,
        request: &PushDeliveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushTransportError>> + Send + 'a>>;
}

/// The production transport: one encrypted, VAPID-signed POST per delivery.
///
/// Holds the coordinator's [`VapidKeyStore`] rather than a keypair, so the
/// identity is the one `PushGetConfig` handed the browser and is loaded (or
/// minted) on first use exactly as v2's `ensureConfigured` did. A failure to
/// load it fails that one delivery, and the next one tries again, as v2 reset
/// its configure promise (`push-sender.ts:31-36`).
#[derive(Debug, Clone)]
pub struct WebPushTransport {
    client: reqwest::Client,
    database: CoordDb,
    vapid: VapidKeyStore,
}

impl WebPushTransport {
    /// A transport over the coordinator's database and VAPID identity.
    ///
    /// REDIRECTS ARE NEVER FOLLOWED. `web-push` issues one request and rejects
    /// every non-2xx (`push-sender.ts:97-98`); a push service answering 3xx is
    /// a failed delivery, not an instruction to post the payload elsewhere.
    pub fn new(database: CoordDb, vapid: VapidKeyStore) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            database,
            vapid,
        })
    }

    /// Encrypt, sign, POST, and read the status the push service answered.
    async fn deliver(&self, request: PushDeliveryRequest) -> Result<(), PushTransportError> {
        let keys = self.vapid.keys(&self.database).await.map_err(|error| {
            PushTransportError::without_status(format!("vapid identity unavailable: {error}"))
        })?;
        let outbound = {
            let message = encrypted_message(&keys, &request)?;
            let (parts, body) =
                web_push::request_builder::build_request::<Vec<u8>>(message).into_parts();
            let mut outbound = self
                .client
                .post(parts.uri.to_string())
                .timeout(request.timeout)
                .body(body);
            for (name, value) in &parts.headers {
                outbound = outbound.header(name.as_str(), value.as_bytes());
            }
            outbound
        };
        let response = outbound.send().await.map_err(|error| {
            if error.is_timeout() {
                PushTransportError::without_status(format!(
                    "no answer within {}ms",
                    request.timeout.as_millis()
                ))
            } else {
                PushTransportError::without_status(error.to_string())
            }
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(PushTransportError::with_status(
            status.as_u16(),
            format!("received unexpected response code {}", status.as_u16()),
        ))
    }
}

impl PushNotificationTransport for WebPushTransport {
    fn send<'a>(
        &'a self,
        request: &PushDeliveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(), PushTransportError>> + Send + 'a>> {
        // Owned, because the future may borrow only `self` (see the trait).
        Box::pin(self.deliver(request.clone()))
    }
}

/// The RFC 8291 `aes128gcm` message, signed with the coordinator's VAPID key.
///
/// `aes128gcm` is `web-push`'s default in v2 too; the older `aesgcm` exists
/// only for browsers that predate the RFC. A TTL too wide for the header is
/// held at `u32::MAX` rather than wrapped to a short one.
///
/// THE AUDIENCE IS THE ENDPOINT'S ORIGIN, PORT INCLUDED. v2's `web-push` signs
/// `protocol//host` of the WHATWG URL, which keeps a non-default port; the
/// Rust crate's default drops it, and a push service on a non-default port
/// would then refuse every token as addressed to someone else.
fn encrypted_message(
    keys: &VapidKeys,
    request: &PushDeliveryRequest,
) -> Result<WebPushMessage, PushTransportError> {
    let refused = |what: &str, error: web_push::WebPushError| {
        PushTransportError::without_status(format!("{what}: {error}"))
    };
    let subscription = SubscriptionInfo::new(
        request.endpoint.as_str(),
        request.p256dh.as_str(),
        request.auth.as_str(),
    );
    let mut signature = VapidSignatureBuilder::from_base64(&keys.private_key, &subscription)
        .map_err(|error| refused("vapid private key", error))?;
    let audience = reqwest::Url::parse(&request.endpoint)
        .map_err(|error| PushTransportError::without_status(format!("push endpoint: {error}")))?
        .origin()
        .ascii_serialization();
    signature.add_claim("aud", audience);
    signature.add_claim("sub", VAPID_SUBJECT);
    let signature = signature
        .build()
        .map_err(|error| refused("vapid signature", error))?;
    let mut message = WebPushMessageBuilder::new(&subscription);
    message.set_ttl(u32::try_from(request.ttl.as_secs()).unwrap_or(u32::MAX));
    if let Some(topic) = &request.topic {
        message.set_topic(topic.clone());
    }
    message.set_payload(ContentEncoding::Aes128Gcm, request.body.as_bytes());
    message.set_vapid_signature(signature);
    message
        .build()
        .map_err(|error| refused("push message", error))
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
