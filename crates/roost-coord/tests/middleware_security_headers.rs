// The security-header and CORS layer as the mounted stack applies it: the policy
// a response carries, the preflight that is answered before any route, and the
// one origin whose `Access-Control-Allow-Origin` is echoed.
//
// The CSP builder is asserted directly as well, because its output is a string
// an operator reads in a browser console and a rule that stopped being enforced
// would not fail any request.
mod middleware_support;

use middleware_support::{FOREIGN_HOST, FixtureConfig, ListenerFixture, WORKER_LOCAL_UI_ORIGIN};
use roost_coord::middleware::security::build_csp;

/// The retired Connect `Sync`, which the listener answers with `410` before
/// Connect sees it -- a real response on a real path, with no credential needed.
const RETIRED_SYNC: &str = "/roost.v1.CoordinatorService/Sync";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_response_carries_the_policy_this_coordinator_declares() {
    let fixture = ListenerFixture::start("headers", FixtureConfig::default()).await;

    let response = fixture.request("POST", RETIRED_SYNC, &[]);
    assert_eq!(response.status, 410);
    assert_eq!(response.header("x-frame-options"), Some("DENY"));
    assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(response.header("referrer-policy"), Some("no-referrer"));
    assert_eq!(
        response.header("permissions-policy"),
        Some("camera=(), geolocation=(), microphone=(self)")
    );
    let csp = response
        .header("content-security-policy")
        .expect("a content security policy");
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(csp.contains("object-src 'none'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    // The transcription client is a first-party destination, and the worker
    // door this coordinator's own pages dial has to be reachable.
    assert!(csp.contains("connect-src 'self' https://api.deepgram.com wss://api.deepgram.com"));
    assert!(csp.contains(roost_host::DEFAULT_WORKER_LOCAL_UI_ORIGIN));
}

/// HSTS follows the operator trusting a front door, not the scheme of the
/// request, because the coordinator never terminates TLS itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hsts_is_answered_only_when_a_trusted_front_door_terminates_tls() {
    let direct = ListenerFixture::start("hsts-direct", FixtureConfig::default()).await;
    let behind_a_door = ListenerFixture::start(
        "hsts-proxy",
        FixtureConfig {
            trust_proxy: true,
            ..FixtureConfig::default()
        },
    )
    .await;

    let plain = direct.request("POST", RETIRED_SYNC, &[]);
    assert_eq!(plain.header("strict-transport-security"), None);

    let proxied = behind_a_door.request("POST", RETIRED_SYNC, &[]);
    assert_eq!(
        proxied.header("strict-transport-security"),
        Some("max-age=31536000")
    );
}

/// A preflight is answered by the layer, not by a route: `204`, no body, and the
/// same headers every other response carries.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preflight_is_answered_before_any_route_is_tried() {
    let fixture = ListenerFixture::start(
        "preflight",
        FixtureConfig {
            cors_allowed_origins: vec!["https://desk.example.com".to_owned()],
            ..FixtureConfig::default()
        },
    )
    .await;

    let response = fixture.request(
        "OPTIONS",
        "/roost.v1.CoordinatorService/PairPoll",
        &[
            ("Origin", "https://desk.example.com"),
            ("Access-Control-Request-Method", "POST"),
        ],
    );
    assert_eq!(response.status, 204);
    assert!(response.body.is_empty(), "a preflight has no body");
    assert_eq!(
        response.header("access-control-allow-origin"),
        Some("https://desk.example.com")
    );
    assert_eq!(response.header("access-control-allow-methods"), Some("*"));
    assert_eq!(response.header("access-control-allow-headers"), Some("*"));
    assert_eq!(response.header("x-frame-options"), Some("DENY"));
}

/// The worker's loopback door is allowed on every coordinator without being
/// configured, and an origin nobody declared gets no `Access-Control-Allow-
/// Origin` -- with `Vary` on both, so a cache cannot hand one origin a decision
/// made for another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_origin_is_echoed_only_when_it_is_on_the_allowlist() {
    let fixture = ListenerFixture::start("cors", FixtureConfig::default()).await;

    let allowed = fixture.request(
        "POST",
        RETIRED_SYNC,
        &[("Origin", WORKER_LOCAL_UI_ORIGIN)],
    );
    assert_eq!(
        allowed.header("access-control-allow-origin"),
        Some(WORKER_LOCAL_UI_ORIGIN)
    );
    assert_eq!(
        allowed.header("access-control-expose-headers"),
        Some("x-roost-auth-layer")
    );

    let foreign = fixture.request("POST", RETIRED_SYNC, &[("Origin", "https://desk.example.com")]);
    assert_eq!(foreign.header("access-control-allow-origin"), None);
    assert_eq!(
        foreign.header("vary"),
        Some("origin, access-control-request-method, access-control-request-headers")
    );
}

/// The CSP's `connect-src` is the set of things a served page may open, and the
/// relaxed policy is the operator opting into plaintext endpoints rather than
/// the coordinator choosing it.
#[test]
fn a_relaxed_policy_adds_plaintext_endpoints_and_nothing_else_changes() {
    let origins = vec!["http://127.0.0.1:4114".to_owned()];
    let strict = build_csp(false, &origins);
    let relaxed = build_csp(true, &origins);

    // The worker's door is plaintext, so its WebSocket twin has to be named or
    // the page cannot open the terminal socket at all.
    assert!(strict.contains("connect-src 'self' http://127.0.0.1:4114 ws://127.0.0.1:4114"));
    assert!(!strict.contains("http:"), "{strict}");
    assert!(relaxed.contains("connect-src 'self' http://127.0.0.1:4114 ws://127.0.0.1:4114 http: ws:"));
    assert_eq!(
        strict.replace("connect-src 'self' http://127.0.0.1:4114 ws://127.0.0.1:4114", "X"),
        relaxed.replace("connect-src 'self' http://127.0.0.1:4114 ws://127.0.0.1:4114 http: ws:", "X"),
        "relaxing connect-src must change nothing else"
    );
    // A repeated origin is listed once, so a front door that is both declared
    // and allowlisted cannot widen the policy by being written twice.
    let repeated = build_csp(
        false,
        &[
            "https://desk.example.com".to_owned(),
            "https://desk.example.com".to_owned(),
        ],
    );
    assert_eq!(repeated.matches("https://desk.example.com").count(), 1);
}

/// A request the gate refuses never becomes a preflight answer either: the
/// browser's question is about the real request, and answering it for a request
/// the coordinator would refuse is a lie with a status code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preflight_from_an_origin_the_gate_refuses_is_refused_too() {
    let fixture = ListenerFixture::start(
        "preflight-refused",
        FixtureConfig {
            cors_allowed_origins: vec!["https://desk.example.com".to_owned()],
            ..FixtureConfig::default()
        },
    )
    .await;

    let response = fixture.request(
        "OPTIONS",
        "/roost.v1.CoordinatorService/PairPoll",
        &[("Host", FOREIGN_HOST), ("Origin", "https://desk.example.com")],
    );
    assert_eq!(response.status, 403);
    assert_eq!(response.body, "forbidden host");
}
