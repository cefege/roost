//! The production Web Push transport (`push/transport.rs` `WebPushTransport`)
//! against a push service on a loopback socket: what one delivery puts on the
//! wire, and how each answer becomes the status the sender prunes on.
//!
//! Ports the transport half of `apps/coord/tests/push/push-delivery-sender.test.ts`
//! ("prunes 404 and 410 subscriptions without exposing endpoints"; "bounds
//! sends to four, applies the timeout, and never retries redirects"). v2
//! substituted `sendNotification`; this drives the real encryption, VAPID
//! signature and HTTP request, so the headers, the timeout and the redirect
//! refusal are observed at a socket rather than asserted of a double.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod push_fixture;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Response, Uri};
use push_fixture::PushFixture;
use roost_coord::push::transport::{
    PushDeliveryRequest, PushNotificationTransport, PushTransportError, VAPID_SUBJECT,
    WebPushTransport,
};
use roost_coord::push::vapid::{P256KeypairGenerator, VapidKeyGenerator};
use serde_json::Value;

const PLAINTEXT: &str = r#"{"kind":"blocked"}"#;
const TOPIC: &str = "abcdefghijklmnopqrstuvwxyz012345";

/// How the loopback push service answers every request.
#[derive(Clone)]
struct Script {
    status: u16,
    body: &'static str,
    location: Option<&'static str>,
    delay: Duration,
}

impl Script {
    fn answering(status: u16) -> Self {
        Self {
            status,
            body: "",
            location: None,
            delay: Duration::ZERO,
        }
    }
}

/// One request the push service received.
struct Seen {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Clone)]
struct PushService {
    script: Script,
    seen: Arc<Mutex<Vec<Seen>>>,
}

async fn answer(
    State(service): State<PushService>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    service.seen.lock().expect("the request log").push(Seen {
        path: uri.path().to_owned(),
        headers,
        body: body.to_vec(),
    });
    tokio::time::sleep(service.script.delay).await;
    let mut response = Response::builder().status(service.script.status);
    if let Some(location) = service.script.location {
        response = response.header("location", location);
    }
    response
        .body(Body::from(service.script.body))
        .expect("a scripted response")
}

/// A push service on a loopback port; its origin and its request log.
async fn push_service(script: Script) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let origin = format!("http://{}", listener.local_addr().expect("an address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = axum::Router::new()
        .fallback(answer)
        .with_state(PushService {
            script,
            seen: Arc::clone(&seen),
        });
    tokio::spawn(async move { axum::serve(listener, app).await.expect("the push service") });
    (origin, seen)
}

/// A delivery to `origin` for a browser subscription with real keys.
fn request(origin: &str, timeout: Duration) -> PushDeliveryRequest {
    PushDeliveryRequest {
        endpoint: format!("{origin}/wpush/device"),
        // A browser's `p256dh` is an uncompressed P-256 point, which is exactly
        // what a VAPID public key is, so the generator mints a valid one.
        p256dh: P256KeypairGenerator.generate().expect("a point").public_key,
        auth: roost_host::b64url_encode(&[7_u8; 16]),
        body: PLAINTEXT.to_owned(),
        ttl: Duration::from_secs(60),
        topic: Some(TOPIC.to_owned()),
        timeout,
    }
}

async fn deliver(
    fixture: &PushFixture,
    request: &PushDeliveryRequest,
) -> Result<(), PushTransportError> {
    let transport = WebPushTransport::new(
        fixture.database().clone(),
        fixture.push.vapid_keys().clone(),
    )
    .expect("an HTTP client");
    transport.send(request).await
}

fn header<'a>(seen: &'a Seen, name: &str) -> &'a str {
    seen.headers
        .get(name)
        .unwrap_or_else(|| panic!("the {name} header"))
        .to_str()
        .expect("an ASCII header")
}

#[tokio::test]
async fn a_delivery_is_one_encrypted_post_signed_with_the_stored_vapid_identity() {
    let fixture = PushFixture::new("web-push-wire").await;
    let (origin, seen) = push_service(Script::answering(201)).await;

    deliver(&fixture, &request(&origin, Duration::from_secs(10)))
        .await
        .expect("a 201 is a delivery");

    let seen = std::mem::take(&mut *seen.lock().expect("the request log"));
    assert_eq!(seen.len(), 1);
    let post = &seen[0];
    assert_eq!(post.path, "/wpush/device");
    assert_eq!(header(post, "ttl"), "60");
    assert_eq!(header(post, "topic"), TOPIC);
    assert_eq!(header(post, "content-encoding"), "aes128gcm");
    assert!(!post.body.is_empty());
    assert!(
        !String::from_utf8_lossy(&post.body).contains(PLAINTEXT),
        "the payload leaves the coordinator encrypted"
    );

    // `vapid t=<jwt>, k=<public key>`: the key is the one `PushGetConfig` gave
    // the browser, and the token's claims name this push service and v2's
    // subject.
    let keys = fixture
        .push
        .keys(fixture.database())
        .await
        .expect("the stored identity");
    let authorization = header(post, "authorization");
    let (token, key) = authorization
        .strip_prefix("vapid t=")
        .and_then(|rest| rest.split_once(", k="))
        .expect("a VAPID authorization");
    assert_eq!(key, keys.public_key);
    let claims: Value = serde_json::from_str(
        &roost_host::b64url_decode_to_utf8(token.split('.').nth(1).expect("a JWT payload"))
            .expect("base64url claims"),
    )
    .expect("JSON claims");
    assert_eq!(claims["sub"], VAPID_SUBJECT);
    assert_eq!(claims["sub"], "mailto:roost@local");
    assert_eq!(claims["aud"], origin);
}

#[tokio::test]
async fn the_status_is_the_one_the_push_service_answered() {
    let fixture = PushFixture::new("web-push-status").await;
    for (script, status, dead) in [
        (Script::answering(410), 410, true),
        (Script::answering(404), 404, true),
        (Script::answering(429), 429, false),
        // A body claiming a dead subscription does not make a 500 one.
        (
            Script {
                body: r#"{"code":410,"errno":999,"error":"Gone","message":"gone"}"#,
                ..Script::answering(500)
            },
            500,
            false,
        ),
    ] {
        let (origin, _seen) = push_service(script).await;
        let error = deliver(&fixture, &request(&origin, Duration::from_secs(10)))
            .await
            .expect_err("a non-2xx answer is a failed delivery");
        assert_eq!(error.status, Some(status));
        assert_eq!(error.is_dead_subscription(), dead, "{status}");
    }
}

#[tokio::test]
async fn a_redirect_is_a_failed_delivery_never_a_second_post() {
    let fixture = PushFixture::new("web-push-redirect").await;
    let (origin, seen) = push_service(Script {
        location: Some("/elsewhere"),
        ..Script::answering(302)
    })
    .await;

    let error = deliver(&fixture, &request(&origin, Duration::from_secs(10)))
        .await
        .expect_err("a 302 is not a delivery");

    assert_eq!(error.status, Some(302));
    assert!(!error.is_dead_subscription());
    assert_eq!(
        seen.lock().expect("the request log").len(),
        1,
        "the payload is posted once, to the subscribed endpoint only"
    );
}

#[tokio::test]
async fn a_push_service_that_does_not_answer_is_abandoned_at_the_attempt_ceiling() {
    let fixture = PushFixture::new("web-push-timeout").await;
    let (origin, _seen) = push_service(Script {
        delay: Duration::from_secs(5),
        ..Script::answering(201)
    })
    .await;

    let started = Instant::now();
    let error = deliver(&fixture, &request(&origin, Duration::from_millis(200)))
        .await
        .expect_err("an unanswered delivery fails");

    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        error.status, None,
        "a timeout has no status, and never prunes"
    );
    assert!(error.reason.contains("200ms"), "{}", error.reason);
}
