//! The gates a browser on this machine passes before it reaches the door's
//! answers (v2 `apps/worker/tests/local-door/local-ui-server.test.ts`): Host
//! and Origin refusal, the bootstrap payload, the CORS answers and the
//! security headers. Drives the real listener over loopback HTTP.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod door_support;

use door_support::{COORDINATOR_URL, DoorOptions, WORKER_FP, request, start_door};
use roost_worker::door::LOCAL_BOOTSTRAP_PATH;

/// DNS rebinding: a page on another name that resolves to 127.0.0.1 still
/// sends its own Host, and is refused before any route runs.
#[tokio::test]
async fn a_host_this_door_does_not_answer_on_is_refused_before_routing() {
    let door = start_door(DoorOptions::default()).await;

    let rebound = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("host", "evil.example")],
    )
    .await;
    let rebound_page = request(&door, "GET", "/", &[("host", "evil.example")]).await;

    assert_eq!(rebound.status, 403);
    assert!(rebound.body.is_empty());
    assert_eq!(rebound_page.status, 403);
    assert!(rebound_page.body.is_empty());
}

#[tokio::test]
async fn a_cross_origin_caller_is_refused_on_every_route() {
    let door = start_door(DoorOptions::default()).await;

    for path in ["/", LOCAL_BOOTSTRAP_PATH] {
        let answer = request(&door, "GET", path, &[("origin", "https://attacker.test")]).await;
        assert_eq!(answer.status, 403, "{path}");
        assert!(answer.body.is_empty(), "{path}");
    }
}

/// Each name that reaches a loopback listener is served, so a user who types
/// `localhost:<port>` gets the page. Driven as a header, never through DNS.
#[tokio::test]
async fn every_loopback_authority_is_served_with_or_without_an_origin() {
    let door = start_door(DoorOptions::default()).await;
    let port = door.address().port();
    let localhost = format!("localhost:{port}");
    let localhost_origin = format!("http://localhost:{port}");
    let own_origin = door.origin();

    let same_origin = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("origin", own_origin.as_str())],
    )
    .await;
    let no_origin = request(&door, "GET", LOCAL_BOOTSTRAP_PATH, &[]).await;
    let via_localhost = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("host", localhost.as_str())],
    )
    .await;
    let via_localhost_origin = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[
            ("host", localhost.as_str()),
            ("origin", localhost_origin.as_str()),
        ],
    )
    .await;

    for answer in [same_origin, no_origin, via_localhost, via_localhost_origin] {
        assert_eq!(answer.status, 200, "{answer:?}");
    }
}

#[tokio::test]
async fn bootstrap_advertises_exactly_the_coordinator_and_fingerprint_uncached() {
    let door = start_door(DoorOptions::default()).await;

    let answer = request(&door, "GET", LOCAL_BOOTSTRAP_PATH, &[]).await;

    assert_eq!(answer.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&answer.body).expect("a JSON body");
    assert_eq!(
        body,
        serde_json::json!({ "coordinatorUrl": COORDINATOR_URL, "workerFingerprint": WORKER_FP })
    );
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    assert_eq!(answer.header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn responses_name_only_this_door_and_its_coordinator_in_connect_src() {
    let plaintext = start_door(DoorOptions::default()).await;
    let tunnelled = start_door(DoorOptions {
        coordinator_url: "https://coord.example.test",
        ..DoorOptions::default()
    })
    .await;

    let over_loopback = request(&plaintext, "GET", LOCAL_BOOTSTRAP_PATH, &[]).await;
    let over_tls = request(&tunnelled, "GET", LOCAL_BOOTSTRAP_PATH, &[]).await;
    let refused = request(
        &plaintext,
        "GET",
        "/",
        &[("origin", "https://attacker.test")],
    )
    .await;

    let csp = |answer: &door_support::Answer| {
        answer
            .header("content-security-policy")
            .unwrap_or_default()
            .to_owned()
    };
    assert!(
        csp(&over_loopback)
            .contains("connect-src 'self' http://coord.test:4102 ws://coord.test:4102;")
    );
    assert!(
        csp(&over_tls)
            .contains("connect-src 'self' https://coord.example.test wss://coord.example.test;")
    );
    assert_eq!(over_loopback.header("x-frame-options"), Some("DENY"));
    assert_eq!(
        refused.header("x-frame-options"),
        Some("DENY"),
        "a refusal is secured too"
    );
}

#[tokio::test]
async fn the_coordinators_own_origin_is_admitted_and_answered_with_cors() {
    let door = start_door(DoorOptions::default()).await;

    let answer = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("origin", COORDINATOR_URL)],
    )
    .await;

    assert_eq!(answer.status, 200);
    let body: serde_json::Value = serde_json::from_slice(&answer.body).expect("a JSON body");
    assert_eq!(body["coordinatorUrl"], COORDINATOR_URL);
    assert_eq!(
        answer.header("access-control-allow-origin"),
        Some(COORDINATOR_URL)
    );
    assert!(answer.header("vary").unwrap_or_default().contains("origin"));
}

#[tokio::test]
async fn a_local_network_preflight_from_the_coordinators_origin_is_answered() {
    let door = start_door(DoorOptions::default()).await;

    let answer = request(
        &door,
        "OPTIONS",
        LOCAL_BOOTSTRAP_PATH,
        &[("origin", COORDINATOR_URL)],
    )
    .await;
    let written = request(&door, "POST", LOCAL_BOOTSTRAP_PATH, &[]).await;

    assert_eq!(answer.status, 204);
    assert_eq!(
        answer.header("access-control-allow-origin"),
        Some(COORDINATOR_URL)
    );
    assert_eq!(answer.header("access-control-allow-methods"), Some("GET"));
    assert_eq!(
        answer.header("access-control-allow-private-network"),
        Some("true")
    );
    assert_eq!(written.status, 405);
}

#[tokio::test]
async fn a_configured_extra_origin_is_admitted_and_nothing_else_is() {
    let door = start_door(DoorOptions {
        allowed_browser_origins: &["https://dash.example"],
        ..DoorOptions::default()
    })
    .await;

    let admitted = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("origin", "https://dash.example")],
    )
    .await;
    let refused = request(
        &door,
        "GET",
        LOCAL_BOOTSTRAP_PATH,
        &[("origin", "https://other.example")],
    )
    .await;

    assert_eq!(admitted.status, 200);
    assert_eq!(
        admitted.header("access-control-allow-origin"),
        Some("https://dash.example")
    );
    assert_eq!(refused.status, 403);
    assert!(refused.body.is_empty());
}
